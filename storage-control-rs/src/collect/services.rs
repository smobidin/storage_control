//! systemd service catalog, batched status collection and unit control.
//!
//! Ported from `collectors/services.py` with one deliberate deviation: instead
//! of ~48 `systemctl` invocations per dashboard refresh (one `is-active`, one
//! `is-enabled` and one `show` per unit, plus a timer lookup each), the same
//! data is fetched in six calls. `systemctl show` accepts several units and
//! prints one block per unit, which is what `show_props` parses.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use serde::Serialize;

use crate::exec;

#[derive(Debug, Clone, Serialize)]
pub struct TimerInfo {
    pub scope: String,
    pub unit: String,
    pub raw: String,
}

pub const UNIT_PATTERN: &str = r"^[A-Za-z0-9_.@-]+\.(service|timer|socket|target)$";

#[derive(Debug, Clone, Copy)]
pub struct Entry {
    pub name: &'static str,
    pub scope: &'static str,
    pub group: &'static str,
    pub order: u32,
    pub critical: bool,
    pub desc: &'static str,
}

pub const CATALOG: &[Entry] = &[
    // Immich stack (user)
    Entry { name: "immich-ml.service", scope: "user", group: "immich", order: 10, critical: false, desc: "ML inference" },
    Entry { name: "immich-server.service", scope: "user", group: "immich", order: 20, critical: false, desc: "API + web (port 2283)" },
    Entry { name: "immich-nightfix.service", scope: "user", group: "immich", order: 30, critical: false, desc: "Night thumbnail fix (manual run)" },
    Entry { name: "immich-nightfix.timer", scope: "user", group: "immich", order: 31, critical: false, desc: "Timer 03:00 daily" },
    Entry { name: "backup-immich-nc.timer", scope: "user", group: "backup", order: 10, critical: false, desc: "DB/config backup 04:00" },
    // Stalwart (user)
    Entry { name: "stalwart.service", scope: "user", group: "stalwart", order: 10, critical: false, desc: "Stalwart mail server (port 8082)" },
    // Infra (system)
    Entry { name: "postgresql.service", scope: "system", group: "infra", order: 10, critical: true, desc: "PostgreSQL 18" },
    Entry { name: "redis-server.service", scope: "system", group: "infra", order: 20, critical: true, desc: "Redis (Nextcloud sessions)" },
    Entry { name: "php8.5-fpm.service", scope: "system", group: "nextcloud", order: 30, critical: true, desc: "PHP-FPM Nextcloud pool" },
    Entry { name: "nginx.service", scope: "system", group: "infra", order: 40, critical: true, desc: "Reverse proxy 8080/8081" },
    // Timers (system)
    Entry { name: "certbot-renew.timer", scope: "system", group: "infra", order: 50, critical: false, desc: "LetsEncrypt renew" },
    Entry { name: "mdcheck_start.timer", scope: "system", group: "infra", order: 51, critical: false, desc: "RAID check monthly" },
];

const SHOW_PROPS: [&str; 9] = [
    "Id",
    "ActiveEnterTimestamp",
    "ActiveExitTimestamp",
    "ExecMainStatus",
    "ExecMainCode",
    "UnitFileState",
    "Type",
    "Result",
    "NextEligibleTimestamp",
];

#[derive(Debug, Clone, Serialize)]
pub struct ServiceStatus {
    pub name: &'static str,
    pub scope: &'static str,
    pub group: &'static str,
    pub order: u32,
    pub critical: bool,
    pub desc: &'static str,
    pub active: String,
    pub enabled: String,
    pub display_status: String,
    pub active_since: Option<String>,
    pub active_exit: Option<String>,
    pub last_run: Option<String>,
    pub memory_current: Option<String>,
    pub exec_status: Option<String>,
    pub exec_code: Option<String>,
    pub unit_type: Option<String>,
    pub result: Option<String>,
    pub next_run: Option<String>,
}

pub fn all_status() -> Vec<ServiceStatus> {
    let (user_entries, system_entries): (Vec<&Entry>, Vec<&Entry>) = CATALOG
        .iter()
        .partition(|entry| entry.scope == "user");

    let user = scope_status("user", &user_entries);
    let system = scope_status("system", &system_entries);
    user.into_iter().chain(system).collect()
}

