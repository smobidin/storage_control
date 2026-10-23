//! Nextcloud update runner — ported from `runners/update_nextcloud.py`.
//!
//! Operational invariants (must not change):
//!   * pre-update DB dump and config tar
//!   * `occ maintenance:mode --on`
//!   * `git fetch origin --tags --no-recurse-submodules`
//!   * `git checkout <tag>`
//!   * `git submodule update --init --recursive` (required! 3rdparty otherwise stale)
//!   * `make build-js-production` (non-fatal; Nextcloud ships prebuilt dist)
//!   * `occ upgrade` — may exit 1 due to stale optional apps even when DB is fine
//!   * verify with `occ status` (versionstring, maintenance=false, needsDbUpgrade=false)
//!   * `occ integrity:check-core`, `occ db:add-missing-indices`
//!   * `occ maintenance:mode --off`
//!   * restart php8.5-fpm and nginx (sudo)
//!   * viewer_sync: refresh apps/viewer from release tarball (only source; not on app store)
//!     - compare <nextcloud max-version> in info.xml; skip if covers new major
//!     - download ~250 MB tarball, extract viewer, replace, re-enable
//!   * smoke test: `occ status` and parse fields, not exit code
//!   * on failure: try to turn maintenance off

use std::path::Path;
use std::time::Duration;

use crate::config::Config;
use crate::db::repo::audit;
use crate::db::repo::update as repo;
use crate::db::Db;
use crate::error::AppError;
use crate::exec;
use crate::updates::steps::StepTracker;

const FATAL_MARKERS: &[&str] = &[
    "An unhandled exception has been thrown",
    "Traceback (most recent call last)",
];

fn fatal_in(output: &str) -> bool {
    FATAL_MARKERS.iter().any(|m| output.contains(m))
}

