//! SMART collectors.
//!
//! Ported from `collectors/smart.py`. The attribute regexes are the same ones
//! the Flask app relied on; changing them silently breaks alerting, so they are
//! covered by tests against real captured smartctl output
//! (`tests/fixtures/raw/smart/*`, git-ignored).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use serde::Serialize;

use crate::exec;

/// Devices are discovered dynamically from /sys/block, like the Python
/// collector: no hard-coded disk list, virtual devices excluded.
const SKIP_PREFIXES: [&str; 6] = ["loop", "ram", "md", "zram", "dm-", "sr"];

pub fn disk_devices() -> Vec<String> {
    let mut devices = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/sys/block") {
        let mut paths: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for path in paths {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if SKIP_PREFIXES.iter().any(|prefix| name.starts_with(prefix)) {
                continue;
            }
            let size_ok = std::fs::read_to_string(path.join("size"))
                .ok()
                .and_then(|raw| raw.trim().parse::<u64>().ok())
                .map(|size| size > 0)
                .unwrap_or(false);
            if !size_ok {
                continue;
            }
            let dev = format!("/dev/{name}");
            if Path::new(&dev).exists() {
                devices.push(dev);
            }
        }
    }
    if devices.is_empty() {
        devices = vec!["/dev/sda".into(), "/dev/sdb".into()];
    }
    devices
}

