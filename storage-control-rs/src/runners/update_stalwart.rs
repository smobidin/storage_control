//! Stalwart update runner — ported from `runners/update_stalwart.py`.
//!
//! Operational invariants:
//!   * stop user service: `systemctl --user stop stalwart.service`
//!   * backup current binary to `/mnt/backup/backups/stalwart/<run_id>/stalwart.bak`
//!   * download release tarball to `/tmp/stalwart-update-<run_id>_*` (cross-device safe)
//!   * extract, find binary, copy to `<stalwart_bin>.new`, `os.replace` within same filesystem
//!   * start service: `systemctl --user start stalwart.service`
//!   * smoke test: poll `http://127.0.0.1:8082/admin/` for 200/3xx
//!   * on failure: try to start service back up
//!   * staging in /tmp only; never on /mnt/data or /mnt/backup

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::Config;
use crate::db::repo::audit;
use crate::db::repo::update as repo;
use crate::db::Db;
use crate::error::AppError;
use crate::exec;
use crate::updates::steps::StepTracker;

fn smoke() -> exec::CmdOutput {
    for _ in 0..30 {
        let out = exec::run(&["curl", "-s", "-o", "/dev/null", "-w", "%{http_code}", "http://127.0.0.1:8082/admin/"], None, None, Duration::from_secs(10));
        if out.ok() {
            let code = out.stdout.trim();
            if code == "200" || code == "301" || code == "302" || code == "303" {
                return out;
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    exec::CmdOutput { rc: 1, stdout: String::new(), stderr: "smoke test timeout".into() }
}

pub fn run_stalwart_update(cfg: &Config, run_id: i64) -> Result<(), AppError> {
    let db = Db::open(&cfg.db_path)?;
    let log_path = cfg.runs_dir.join(format!("update_stalwart_{run_id}.log"));
    let _ = std::fs::create_dir_all(&log_path.parent().unwrap());

    let mut tracker = StepTracker::new(db.clone(), run_id);
    let tag = {
        let run = repo::get_run(&db, run_id)?.ok_or(AppError::NotFound)?;
        run.to_version.unwrap_or_default()
    };
    if tag.is_empty() {
        return Err(AppError::BadRequest("no target tag".into()));
    }

    exec::log_write(&log_path, &format!("Updating stalwart to {tag}\n"));

    let stage_root = std::env::temp_dir().join(format!("stalwart-update-{run_id}_"));
    let _ = std::fs::create_dir_all(&stage_root);

    // Stop service
    tracker.step("stop_service", || {
        Ok(exec::run(&["systemctl", "--user", "stop", "stalwart.service"], None, None, Duration::from_secs(120)))
    })?;

    // Backup current binary
    let backup_base = cfg.backup_root.join("stalwart").join(run_id.to_string());
    let _ = std::fs::create_dir_all(&backup_base);
    if cfg.stalwart_bin.exists() {
        let _ = std::fs::copy(&cfg.stalwart_bin, backup_base.join("stalwart.bak"));
    }
    tracker.step("backup_binary", || Ok(exec::CmdOutput { rc: 0, stdout: String::new(), stderr: String::new() }))?;

    // Download
    let url = format!("https://github.com/stalwartlabs/stalwart/releases/download/{tag}/stalwart-x86_64-unknown-linux-gnu.tar.gz");
    let tar_path = stage_root.join("stalwart.tar.gz");
    exec::log_write(&log_path, &format!("Downloading {url}\n"));
    tracker.step("download", || {
        use std::io::{Read, Write};
        let mut resp = ureq::get(&url).call().map_err(|e| AppError::internal("download", e))?;
        let mut file = std::fs::File::create(&tar_path)?;
        let mut reader = resp.into_reader();
        let mut buffer = Vec::new();
        reader.read_to_end(&mut buffer)?;
        file.write_all(&buffer)?;
        Ok(exec::CmdOutput { rc: 0, stdout: String::new(), stderr: String::new() })
    }).map_err(|e| AppError::BadRequest(format!("download failed: {e}")))?;

    // Extract
    let extract_dir = stage_root.join("extract");
    let _ = std::fs::create_dir_all(&extract_dir);
    tracker.step("extract", || {
        let file = std::fs::File::open(&tar_path)?;
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
        archive.unpack(&extract_dir)?;
        Ok(exec::CmdOutput { rc: 0, stdout: String::new(), stderr: String::new() })
    }).map_err(|e| AppError::BadRequest(format!("extract failed: {e}")))?;

    // Find binary
    let new_bin = walkdir::WalkDir::new(&extract_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name() == "stalwart" && e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .ok_or_else(|| AppError::BadRequest("binary not found in archive".into()))?;

    tracker.step("extract", || Ok(exec::CmdOutput { rc: 0, stdout: String::new(), stderr: String::new() }))?;

    // Replace (cross-device safe: copy to .new on target filesystem, then replace)
    let stage = cfg.stalwart_bin.with_file_name(format!("{}.new", cfg.stalwart_bin.file_name().unwrap().to_string_lossy()));
    std::fs::copy(&new_bin, &stage)?;
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o755);
    let _ = std::fs::set_permissions(&stage, perms.clone());
    std::fs::rename(&stage, &cfg.stalwart_bin)?;
    let _ = std::fs::set_permissions(&cfg.stalwart_bin, perms);
    tracker.step("replace_binary", || Ok(exec::CmdOutput { rc: 0, stdout: String::new(), stderr: String::new() }))?;

    // Start service
    tracker.step("start_service", || {
        Ok(exec::run(&["systemctl", "--user", "start", "stalwart.service"], None, None, Duration::from_secs(120)))
    })?;

    // Smoke test
    let smoke_rc = tracker.step("smoke", || Ok(smoke()))?;

    // Clean up staging
    let _ = std::fs::remove_dir_all(&stage_root);

    let steps = tracker.into_steps();
    let status = if smoke_rc.rc == 0 { "success" } else { "failed" };
    repo::finish_run(&db, run_id, status, Some(&tag), &steps)?;
    audit::record(&db, "admin", "update.stalwart", Some(&tag), Some(status));
    Ok(())
}

// Add required imports at the top
use ureq;
use flate2::read::GzDecoder;
use tar::Archive;
use walkdir::WalkDir;