/// Parse `occ status` output and return problems (empty = healthy).
/// `occ status` exits 0 even when broken — we parse fields instead.
fn occ_status_problems(out: &str, expect_version: Option<&str>, check_maintenance: bool) -> Vec<String> {
    let mut info = std::collections::HashMap::new();
    for line in out.lines() {
        let s = line.trim().trim_start_matches('-').trim();
        if let Some((k, v)) = s.split_once(':') {
            info.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    let mut problems = Vec::new();
    if info.get("installed") != Some(&"true".to_string()) {
        problems.push(format!("installed={:?}", info.get("installed")));
    }
    if check_maintenance && info.get("maintenance") != Some(&"false".to_string()) {
        problems.push("still in maintenance mode".into());
    }
    if info.get("needsDbUpgrade") != Some(&"false".to_string()) {
        problems.push("needsDbUpgrade=true".into());
    }
    if let Some(expected) = expect_version {
        let expected = expected.trim_start_matches('v');
        if info.get("versionstring") != Some(&expected.to_string()) {
            problems.push(format!("versionstring={:?}, expected {}", info.get("versionstring"), expected));
        }
    }
    problems
}

/// Read <nextcloud max-version=".."> from an app's info.xml.
fn viewer_max_major(info_path: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(info_path).ok()?;
    let re = regex::Regex::new(r#"<nextcloud[^>]*\bmax-version=["'](\d+)["']"#).ok()?;
    re.captures(&text).and_then(|c| c.get(1).map(|m| m.as_str().parse().ok())).flatten()
}

/// Refresh apps/viewer from the release tarball matching the new version.
fn sync_viewer(cfg: &Config, log_path: &Path, tag: &str) -> exec::CmdOutput {
    let version = tag.trim_start_matches('v');
    let major: u64 = match version.split('.').next().and_then(|s| s.parse().ok()) {
        Some(m) => m,
        None => return exec::CmdOutput { rc: 1, stdout: String::new(), stderr: format!("cannot parse major version out of tag {tag}") },
    };

    let viewer_dir = cfg.nextcloud_root.join("apps/viewer");
    let installed_major = viewer_max_major(&viewer_dir.join("appinfo/info.xml"));
    if let Some(installed) = installed_major {
        if installed >= major {
            exec::log_write(log_path, &format!("viewer max-version={installed} covers server {version}; nothing to do\n"));
            return exec::CmdOutput { rc: 0, stdout: String::new(), stderr: String::new() };
        }
    }

    let url = format!("https://download.nextcloud.com/server/releases/nextcloud-{version}.tar.bz2");
    exec::log_write(log_path, &format!("viewer missing or incompatible (max-version={:?}); refreshing from {}\n", installed_major, url));

    let _ = std::fs::create_dir_all(&cfg.runs_dir);
    let archive = cfg.runs_dir.join(format!("nextcloud-{version}.tar.bz2"));

    let dl = exec::run(&["curl", "-sSfL", "--retry", "3", "--connect-timeout", "30", "-o", &archive.to_string_lossy(), &url], None, None, Duration::from_secs(7200));
    if !dl.ok() {
        return exec::CmdOutput { rc: dl.rc, stdout: String::new(), stderr: dl.trimmed() + "failed to download release tarball for viewer refresh\n" };
    }

    let stage = cfg.runs_dir.join("viewer_stage");
    let _ = std::fs::remove_dir_all(&stage);
    let _ = std::fs::create_dir_all(&stage);

    let extract = exec::run(&["tar", "-xjf", &archive.to_string_lossy(), "-C", &stage.to_string_lossy(), "--strip-components=1", "nextcloud/apps/viewer"], None, None, Duration::from_secs(600));
    if !extract.ok() {
        return extract;
    }

    let staged_dir = stage.join("apps/viewer");
    let staged_major = viewer_max_major(&staged_dir.join("appinfo/info.xml"));
    if staged_major.map_or(true, |m| m < major) {
        return exec::CmdOutput { rc: 1, stdout: String::new(), stderr: format!("tarball viewer max-version={:?} does not cover server {version}\n", staged_major) };
    }

    let _ = std::fs::remove_dir_all(&viewer_dir);
    let _ = std::fs::rename(&staged_dir, &viewer_dir);
    let _ = std::fs::remove_dir_all(&stage);

    let enable = exec::run(&["php", "occ", "app:enable", "viewer"], Some(&cfg.nextcloud_root), None, Duration::from_secs(120));
    if !enable.ok() {
        return enable;
    }
    let list = exec::run(&["php", "occ", "app:list", "--enabled"], Some(&cfg.nextcloud_root), None, Duration::from_secs(120));
    if !list.ok() || !regex::Regex::new(r"(?m)^\s*-\s*viewer:").unwrap().is_match(&list.stdout) {
        return exec::CmdOutput { rc: 1, stdout: String::new(), stderr: "viewer still not enabled after refresh\n".into() };
    }
    exec::log_write(log_path, &format!("viewer refreshed to {version}\n"));

    let _ = std::fs::remove_file(&archive);
    exec::CmdOutput { rc: 0, stdout: String::new(), stderr: String::new() }
}

fn occ(cfg: &Config, args: &[&str], log_path: &Path, timeout: Duration) -> exec::CmdOutput {
    let mut argv = vec!["php", "occ"];
    argv.extend(args);
    exec::run(&argv, Some(&cfg.nextcloud_root), None, timeout)
}

pub fn run_nextcloud_update(cfg: &Config, run_id: i64) -> Result<(), AppError> {
    let db = Db::open(&cfg.db_path)?;
    let log_path = cfg.runs_dir.join(format!("update_nextcloud_{run_id}.log"));
    let _ = std::fs::create_dir_all(&log_path.parent().unwrap());

    let mut tracker = StepTracker::new(db.clone(), run_id);
    let tag = {
        let run = repo::get_run(&db, run_id)?.ok_or(AppError::NotFound)?;
        run.to_version.unwrap_or_default()
    };
    if tag.is_empty() {
        return Err(AppError::BadRequest("no target tag".into()));
    }

    let backup_root = &cfg.backup_root;
    let _ = std::fs::create_dir_all(&backup_root.join("nextcloud"));

    tracker.step("backup_db", || {
        let out = backup_root.join("nextcloud").join(format!("nextcloud_db_preupdate_{}.dump", chrono::Local::now().format("%Y-%m-%d_%H%M")));
        Ok(exec::run(
            &["pg_dump", &format!("--dbname=postgresql://nextcloud_db@localhost:5432/nextcloud_db"), "-Fc", "-f", &out.to_string_lossy()],
            None, None, Duration::from_secs(3600),
        ))
    })?;

    tracker.step("backup_config", || {
        let out = backup_root.join("nextcloud").join(format!("nextcloud_config_preupdate_{}.tar.gz", chrono::Local::now().format("%Y-%m-%d_%H%M")));
        Ok(exec::run(
            &["tar", "czf", &out.to_string_lossy(), "-C", &cfg.nextcloud_root.to_string_lossy(), "config/config.php"],
            None, None, Duration::from_secs(300),
        ))
    })?;

    tracker.step("maintenance_on", || Ok(occ(cfg, &["maintenance:mode", "--on"], &log_path, Duration::from_secs(120))))?;

    tracker.step("git_fetch", || {
        Ok(exec::run(&["git", "fetch", "origin", "--tags", "--no-recurse-submodules"], Some(&cfg.nextcloud_root), None, Duration::from_secs(300)))
    })?;

    tracker.step("git_checkout", || {
        Ok(exec::run(&["git", "checkout", &tag], Some(&cfg.nextcloud_root), None, Duration::from_secs(300)))
    })?;

    tracker.step("git_submodules", || {
        Ok(exec::run(&["git", "submodule", "update", "--init", "--recursive"], Some(&cfg.nextcloud_root), None, Duration::from_secs(1800)))
    })?;

    let js_rc = tracker.step("build_js", || {
        Ok(exec::run(&["make", "build-js-production"], Some(&cfg.nextcloud_root), None, Duration::from_secs(1800)))
    })?;
    if js_rc.rc != 0 {
        exec::log_write(&log_path, "WARN: build-js-production failed; continuing\n");
    }

    tracker.step("occ_upgrade", || Ok(occ(cfg, &["upgrade"], &log_path, Duration::from_secs(1800))))?;

    // Verify upgrade applied
    let verify_out = tracker.step("verify_upgrade", || Ok(occ(cfg, &["status"], &log_path, Duration::from_secs(120))))?;
    let problems = occ_status_problems(&verify_out.trimmed(), Some(&tag), false);
    if !problems.is_empty() {
        return Err(AppError::BadRequest(format!("upgrade did not apply: {}", problems.join("; "))));
    }

    tracker.step("integrity_check", || Ok(occ(cfg, &["integrity:check-core"], &log_path, Duration::from_secs(300))))?;
    tracker.step("add_missing_indices", || Ok(occ(cfg, &["db:add-missing-indices"], &log_path, Duration::from_secs(300))))?;

    tracker.step("maintenance_off", || Ok(occ(cfg, &["maintenance:mode", "--off"], &log_path, Duration::from_secs(120))))?;

    tracker.step("restart_services", || {
        Ok(exec::run(&["sudo", "systemctl", "restart", "php8.5-fpm", "nginx"], None, None, Duration::from_secs(120)))
    })?;

    let viewer_rc = tracker.step("viewer_sync", || Ok(sync_viewer(cfg, &log_path, &tag)))?;
    if viewer_rc.rc != 0 {
        return Err(AppError::BadRequest("nextcloud upgraded, but refreshing apps/viewer failed -- office documents will download instead of opening in Collabora".into()));
    }

    let smoke_out = tracker.step("smoke_test", || Ok(occ(cfg, &["status"], &log_path, Duration::from_secs(120))))?;
    let problems = occ_status_problems(&smoke_out.trimmed(), Some(&tag), true);
    let smoke_failed = problems.iter().any(|p| !p.is_empty()) || fatal_in(&smoke_out.trimmed());
    if smoke_failed {
        exec::log_write(&log_path, &format!("SMOKE TEST FAILED: {}\n", problems.join("; ")));
        let steps = tracker.into_steps();
        repo::finish_run(&db, run_id, "failed", Some(&tag), &steps)?;
        audit::record(&db, "admin", "update.nextcloud", Some(&tag), Some("failed"));
    } else {
        let steps = tracker.into_steps();
        repo::finish_run(&db, run_id, "success", Some(&tag), &steps)?;
        audit::record(&db, "admin", "update.nextcloud", Some(&tag), Some("success"));
    }
    Ok(())
}