fn scope_status(scope: &str, entries: &[&Entry]) -> Vec<ServiceStatus> {
    let names: Vec<&str> = entries.iter().map(|entry| entry.name).collect();
    let props = show_props(scope, &names, &SHOW_PROPS);

    let active = multi_state(scope, "is-active", &names);
    let enabled = multi_state(scope, "is-enabled", &names);

    // One extra call resolves "when did the timer last fire" for every service
    // in the catalog; nonexistent timers just come back with empty properties.
    let timers: Vec<String> = names
        .iter()
        .filter(|name| name.ends_with(".service"))
        .map(|name| name.replace(".service", ".timer"))
        .collect();
    let timer_names: Vec<&str> = timers.iter().map(String::as_str).collect();
    let timer_props = show_props(scope, &timer_names, &["Id", "LastTriggerUSec"]);

    entries
        .iter()
        .map(|entry| {
            let detail = props.get(entry.name).cloned().unwrap_or_default();
            let active = active.get(entry.name).cloned().unwrap_or_default();
            let enabled = enabled.get(entry.name).cloned().unwrap_or_default();

            let last_trigger = if entry.name.ends_with(".service") {
                let timer = entry.name.replace(".service", ".timer");
                timer_props
                    .get(&timer)
                    .and_then(|props| props.get("LastTriggerUSec"))
                    .map(|value| format_trigger(value))
            } else {
                None
            };

            let unit_type = detail.get("Type").cloned().unwrap_or_default();
            let result = detail.get("Result").cloned().unwrap_or_default();
            let exec_status = detail.get("ExecMainStatus").cloned().unwrap_or_default();

            // A oneshot unit is "inactive" after it completes; report what the
            // run actually did instead of the meaningless inactive state.
            let mut display_status = active.clone();
            if unit_type == "oneshot" && active == "inactive" {
                if result == "success" || exec_status == "0" {
                    display_status = "success".into();
                } else if result == "failure" || (!exec_status.is_empty() && exec_status != "0") {
                    display_status = "failed".into();
                } else if prop(&detail, "ActiveEnterTimestamp").is_none() {
                    display_status = "never-run".into();
                }
            }

            let last_run = prop(&detail, "ActiveExitTimestamp")
                .or_else(|| prop(&detail, "ActiveEnterTimestamp"))
                .or(last_trigger);

            ServiceStatus {
                name: entry.name,
                scope: entry.scope,
                group: entry.group,
                order: entry.order,
                critical: entry.critical,
                desc: entry.desc,
                active,
                enabled,
                display_status,
                active_since: prop(&detail, "ActiveEnterTimestamp"),
                active_exit: prop(&detail, "ActiveExitTimestamp"),
                last_run,
                // MemoryCurrent was never requested by the Flask collector, so
                // it always rendered as null; keep the field for the UI shape.
                memory_current: None,
                exec_status: prop(&detail, "ExecMainStatus"),
                exec_code: prop(&detail, "ExecMainCode"),
                unit_type: prop(&detail, "Type"),
                result: prop(&detail, "Result"),
                next_run: prop(&detail, "NextEligibleTimestamp"),
            }
        })
        .collect()
}

/// `systemctl show unit1 unit2 ... -p A -p B` → map keyed by the `Id=` property.
fn show_props(
    scope: &str,
    units: &[&str],
    properties: &[&str],
) -> HashMap<String, HashMap<String, String>> {
    let mut out = HashMap::new();
    if units.is_empty() {
        return out;
    }
    let args = show_args(scope, units, properties);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = exec::run(&argv, None, None, Duration::from_secs(30));
    for block in result.stdout.split("\n\n") {
        let mut props = HashMap::new();
        for line in block.lines() {
            if let Some((key, value)) = line.split_once('=') {
                props.insert(key.trim().to_string(), value.to_string());
            }
        }
        if let Some(id) = props.get("Id").filter(|id| !id.is_empty()) {
            out.insert(id.clone(), props);
        }
    }
    out
}

/// `systemctl is-active a b c` prints one line per unit, in argument order.
fn multi_state(scope: &str, action: &str, units: &[&str]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if units.is_empty() {
        return out;
    }
    let mut args = ctl_prefix(scope);
    args.push(action.to_string());
    args.extend(units.iter().map(|unit| unit.to_string()));
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = exec::run(&argv, None, None, Duration::from_secs(30));
    for (unit, line) in units.iter().zip(result.stdout.lines()) {
        out.insert((*unit).to_string(), line.trim().to_string());
    }
    out
}

