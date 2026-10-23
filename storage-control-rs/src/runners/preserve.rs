//! Preserve (restore point) runner — ported from `runners/preserve.py`.
//!
//! Creates a restorable snapshot of code, DB, and config before an update.
//! Tracks the git stash so the exact working tree can be restored.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::Config;
use crate::db::repo::audit;
use crate::db::repo::update as repo;
use crate::db::Db;
use crate::error::AppError;
use crate::exec;
use crate::updates::steps::StepTracker;
use crate::updates::versions::git_version;

fn copy_safe(source: &Path, destination: &Path) {
    if let Some(parent) = destination.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if source.exists() {
        let _ = std::fs::copy(source, destination);
    }
}

fn run_git_before(repo: &Path, log_path: &Path) -> String {
    let version = git_version(repo);
    exec::log_write(log_path, &format!("$ git version: {version}\n"));
    version
}

fn stash_repo(repo: &Path, preserve_id: i64, log_path: &Path) -> Option<String> {
    let status = exec::run(&["git", "status", "--porcelain"], Some(repo), None, Duration::from_secs(30));
    if !status.ok() || status.stdout.trim().is_empty() {
        return None;
    }
    let stash_msg = format!("storage-control preserve {preserve_id}");
    let stash = exec::run(&["git", "stash", "push", "-u", "-m", &stash_msg], Some(repo), None, Duration::from_secs(1800));
    if !stash.ok() {
        return None;
    }
    let list = exec::run(&["git", "stash", "list", "--format=%H", "-n", "1"], Some(repo), None, Duration::from_secs(30));
    let hash = list.stdout.lines().next().map(|s| s.trim().to_string());
    if let Some(ref h) = hash {
        let _ = exec::run(&["git", "update-ref", &format!("refs/preserve/{preserve_id}"), h], Some(repo), None, Duration::from_secs(30));
    }
    hash
}

