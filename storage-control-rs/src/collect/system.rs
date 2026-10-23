//! Host overview: CPU, memory, swap, disks, temperatures.
//!
//! Ported from `collectors/system.py` (psutil) on top of `sysinfo`, with two
//! deliberate differences:
//!   * temperatures come from `/sys/class/hwmon` directly, which reproduces the
//!     per-chip grouping psutil produced (sysinfo flattens it);
//!   * disk usage is `total - available` (the same definition df uses).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Map, Value};
use sysinfo::{Disks, System};

use crate::exec;

const SKIP_FSTYPES: [&str; 7] = [
    "squashfs",
    "tmpfs",
    "devtmpfs",
    "overlay",
    "proc",
    "sysfs",
    "devpts",
];

struct Snapshot {
    cpu_pct: f64,
    per_core: Vec<f64>,
    freq_mhz: u64,
    logical: usize,
    mem_total: u64,
    mem_used: u64,
    mem_available: u64,
    swap_total: u64,
    swap_used: u64,
}

pub fn summary(sys: &Mutex<System>) -> Value {
    let snap = {
        let mut sys = sys.lock().unwrap_or_else(|e| e.into_inner());
        // (Re)compute usage since the previous call: the same "since last
        // refresh" semantics as psutil.cpu_percent(interval=None).
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        Snapshot {
            cpu_pct: sys.global_cpu_usage() as f64,
            per_core: sys.cpus().iter().map(|c| c.cpu_usage() as f64).collect(),
            freq_mhz: sys.cpus().first().map(|c| c.frequency()).unwrap_or(0),
            logical: sys.cpus().len(),
            mem_total: sys.total_memory(),
            mem_used: sys.used_memory(),
            mem_available: sys.available_memory(),
            swap_total: sys.total_swap(),
            swap_used: sys.used_swap(),
        }
    };

    let load = System::load_average();
    let cached = (meminfo_kb("Cached") + meminfo_kb("SReclaimable")) * 1024;

    json!({
        "hostname": System::host_name().unwrap_or_else(|| "unknown".into()),
        "os": System::long_os_version().or_else(System::os_version).unwrap_or_else(|| "unknown".into()),
        "uptime_seconds": System::uptime(),
        "cpu": {
            "percent_total": round1(snap.cpu_pct),
            "percent_per_core": snap.per_core.iter().map(|v| round1(*v)).collect::<Vec<_>>(),
            "count_logical": snap.logical,
            "count_physical": physical_cores(),
            "freq_mhz": snap.freq_mhz,
            "loadavg": [round2(load.one), round2(load.five), round2(load.fifteen)],
        },
        "memory": {
            "total": snap.mem_total,
            "available": snap.mem_available,
            "used": snap.mem_used,
            "percent": percent(snap.mem_used, snap.mem_total),
            "cached": cached,
        },
        "swap": {
            "total": snap.swap_total,
            "used": snap.swap_used,
            "percent": percent(snap.swap_used, snap.swap_total),
        },
        "disks": disks(),
        "temps": temps(),
    })
}

fn disks() -> Vec<Value> {
    let disks = Disks::new_with_refreshed_list();
    let mut out = Vec::new();
    for disk in &disks {
        let device = disk.name().to_string_lossy().to_string();
        if !device.starts_with("/dev/") || device.starts_with("/dev/loop") {
            continue;
        }
        let fstype = disk.file_system().to_string_lossy().to_string();
        if SKIP_FSTYPES.contains(&fstype.as_str()) {
            continue;
        }
        let total = disk.total_space();
        let available = disk.available_space();
        let used = total.saturating_sub(available);
        out.push(json!({
            "device": device,
            "mountpoint": disk.mount_point().to_string_lossy(),
            "fstype": fstype,
            "total": total,
            "used": used,
            "free": available,
            "percent": percent(used, total),
        }));
    }
    out.sort_by(|a, b| {
        a["mountpoint"]
            .as_str()
            .unwrap_or("")
            .cmp(b["mountpoint"].as_str().unwrap_or(""))
    });
    out
}