fn show_args(scope: &str, units: &[&str], properties: &[&str]) -> Vec<String> {
    let mut args = ctl_prefix(scope);
    args.push("show".into());
    args.extend(units.iter().map(|unit| unit.to_string()));
    for property in properties {
        args.push("-p".into());
        args.push((*property).to_string());
    }
    args
}

fn ctl_prefix(scope: &str) -> Vec<String> {
    if scope == "user" {
        vec!["systemctl".into(), "--user".into()]
    } else {
        vec!["sudo".into(), "systemctl".into()]
    }
}

/// `ActiveEnterTimestamp=Wed 2026-10-21 03:36:44 MSK` → `Some("...")`,
/// `ActiveEnterTimestamp=` → `None`.
fn prop(detail: &HashMap<String, String>, key: &str) -> Option<String> {
    detail
        .get(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Normalise `Tue 2026-10-20 03:36:44 MSK` to `2026-10-20T03:36:44`.
fn format_trigger(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return value.to_string();
    }
    let re = rx(&RE_TRIGGER, r"(\w{3})\s+(\d{4}-\d{2}-\d{2})\s+(\d{2}:\d{2}:\d{2})\s+\w+");
    match re.captures(value) {
        Some(caps) => format!("{}T{}", &caps[2], &caps[3]),
        None => value.to_string(),
    }
}

pub fn validate_unit(name: &str) -> bool {
    rx(&RE_UNIT, UNIT_PATTERN).is_match(name)
}

/// Action names accepted from the UI, exactly as in the Flask app.
pub fn valid_action(action: &str) -> bool {
    matches!(action, "start" | "stop" | "restart" | "enable" | "disable")
}

/// Scope declared for a catalog unit, or `system` for anything unknown — the
/// same fallback the Flask `manage_one` used.
pub fn scope_of(name: &str) -> &'static str {
    CATALOG
        .iter()
        .find(|entry| entry.name == name)
        .map(|entry| entry.scope)
        .unwrap_or("system")
}

/// Run one control action. Returns `(exit_ok, trimmed combined output)`; an
/// invalid unit or action is a client error, not a command result.
pub fn manage_one(name: &str, scope: &str, action: &str) -> Result<(bool, String), String> {
    if !valid_action(action) {
        return Err("invalid action".into());
    }
    if !validate_unit(name) {
        return Err("invalid unit name".into());
    }
    let scope = if scope == "user" { "user" } else { "system" };
    let mut args = ctl_prefix(scope);
    args.push(action.to_string());
    args.push(name.to_string());
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = exec::run(&argv, None, None, Duration::from_secs(30));
    Ok((out.ok(), out.trimmed()))
}

pub fn groups() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = CATALOG.iter().map(|entry| entry.group).collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Apply one action to every unit of a group. Ordering mirrors the Flask app:
/// start in catalog order (dependencies first), everything else in reverse.
pub fn group_action(group: &str, action: &str) -> Result<Vec<(String, bool, String)>, String> {
    if !valid_action(action) {
        return Err("invalid action".into());
    }
    let mut entries: Vec<&Entry> = CATALOG.iter().filter(|entry| entry.group == group).collect();
    if entries.is_empty() {
        return Err("unknown group".into());
    }
    entries.sort_by_key(|entry| entry.order);
    if action != "start" {
        entries.reverse();
    }
    Ok(entries
        .into_iter()
        .map(|entry| match manage_one(entry.name, entry.scope, action) {
            Ok((ok, output)) => (entry.name.to_string(), ok, output),
            Err(error) => (entry.name.to_string(), false, error),
        })
        .collect())
}

/// Last 300 journal lines for a unit, stdout and stderr concatenated like the
/// Flask route did. `None` means the unit name was rejected.
pub fn logs(unit: &str, scope: &str) -> Option<String> {
    if !validate_unit(unit) {
        return None;
    }
    let argv: Vec<&str> = if scope == "user" {
        vec!["journalctl", "--user", "-u", unit, "-n", "300", "--no-pager"]
    } else {
        vec!["sudo", "journalctl", "-u", unit, "-n", "300", "--no-pager"]
    };
    Some(exec::run(&argv, None, None, Duration::from_secs(30)).combined())
}