pub fn prepare_preserve(cfg: &Config, preserve_id: i64) -> Result<(), AppError> {
    let db = Db::open(&cfg.db_path)?;
    let log_path = cfg.runs_dir.join(format!("prepare_{}_preserve_{}.log", 
        repo::get_preserve(&db, preserve_id)?.map(|p| p.target).unwrap_or_default(), preserve_id));
    let _ = std::fs::create_dir_all(&log_path.parent().unwrap());

    let mut tracker = StepTracker::new(db.clone(), preserve_id);
    repo::update_preserve_status(&db, preserve_id, "preparing")?;

    let target = {
        let p = repo::get_preserve(&db, preserve_id)?.ok_or(AppError::NotFound)?;
        p.target.clone()
    };
    let storage_dir = {
        let p = repo::get_preserve(&db, preserve_id)?.ok_or(AppError::NotFound)?;
        p.storage_dir.clone().unwrap_or_else(|| cfg.backup_root.join("preserves").join(&target).join(preserve_id.to_string()).to_string_lossy().to_string())
    };
    let base = Path::new(&storage_dir);
    let _ = std::fs::create_dir_all(base);

    let (repo_path, git_before, stash_hash) = match target.as_str() {
        "immich" => {
            let repo = &cfg.immich_src;
            let v = run_git_before(repo, &log_path);
            let h = stash_repo(repo, preserve_id, &log_path);
            tracker.step("backup_db", || {
                let out = base.join("immich_db.dump");
                Ok(exec::run(&["pg_dump", &format!("--dbname=postgresql://storage@localhost:5432/immich"), "-Fc", "-f", &out.to_string_lossy()], None, None, Duration::from_secs(3600)))
            })?;
            tracker.step("backup_config", || {
                let out = base.join("immich_config.tar.gz");
                Ok(exec::run(&["tar", "czf", &out.to_string_lossy(), "-C", &cfg.storage_home.to_string_lossy(), "immich/.env"], None, None, Duration::from_secs(300)))
            })?;
            copy_safe(&cfg.immich_env, &base.join(".env"));
            (repo.to_path_buf(), v, h)
        }
        "nextcloud" => {
            let repo = &cfg.nextcloud_root;
            let v = run_git_before(repo, &log_path);
            let h = stash_repo(repo, preserve_id, &log_path);
            tracker.step("backup_db", || {
                let out = base.join("nextcloud_db.dump");
                Ok(exec::run(&["pg_dump", &format!("--dbname=postgresql://nextcloud_db@localhost:5432/nextcloud_db"), "-Fc", "-f", &out.to_string_lossy()], None, None, Duration::from_secs(3600)))
            })?;
            tracker.step("backup_config", || {
                let out = base.join("nextcloud_config.tar.gz");
                Ok(exec::run(&["tar", "czf", &out.to_string_lossy(), "-C", &cfg.nextcloud_root.to_string_lossy(), "config/config.php"], None, None, Duration::from_secs(300)))
            })?;
            copy_safe(&cfg.nextcloud_root.join("config/config.php"), &base.join("config.php"));
            copy_safe(&cfg.nextcloud_root.join(".htaccess"), &base.join(".htaccess"));
            (repo.to_path_buf(), v, h)
        }
        "stalwart" => {
            let v = crate::updates::versions::stalwart_current(cfg);
            tracker.step("stop_service", || {
                Ok(exec::run(&["systemctl", "--user", "stop", "stalwart.service"], None, None, Duration::from_secs(120)))
            })?;
            tracker.step("backup_data", || {
                let out = base.join("stalwart_data.tar.gz");
                Ok(exec::run(&["tar", "czf", &out.to_string_lossy(), "-C", "/mnt/data", "stalwart"], None, None, Duration::from_secs(3600)))
            })?;
            tracker.step("backup_config", || {
                let out = base.join("stalwart_config.tar.gz");
                Ok(exec::run(&["tar", "czf", &out.to_string_lossy(), "-C", &cfg.storage_home.to_string_lossy(), ".config/systemd/user/stalwart.service", "stalwart/etc"], None, None, Duration::from_secs(300)))
            })?;
            let _ = exec::run(&["systemctl", "--user", "start", "stalwart.service"], None, None, Duration::from_secs(120));
            (Path::new("").to_path_buf(), v, None)
        }
        _ => return Err(AppError::BadRequest("unknown target".into())),
    };

    let extra_files = serde_json::json!({
        "config_archive": base.join(format!("{}_config.tar.gz", target)).to_string_lossy(),
        "htaccess": if target == "nextcloud" { serde_json::Value::String(base.join(".htaccess").to_string_lossy().to_string()) } else { serde_json::Value::Null },
    }).to_string();

    let steps = tracker.into_steps();
    repo::finish_preserve(
        &db, preserve_id, "ready",
        Some(&git_before), git_before.as_str().into(), stash_hash.as_deref(),
        Some(&base.join(format!("{}_db.dump", target)).to_string_lossy()),
        Some(&base.join(if target == "stalwart" { "stalwart_config.tar.gz" } else { ".env" }).to_string_lossy()),
        Some(&extra_files),
        &steps,
    )?;
    audit::record(&db, "admin", "update.prepare", Some(&target), Some(&preserve_id.to_string()));
    Ok(())
}

fn occ(cfg: &Config, args: &[&str], log_path: &Path, timeout: Duration) -> exec::CmdOutput {
    let mut argv = vec!["php", "occ"];
    argv.extend(args);
    exec::run(&argv, Some(&cfg.nextcloud_root), None, timeout)
}

