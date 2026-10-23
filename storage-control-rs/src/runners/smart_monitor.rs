//! Nightly SMART monitor, ported from `runners/smart_monitor.py`.
//!
//! Alerting rules (kept verbatim — they encode the operator's decisions):
//!   * an alert is raised only when a counter grew past the last resolution,
//!     or when there is no baseline yet and the disk is unhealthy / has
//!     non-zero counters;
//!   * with a baseline in place, a pre-existing condition does not re-alert;
//!   * a health failure or any increase makes the alert `critical`;
//!   * every run first resolves the device's previous active alerts;
//!   * the baseline is what the operator accepted by pressing "Resolve".

use serde::Deserialize;
use serde_json::{json, Value};

use crate::collect::smart;
use crate::db::repo::smart as repo;
use crate::db::{now_ms, Db};
use crate::error::AppError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Auto,
    Manual,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Source::Auto => "auto",
            Source::Manual => "manual",
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Baseline {
    #[serde(default)]
    reallocated: i64,
    #[serde(default)]
    pending: i64,
    #[serde(default)]
    offline: i64,
    #[serde(default)]
    ata_errors: i64,
}

fn parse_baseline(raw: Option<&str>) -> Option<Baseline> {
    let raw = raw?;
    serde_json::from_str(raw).ok()
}

fn counters_json(reallocated: i64, pending: i64, offline: i64, ata_errors: i64) -> String {
    json!({
        "reallocated": reallocated,
        "pending": pending,
        "offline": offline,
        "ata_errors": ata_errors,
    })
    .to_string()
}

fn compare(current: i64, baseline: Option<i64>, label: &str) -> (bool, String) {
    match baseline {
        None => (false, format!("{label}: {current}")),
        Some(base) if current > base => (true, format!("{label} increased: {base} -> {current}")),
        Some(base) => (false, format!("{label}: {current} (baseline: {base})")),
    }
}

/// Run one full check. Auto mode also rewrites alerts; manual mode only stores
/// samples and reports what changed (it must not clear a real alarm).
pub fn run_check(db: &Db, source: Source) -> Result<Vec<Value>, AppError> {
    let now = now_ms();
    let mut results = Vec::new();

    for device in smart::disk_devices() {
        let output = smart::run_smartctl(&device);
        let text = output.combined();
        let info = smart::parse(&device, &text);
        let ata_errors = smart::ata_error_count(&text);

        let reallocated = info.attr("Reallocated_Sector_Ct");
        let pending = info.attr("Current_Pending_Sector");
        let offline = info.attr("Offline_Uncorrectable");
        let health = info.health.clone();
        let temperature = info.temperature;

        let previous = repo::find_state(db, &device)?;
        let baseline = parse_baseline(previous.as_ref().and_then(|s| s.resolved_counters.as_deref()));

        let mut metrics = Vec::new();
        let mut increased = Vec::new();
        let counters = [
            ("Reallocated sectors", reallocated, "reallocated"),
            ("Current pending sectors", pending, "pending"),
            ("Offline uncorrectable sectors", offline, "offline"),
            ("ATA errors", ata_errors, "ata_errors"),
        ];
        for (label, current, key) in counters {
            let base = baseline.as_ref().map(|b| match key {
                "reallocated" => b.reallocated,
                "pending" => b.pending,
                "offline" => b.offline,
                _ => b.ata_errors,
            });
            let (grew, message) = compare(current, base, label);
            metrics.push(message);
            if grew {
                increased.push(label);
            }
        }

        let health_failed = !matches!(health.as_str(), "PASSED" | "PASSED!") || output.rc != 0;
        let mut issues = Vec::new();
        if health_failed {
            issues.push(format!(
                "SMART self-assessment: {health} (smartctl rc={})",
                output.rc
            ));
        }

        let mut temperature_issue: Option<String> = None;
        if let Some(temp) = temperature {
            if temp >= 60 {
                temperature_issue = Some(format!("Temperature critical: {temp} C"));
            } else if temp >= 50 {
                temperature_issue = Some(format!("Temperature high: {temp} C"));
            }
            if let Some(issue) = &temperature_issue {
                issues.push(issue.clone());
            }
        }

        let any_counter = reallocated > 0 || pending > 0 || offline > 0 || ata_errors > 0;
        if health_failed || any_counter {
            issues.extend(metrics.iter().cloned());
            if let Some(temp) = temperature {
                if temperature_issue.is_none() {
                    issues.push(format!("Temperature: {temp} C"));
                }
            }
        }

        let should_alert = if !increased.is_empty() {
            true
        } else if baseline.is_none() {
            health_failed || any_counter
        } else {
            false
        };

        if source == Source::Auto {
            repo::resolve_alerts_for_device(db, &device, now)?;
            if should_alert && !issues.is_empty() {
                let severity = if health_failed || metrics.iter().any(|m| m.contains("increased")) {
                    "critical"
                } else {
                    "warning"
                };
                repo::insert_alert(
                    db,
                    &device,
                    severity,
                    &issues.join("; "),
                    &counters_json(reallocated, pending, offline, ata_errors),
                    now,
                )?;
            }
        }

        repo::insert_sample(
            db,
            &repo::SampleInsert {
                device: &device,
                reallocated,
                pending,
                offline_uncorrectable: offline,
                ata_error_count: ata_errors,
                temperature,
                health: &health,
                smartctl_rc: output.rc as i64,
                source: source.as_str(),
                now,
            },
        )?;

        repo::upsert_state(
            db,
            &repo::StateUpsert {
                device: &device,
                model: info.model.as_deref(),
                serial: info.serial.as_deref(),
                health: &health,
                reallocated,
                pending,
                offline_uncorrectable: offline,
                ata_error_count: ata_errors,
                temperature,
                power_on_hours: info.power_on_hours,
                smartctl_rc: output.rc as i64,
                now,
            },
        )?;

        results.push(json!({
            "device": device,
            "model": info.model,
            "health": health,
            "temperature": temperature,
            "reallocated": reallocated,
            "pending": pending,
            "offline": offline,
            "ata_errors": ata_errors,
            "increased_since_resolve": increased,
            "baseline": baseline.map(|b| json!({
                "reallocated": b.reallocated,
                "pending": b.pending,
                "offline": b.offline,
                "ata_errors": b.ata_errors,
            })),
        }));
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_parsing_tolerates_missing_keys() {
        let baseline = parse_baseline(Some(r#"{"reallocated": 4}"#)).unwrap();
        assert_eq!(baseline.reallocated, 4);
        assert_eq!(baseline.pending, 0);
        assert!(parse_baseline(Some("not json")).is_none());
        assert!(parse_baseline(None).is_none());
    }

    #[test]
    fn comparison_messages_match_flask() {
        assert_eq!(
            compare(8, Some(4), "Reallocated sectors"),
            (true, "Reallocated sectors increased: 4 -> 8".to_string())
        );
        assert_eq!(
            compare(4, Some(4), "Reallocated sectors"),
            (false, "Reallocated sectors: 4 (baseline: 4)".to_string())
        );
        assert_eq!(
            compare(0, None, "ATA errors"),
            (false, "ATA errors: 0".to_string())
        );
    }
}