/// List systemd timers from both user and system scopes.
/// Mirrors the Flask `parse_system_timers` function.
pub fn list_timers() -> Vec<TimerInfo> {
    let mut out = Vec::new();
    for scope in ["user", "system"] {
        let argv: Vec<&str> = if scope == "user" {
            vec!["systemctl", "--user", "list-timers", "--all", "--no-pager"]
        } else {
            vec!["sudo", "systemctl", "list-timers", "--all", "--no-pager"]
        };
        let result = exec::run(&argv, None, None, std::time::Duration::from_secs(20));
        let text = result.combined();
        for line in text.lines() {
            if line.contains(".timer") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if let Some(unit) = parts.iter().find(|p| p.ends_with(".timer")) {
                    out.push(TimerInfo {
                        scope: scope.to_string(),
                        unit: unit.to_string(),
                        raw: line.trim().to_string(),
                    });
                }
            }
        }
    }
    out
}

/// `Wed 2026-10-21 03:36:44 MSK` and `2026-10-21T03:36:44` both render as
/// `2026-10-21 03:36:44`; an empty value renders as an em dash.
pub fn display_timestamp(value: Option<&str>) -> String {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return "—".to_string();
    };
    let re = rx(&RE_STAMP, r"^\w{3}\s+(\d{4}-\d{2}-\d{2})\s+(\d{2}:\d{2}:\d{2})");
    if let Some(caps) = re.captures(value) {
        return format!("{} {}", &caps[1], &caps[2]);
    }
    value.replace('T', " ").chars().take(19).collect()
}

fn rx<'a>(cell: &'a OnceLock<Regex>, pattern: &str) -> &'a Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

static RE_UNIT: OnceLock<Regex> = OnceLock::new();
static RE_TRIGGER: OnceLock<Regex> = OnceLock::new();
static RE_STAMP: OnceLock<Regex> = OnceLock::new();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_names_are_validated() {
        assert!(validate_unit("nginx.service"));
        assert!(validate_unit("immich-nightfix.timer"));
        assert!(!validate_unit("nginx"));
        assert!(!validate_unit("../../etc/passwd"));
        assert!(!validate_unit("nginx.service; rm -rf /"));
    }

    #[test]
    fn trigger_timestamp_is_normalised() {
        assert_eq!(format_trigger("Tue 2026-10-20 03:36:44 MSK"), "2026-10-20T03:36:44");
        assert_eq!(format_trigger(""), "");
        assert_eq!(format_trigger("garbage"), "garbage");
    }

    #[test]
    fn action_whitelist() {
        assert!(valid_action("restart"));
        assert!(!valid_action("rm"));
        assert!(!valid_action(""));
    }

    #[test]
    fn catalog_matches_flask() {
        assert_eq!(CATALOG.len(), 12);
        assert!(CATALOG.iter().any(|e| e.name == "stalwart.service" && e.scope == "user"));
        assert!(CATALOG.iter().any(|e| e.name == "php8.5-fpm.service" && e.scope == "system"));
    }

    #[test]
    fn timestamps_are_normalised_for_display() {
        assert_eq!(
            display_timestamp(Some("Wed 2026-10-21 03:36:44 MSK")),
            "2026-10-21 03:36:44"
        );
        assert_eq!(display_timestamp(Some("2026-10-21T03:36:44")), "2026-10-21 03:36:44");
        assert_eq!(display_timestamp(Some("   ")), "—");
        assert_eq!(display_timestamp(None), "—");
    }

    #[test]
    fn scope_falls_back_to_system() {
        assert_eq!(scope_of("nginx.service"), "system");
        assert_eq!(scope_of("stalwart.service"), "user");
        assert_eq!(scope_of("mystery.service"), "system");
    }

    #[test]
    fn group_action_rejects_unknown_input() {
        assert!(group_action("nope", "start").is_err());
        assert!(group_action("infra", "rm").is_err());
    }

    #[test]
    fn logs_rejects_bad_units() {
        assert!(logs("nginx.service; rm -rf /", "system").is_none());
        assert!(logs("../etc/passwd", "user").is_none());
    }
}