pub fn run_smartctl(dev: &str) -> exec::CmdOutput {
    exec::run(
        &["sudo", "smartctl", "-a", dev],
        None,
        None,
        Duration::from_secs(30),
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct SmartInfo {
    pub device: String,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub health: String,
    pub temperature: Option<i64>,
    pub attributes: BTreeMap<String, i64>,
    pub power_on_hours: Option<i64>,
    pub raw_tail: String,
    pub rc: i32,
}

impl SmartInfo {
    pub fn attr(&self, key: &str) -> i64 {
        self.attributes.get(key).copied().unwrap_or(0)
    }
}

pub fn parse(dev: &str, text: &str) -> SmartInfo {
    let health = capture(rx(&RE_HEALTH, HEALTH_PATTERN), text)
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let temperature = capture(rx(&RE_TEMP, TEMP_PATTERN), text)
        .or_else(|| capture(rx(&RE_TEMP_NVME, TEMP_NVME_PATTERN), text))
        .and_then(|s| s.parse::<i64>().ok());

    let mut attributes = BTreeMap::new();
    for key in [
        "Reallocated_Sector_Ct",
        "Current_Pending_Sector",
        "Offline_Uncorrectable",
        "UDMA_CRC_Error_Count",
    ] {
        let pattern = format!(
            r"^\s*\d+\s+{key}\s+0x[0-9a-f]+\s+\d+\s+\d+\s+\d+\s+\S+\s+\S+\s+\S+\s+(\d+)\s*$"
        );
        if let Ok(re) = Regex::new(&format!("(?m){pattern}")) {
            if let Some(value) = capture(&re, text) {
                if let Ok(value) = value.parse::<i64>() {
                    attributes.insert(key.to_string(), value);
                }
            }
        }
    }

    let model = capture(rx(&RE_MODEL, MODEL_PATTERN), text).map(|s| s.trim().to_string());
    let serial = capture(rx(&RE_SERIAL, SERIAL_PATTERN), text).map(|s| s.trim().to_string());

    let power_on_hours = capture(rx(&RE_POH, POH_PATTERN), text)
        .or_else(|| capture(rx(&RE_POH_ALT, POH_ALT_PATTERN), text))
        .and_then(|s| s.parse::<i64>().ok());

    SmartInfo {
        device: dev.to_string(),
        model,
        serial,
        health,
        temperature,
        attributes,
        power_on_hours,
        raw_tail: tail(text, 1500),
        rc: 0,
    }
}

/// `ATA Error Count: N` — not a SMART attribute, parsed from the error log
/// section like the Python monitor did.
pub fn ata_error_count(text: &str) -> i64 {
    capture(rx(&RE_ATA, ATA_PATTERN), text)
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
}

/// Live SMART summary for every disk, one `smartctl -a` per device.
pub fn summary() -> Vec<SmartInfo> {
    let mut out = Vec::new();
    for dev in disk_devices() {
        let result = run_smartctl(&dev);
        let mut info = parse(&dev, &result.combined());
        info.rc = result.rc;
        out.push(info);
    }
    out
}

fn capture(re: &Regex, text: &str) -> Option<String> {
    re.captures(text).map(|caps| caps[1].to_string())
}

fn tail(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut start = text.len() - limit;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

fn rx<'a>(cell: &'a OnceLock<Regex>, pattern: &str) -> &'a Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

const HEALTH_PATTERN: &str = r"SMART overall-health self-assessment test result:\s*(.+)";
const TEMP_PATTERN: &str = r"(?m)Temperature_Celsius.*?(\d+)\s*(?:\(.*\))?\s*$";
const TEMP_NVME_PATTERN: &str = r"Current Drive Temperature:\s*(\d+)\s*C";
const MODEL_PATTERN: &str = r"Device Model:\s*(.+)";
const SERIAL_PATTERN: &str = r"Serial Number:\s*(.+)";
const POH_PATTERN: &str = r"(?m)^\s*9\s+Power_On_Hours\b.*\s(\d+)\s*$";
const POH_ALT_PATTERN: &str = r"Power On Hours:\s*(\d+)";
const ATA_PATTERN: &str = r"ATA Error Count:\s*(\d+)";

static RE_HEALTH: OnceLock<Regex> = OnceLock::new();
static RE_TEMP: OnceLock<Regex> = OnceLock::new();
static RE_TEMP_NVME: OnceLock<Regex> = OnceLock::new();
static RE_MODEL: OnceLock<Regex> = OnceLock::new();
static RE_SERIAL: OnceLock<Regex> = OnceLock::new();
static RE_POH: OnceLock<Regex> = OnceLock::new();
static RE_POH_ALT: OnceLock<Regex> = OnceLock::new();
static RE_ATA: OnceLock<Regex> = OnceLock::new();

#[cfg(test)]
mod tests {
    use super::*;

    const SATA: &str = "\
Device Model:     WDC WD40EFRX-68N32N0
Serial Number:    WD-WCC7K0ABCDEF
SMART overall-health self-assessment test result: PASSED

ID# ATTRIBUTE_NAME          FLAG     VALUE WORST THRESH TYPE      UPDATED  WHEN_FAILED RAW_VALUE
  5 Reallocated_Sector_Ct   0x0033   200   200   140    Pre-fail  Always       -       0
  9 Power_On_Hours          0x0032   072   072   000    Old_age   Always       -       20345
194 Temperature_Celsius     0x0022   109   094   000    Old_age   Always       -       43
197 Current_Pending_Sector  0x0012   200   200   000    Old_age   Always       -       8
198 Offline_Uncorrectable   0x0010   200   200   000    Old_age   Offline      -       2
199 UDMA_CRC_Error_Count    0x003e   200   200   000    Old_age   Always       -       1

ATA Error Count: 3
";

    #[test]
    fn parses_sata_drive() {
        let info = parse("/dev/sda", SATA);
        assert_eq!(info.health, "PASSED");
        assert_eq!(info.model.as_deref(), Some("WDC WD40EFRX-68N32N0"));
        assert_eq!(info.serial.as_deref(), Some("WD-WCC7K0ABCDEF"));
        assert_eq!(info.temperature, Some(43));
        assert_eq!(info.power_on_hours, Some(20345));
        assert_eq!(info.attr("Reallocated_Sector_Ct"), 0);
        assert_eq!(info.attr("Current_Pending_Sector"), 8);
        assert_eq!(info.attr("Offline_Uncorrectable"), 2);
        assert_eq!(info.attr("UDMA_CRC_Error_Count"), 1);
        assert_eq!(ata_error_count(SATA), 3);
    }

    #[test]
    fn missing_fields_do_not_panic() {
        let info = parse("/dev/sdz", "smartctl: command failed");
        assert_eq!(info.health, "unknown");
        assert_eq!(info.temperature, None);
        assert_eq!(info.attr("Reallocated_Sector_Ct"), 0);
        assert_eq!(ata_error_count("nothing here"), 0);
    }

    #[test]
    fn health_failure_is_captured() {
        let text = "SMART overall-health self-assessment test result: FAILED!\n";
        let info = parse("/dev/sdb", text);
        assert_eq!(info.health, "FAILED!");
    }

    #[test]
    fn raw_tail_is_utf8_safe() {
        let text = "ж".repeat(2000);
        let info = parse("/dev/sdc", &text);
        assert!(info.raw_tail.len() <= 1500);
        assert!(info.raw_tail.chars().all(|c| c == 'ж'));
    }

    #[test]
    fn nvme_temperature_fallback() {
        let text = "Current Drive Temperature:     38 C\n";
        let info = parse("/dev/nvme0", text);
        assert_eq!(info.temperature, Some(38));
    }
}
