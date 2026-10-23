//! RAID collector, ported from `collectors/raid.py`.

use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use serde_json::{json, Value};

use crate::exec;

const ARRAYS_PATTERN: &str = r"^(md\d+)\s*:\s*(.*)$";
const MDADM_PATTERN: &str = r"State\s*:\s*(.+)";
const RAID_DEVICES: &str = r"Raid Devices\s*:\s*(\d+)";
const ACTIVE_DEVICES: &str = r"Active Devices\s*:\s*(\d+)";
const FAILED_DEVICES: &str = r"Failed Devices\s*:\s*(\d+)";
const SPARE_DEVICES: &str = r"Spare Devices\s*:\s*(\d+)";
const TOTAL_DEVICES: &str = r"Total Devices\s*:\s*(\d+)";

pub fn summary() -> Value {
    let text = std::fs::read_to_string("/proc/mdstat").unwrap_or_default();

    // The Python version used a look-ahead to split arrays; Rust's regex crate
    // has none, so the split is a plain line scan: a header line starts a new
    // array, every following line belongs to its body.
    let mut arrays = Vec::new();
    let header = rx(&RE_ARRAYS, ARRAYS_PATTERN);
    let mut current: Option<(String, Vec<String>)> = None;
    let flush = |array: Option<(String, Vec<String>)>, out: &mut Vec<Value>| {
        if let Some((name, lines)) = array {
            let body = lines.join("\n").trim().to_string();
            out.push(json!({
                "name": name,
                "summary": body.lines().next().unwrap_or("").trim(),
                "raw": body,
            }));
        }
    };
    for line in text.lines() {
        if let Some(caps) = header.captures(line) {
            flush(current.take(), &mut arrays);
            current = Some((caps[1].to_string(), vec![caps[2].to_string()]));
        } else if let Some((_, lines)) = current.as_mut() {
            lines.push(line.to_string());
        }
    }
    flush(current.take(), &mut arrays);

    let detail = exec::run(
        &["sudo", "mdadm", "--detail", "/dev/md0"],
        None,
        None,
        Duration::from_secs(10),
    );
    let detail_text = if detail.ok() {
        detail.stdout.trim().to_string()
    } else if !detail.stderr.trim().is_empty() {
        detail.stderr.trim().to_string()
    } else {
        "mdadm failed".to_string()
    };

    json!({
        "arrays": arrays,
        "md0": parse_mdadm(&detail_text),
        "mdadm_detail": detail_text,
        "raw": text,
    })
}

pub fn parse_mdadm(detail: &str) -> Value {
    let mut out = json!({
        "state": "unknown",
        "raid_devices": Value::Null,
        "active_devices": Value::Null,
        "failed_devices": Value::Null,
        "spare_devices": Value::Null,
        "total_devices": Value::Null,
    });
    if detail.trim().is_empty() {
        return out;
    }
    if let Some(caps) = rx(&RE_STATE, MDADM_PATTERN).captures(detail) {
        out["state"] = json!(caps[1].trim());
    }
    for (key, re) in [
        ("raid_devices", &RE_RAID),
        ("active_devices", &RE_ACTIVE),
        ("failed_devices", &RE_FAILED),
        ("spare_devices", &RE_SPARE),
        ("total_devices", &RE_TOTAL),
    ] {
        let pattern = match key {
            "raid_devices" => RAID_DEVICES,
            "active_devices" => ACTIVE_DEVICES,
            "failed_devices" => FAILED_DEVICES,
            "spare_devices" => SPARE_DEVICES,
            _ => TOTAL_DEVICES,
        };
        if let Some(caps) = rx(re, pattern).captures(detail) {
            if let Ok(value) = caps[1].parse::<i64>() {
                out[key] = json!(value);
            }
        }
    }
    out
}

fn rx<'a>(cell: &'a OnceLock<Regex>, pattern: &str) -> &'a Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

static RE_ARRAYS: OnceLock<Regex> = OnceLock::new();
static RE_STATE: OnceLock<Regex> = OnceLock::new();
static RE_RAID: OnceLock<Regex> = OnceLock::new();
static RE_ACTIVE: OnceLock<Regex> = OnceLock::new();
static RE_FAILED: OnceLock<Regex> = OnceLock::new();
static RE_SPARE: OnceLock<Regex> = OnceLock::new();
static RE_TOTAL: OnceLock<Regex> = OnceLock::new();

#[cfg(test)]
mod tests {
    use super::*;

    const MDSTAT: &str = "\
Personalities : [raid1]
md0 : active raid1 sda1[0] sdb1[1]
      3906886464 blocks super 1.2 [2/2] [UU]
      bitmap: 0/30 pages [0KB], 65536KB chunk

unused devices: <none>
";

    #[test]
    fn parses_mdstat_array() {
        let re = rx(&RE_ARRAYS, ARRAYS_PATTERN);
        let caps: Vec<_> = MDSTAT.lines().filter_map(|line| re.captures(line)).collect();
        assert_eq!(caps.len(), 1);
        assert_eq!(&caps[0][1], "md0");
        assert!(caps[0][2].contains("active raid1"));
    }

    #[test]
    fn ignores_non_array_lines() {
        let text = "Personalities : [raid1]\nunused devices: <none>\n";
        let re = rx(&RE_ARRAYS, ARRAYS_PATTERN);
        assert_eq!(text.lines().filter(|l| re.is_match(l)).count(), 0);
    }

    #[test]
    fn parses_mdadm_detail() {
        let detail = "\
/dev/md0:
           Version : 1.2
     Creation Time : Sun Jan  1 00:00:00 2023
        Raid Level : raid1
        Array Size : 3906886464 (3725.90 GiB 4000.65 GB)
     Used Dev Size : 3906886464 (3725.90 GiB 4000.65 GB)
      Raid Devices : 2
     Total Devices : 2
       Persistence : Superblock is persistent

             State : clean
    Active Devices : 2
   Working Devices : 2
    Failed Devices : 0
     Spare Devices : 0
";
        let parsed = parse_mdadm(detail);
        assert_eq!(parsed["state"], "clean");
        assert_eq!(parsed["raid_devices"], 2);
        assert_eq!(parsed["active_devices"], 2);
        assert_eq!(parsed["failed_devices"], 0);
        assert_eq!(parsed["spare_devices"], 0);
        assert_eq!(parsed["total_devices"], 2);
    }

    #[test]
    fn empty_detail_is_unknown() {
        assert_eq!(parse_mdadm("")["state"], "unknown");
    }
}