pub fn restore_preserve(cfg: &Config, preserve_id: i64) -> Result<(), AppError> {
    let db = Db::open(&cfg.db_path)?;
    let log_path = cfg.runs_dir.join(format!("restore_preserve_{preserve_id}.log"));
    let _ = std::fs::create_dir_all(&log_path.parent().unwrap());

    let preserve = repo::get_preserve(&db, preserve_id)?.ok_or(AppError::NotFound)?;
    if !matches!(preserve.status.as_str(), "ready" | "failed" | "restored") {
        return Ok(());
    }

    repo::update_preserve_status(&db, preserve_id, "restoring")?;
    let mut tracker = StepTracker::new(db.clone(), preserve_id);
    let storage_dir = preserve.storage_dir.unwrap_or_default();
    let base = Path::new(&storage_dir);

    let result = match preserve.target.as_str() {
        "nextcloud" => {
            let _ = tracker.step("maintenance_on", || Ok(occ(cfg, &["maintenance:mode", "--on"], &log_path, Duration::from_secs(120))))?;
            copy_safe(&base.join("config.php"), &cfg.nextcloud_root.join("config/config.php"));
            copy_safe(&base.join(".htaccess"), &cfg.nextcloud_root.join(".htaccess"));
            let _ = tracker.step("git_checkout", || {
                Ok(exec::run(&["git", "checkout", &preserve.git_before.unwrap_or_else(|| "-".into())], Some(&cfg.nextcloud_root), None, Duration::from_secs(300)))
            })?;
            if let Some(stash) = preserve.git_stash {
                let _ = tracker.step("git_stash_apply", || {
                    Ok(exec::run(&["git", "stash", "apply", &stash], Some(&cfg.nextcloud_root), None, Duration::from_secs(300)))
                })?;
            }
            let _ = tracker.step("restore_db", || {
                Ok(exec::run(&["pg_restore", &format!("--dbname=postgresql://nextcloud_db@localhost:5432/nextcloud_db"), "--clean", "--if-exists", &base.join("nextcloud_db.dump").to_string_lossy()], None, None, Duration::from_secs(3600)))
            })?;
            let _ = tracker.step("maintenance_off", || Ok(occ(cfg, &["maintenance:mode", "--off"], &log_path, Duration::from_secs(120))))?;
            let _ = tracker.step("restart_services", || {
                Ok(exec::run(&["sudo", "systemctl", "restart", "php8.5-fpm", "nginx"], None, None, Duration::from_secs(120)))
            })?;
            Ok(())
        }
        "stalwart" => {
            let _ = exec::run(&["systemctl", "--user", "stop", "stalwart.service"], None, None, Duration::from_secs(120));
            let _ = tracker.step("restore_data", || {
                Ok(exec::run(&["tar", "xzf", &base.join("stalwart_data.tar.gz").to_string_lossy(), "-C", "/mnt/data"], None, None, Duration::from_secs(3600)))
            })?;
            let _ = exec::run(&["systemctl", "--user", "start", "stalwart.service"], None, None, Duration::from_secs(120));
            Ok(())
        }
        "immich" => {
            let _ = exec::run(&["systemctl", "--user", "stop", "immich-ml.service", "immich-server.service"], None, None, Duration::from_secs(120));
            let _ = tracker.step("restore_db", || {
                Ok(exec::run(&["pg_restore", &format!("--dbname=postgresql://storage@localhost:5432/immich"), "--clean", "--if-exists", &base.join("immich_db.dump").to_string_lossy()], None, None, Duration::from_secs(3600)))
            })?;
            copy_safe(&base.join(".env"), &cfg.immich_env);
            let _ = tracker.step("git_checkout", || {
                Ok(exec::run(&["git", "checkout", &preserve.git_before.unwrap_or_else(|| "-".into())], Some(&cfg.immich_src), None, Duration::from_secs(300)))
            })?;
            if let Some(stash) = preserve.git_stash {
                let _ = tracker.step("git_stash_apply", || {
                    Ok(exec::run(&["git", "stash", "apply", &stash], Some(&cfg.immich_src), None, Duration::from_secs(300)))
                })?;
            }
            let _ = exec::run(&["systemctl", "--user", "start", "immich-ml.service", "immich-server.service"], None, None, Duration::from_secs(120));
            Ok(())
        }
        _ => Err(AppError::BadRequest("unknown target".into())),
    };

    let steps = tracker.into_steps();
    if result.is_ok() {
        repo::mark_preserve_restored(&db, preserve_id, &steps)?;
        audit::record(&db, "admin", "update.restore", Some(&preserve.target), Some(&preserve_id.to_string()));
    } else {
        repo::set_preserve_error(&db, preserve_id, &result.unwrap_err().to_string(), &steps)?;
    }
    Ok(())
}

pub fn delete_preserve(cfg: &Config, preserve_id: i64) -> Result<(), AppError> {
    let db = Db::open(&cfg.db_path)?;
    if let Some(preserve) = repo::get_preserve(&db, preserve_id)? {
        let _ = std::fs::remove_dir_all(&preserve.storage_dir.unwrap_or_default());
    }
    repo::delete_preserve(&db, preserve_id)?;
    audit::record(&db, "admin", "update.preserve.delete", None, Some(&preserve_id.to_string()));
    Ok(())
}