/// psutil-compatible temperature map: `{chip: [{label, current, high, critical}]}`.
/// Falls back to `sensors -j` when hwmon exposes nothing (some boards only
/// publish sensors through the CLI).
fn temps() -> Value {
    let mut out = Map::new();
    if let Ok(entries) = std::fs::read_dir("/sys/class/hwmon") {
        let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        dirs.sort();
        for dir in dirs {
            let chip = read_trim(&dir.join("name"))
                .or_else(|| dir.file_name().map(|n| n.to_string_lossy().to_string()))
                .unwrap_or_else(|| "hwmon".into());
            let items = hwmon_temps(&dir);
            if items.is_empty() {
                continue;
            }
            let entry = out.entry(chip).or_insert_with(|| json!([]));
            if let Some(array) = entry.as_array_mut() {
                array.extend(items);
            }
        }
    }

    if out.is_empty() {
        if let Some(value) = sensors_json() {
            return value;
        }
    }
    Value::Object(out)
}

fn hwmon_temps(dir: &Path) -> Vec<Value> {
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("temp") && name.ends_with("_input") {
            found.push((name.trim_end_matches("_input").to_string(), path));
        }
    }
    found.sort();

    let mut items = Vec::new();
    for (prefix, input) in found {
        let Some(current) = read_f64(&input) else {
            continue;
        };
        let label = read_trim(&dir.join(format!("{prefix}_label")))
            .unwrap_or_else(|| prefix.clone());
        let high = read_f64(&dir.join(format!("{prefix}_max")));
        let critical = read_f64(&dir.join(format!("{prefix}_crit")));
        items.push(json!({
            "label": label,
            "current": round1(current / 1000.0),
            "high": high.map(|v| round1(v / 1000.0)),
            "critical": critical.map(|v| round1(v / 1000.0)),
        }));
    }
    items
}

fn sensors_json() -> Option<Value> {
    let out = exec::run(
        &["sensors", "-j"],
        None,
        None,
        std::time::Duration::from_secs(5),
    );
    if !out.ok() {
        return None;
    }
    let parsed: Value = serde_json::from_str(&out.stdout).ok()?;
    parsed.as_object().map(|obj| Value::Object(obj.clone()))
}

fn meminfo_kb(key: &str) -> u64 {
    let Ok(text) = std::fs::read_to_string("/proc/meminfo") else {
        return 0;
    };
    for line in text.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name == key {
            return value
                .trim()
                .trim_end_matches(" kB")
                .trim()
                .parse::<u64>()
                .unwrap_or(0);
        }
    }
    0
}

/// Physical core count from /proc/cpuinfo: unique (physical id, core id) pairs.
fn physical_cores() -> usize {
    let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") else {
        return 0;
    };
    let mut pairs = std::collections::BTreeSet::new();
    let (mut physical, mut core) = (String::new(), String::new());
    for line in text.lines().chain(std::iter::once("")) {
        let Some((key, value)) = line.split_once(':') else {
            if line.trim().is_empty() {
                if !core.is_empty() {
                    pairs.insert((physical.clone(), core.clone()));
                }
                physical.clear();
                core.clear();
            }
            continue;
        };
        match key.trim() {
            "physical id" => physical = value.trim().to_string(),
            "core id" => core = value.trim().to_string(),
            _ => {}
        }
    }
    if pairs.is_empty() {
        // ARM boards have no physical/core ids; one socket per cpu entry.
        return num_cpus_from_cpuinfo();
    }
    pairs.len()
}

fn num_cpus_from_cpuinfo() -> usize {
    let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") else {
        return 0;
    };
    text.lines().filter(|l| l.starts_with("processor")).count()
}

fn read_trim(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn read_f64(path: &Path) -> Option<f64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn percent(used: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    round1(used as f64 * 100.0 / total as f64)
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_has_expected_shape() {
        let mut sys = System::new();
        sys.refresh_cpu_all();
        sys.refresh_memory();
        let value = summary(&Mutex::new(sys));
        for key in ["hostname", "os", "uptime_seconds", "cpu", "memory", "swap", "disks", "temps"] {
            assert!(value.get(key).is_some(), "missing {key}");
        }
        assert!(value["memory"]["total"].as_u64().unwrap() > 0);
        assert!(value["cpu"]["count_logical"].as_u64().unwrap() > 0);
        assert!(value["cpu"]["loadavg"].as_array().unwrap().len() == 3);
    }

    #[test]
    fn percent_guards_zero() {
        assert_eq!(percent(0, 0), 0.0);
        assert_eq!(percent(50, 200), 25.0);
    }

    #[test]
    fn physical_cores_are_plausible() {
        let cores = physical_cores();
        assert!(cores > 0, "host must report at least one core");
    }
}
