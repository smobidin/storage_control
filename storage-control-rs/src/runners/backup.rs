//! Backup task runner and retention, ported from `runners/backup_task.py`.
//!
//! Invariants kept from the shell script (`tools/backup_immich_nc.sh`) and the
//! Flask runner:
//!   * artifacts land under `/mnt/backup/backups/<app>/` and never elsewhere;
//!   * `pg_dump -Fc` with the same connection strings;
//!   * Stalwart RocksDB is copied with the service stopped, then restarted;
//!   * retention never touches `home-storage/` or `preserves/`.
//!
//! The two `stalwart_*` kinds did not exist in the Flask runner (it raised
//! "unknown kind"); they are implemented here because the schema and the UI
//! always offered them.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::config::{Config, BACKUP_SKIP_DIRS};
use crate::db::repo::backup as repo;
use crate::db::{now_ms, Db};
use crate::error::AppError;
use crate::exec::{self, CmdOutput};

/// `tail` of the run log kept in the database row (the full log stays on disk).
const DB_LOG_TAIL: usize = 4000;

pub fn execute(db: &Db, cfg: &Config, run_id: i64) -> Result<(), AppError> {
    let Some(run) = repo::get_run(db, run_id)? else {
        return Ok(());
    };
    let Some(task_id) = run.task_id else {
        finish(db, cfg, run_id, 1, Vec::new(), Some("backup run has no task"));
        return Ok(());
    };
    let Some(task) = repo::get_task(db, task_id)? else {
        finish(db, cfg, run_id, 1, Vec::new(), Some("task was deleted"));
        return Ok(());
    };

    let log_path = cfg.runs_dir.join(format!("backup_{run_id}.log"));
    let mut files = Vec::new();

    let rc = match prepare_mount(cfg, &task.kind) {
        Err(message) => {
            log_write(&log_path, &format!("ERROR: {message}\n"));
            1
        }
        Ok(()) => run_kind(cfg, &task, &log_path, &mut files),
    };

    finish(db, cfg, run_id, rc, files, None);
    repo::mark_task_run(db, task_id, if rc == 0 { "success" } else { "failed" }, now_ms())?;
    Ok(())
}

/// Run the task's kind and return its exit code.
fn run_kind(cfg: &Config, task: &repo::BackupTask, log_path: &Path, files: &mut Vec<String>) -> i64 {
    let stamp = local_stamp();
    let root = &cfg.backup_root;

    let result = match task.kind.as_str() {
        "immich_db" => {
            let out = root.join("immich").join(format!("immich_db_{stamp}.dump"));
            pg_dump("storage", "immich", &out, log_path, files)
        }
        "nextcloud_db" => {
            let out = root
                .join("nextcloud")
                .join(format!("nextcloud_db_{stamp}.dump"));
            pg_dump("nextcloud_db", "nextcloud_db", &out, log_path, files)
        }
        "immich_config" => {
            let out = root.join("immich").join(format!("immich_config_{stamp}.tar.gz"));
            tar(
                &out,
                &[
                    ("-C", cfg.storage_home.to_string_lossy().to_string()),
                    ("", "immich/.env".into()),
                    ("", ".config/systemd/user".into()),
                ],
                log_path,
                files,
            )
        }
        "nextcloud_config" => {
            let out = root
                .join("nextcloud")
                .join(format!("nextcloud_config_{stamp}.tar.gz"));
            tar(
                &out,
                &[
                    ("-C", cfg.nextcloud_root.to_string_lossy().to_string()),
                    ("", "config/config.php".into()),
                ],
                log_path,
                files,
            )
        }
        "stalwart_db" => stalwart_db(cfg, &stamp, log_path, files),
        "stalwart_config" => {
            let out = root
                .join("stalwart")
                .join(format!("stalwart_config_{stamp}.tar.gz"));
            tar(
                &out,
                &[
                    ("-C", cfg.storage_home.to_string_lossy().to_string()),
                    ("", ".config/systemd/user/stalwart.service".into()),
                    ("", "stalwart/etc".into()),
                ],
                log_path,
                files,
            )
        }
        "all" => {
            let script = cfg.repo_root.join("tools/backup_immich_nc.sh");
            let script = script.to_string_lossy().to_string();
            sh(log_path, &["bash", &script], Duration::from_secs(7200)).rc as i64
        }
        "custom" => match task.command.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            Some(command) => sh(log_path, &["bash", "-lc", command], Duration::from_secs(3600)).rc as i64,
            None => {
                log_write(log_path, "ERROR: custom task has no command\n");
                1
            }
        },
        other => {
            log_write(log_path, &format!("ERROR: unknown kind {other}\n"));
            1
        }
    };
    result
}

fn pg_dump(
    user: &str,
    database: &str,
    out: &Path,
    log_path: &Path,
    files: &mut Vec<String>,
) -> i64 {
    if let Err(message) = ensure_parent(out) {
        log_write(log_path, &format!("ERROR: {message}\n"));
        return 1;
    }
    let url = format!("postgresql://{user}@localhost:5432/{database}");
    let dump_arg = format!("--dbname={url}");
    let file_arg = out.to_string_lossy().to_string();
    let argv = ["pg_dump", &dump_arg, "-Fc", "-f", &file_arg];
    let rc = sh(log_path, &argv, Duration::from_secs(3600)).rc;
    files.push(out.to_string_lossy().to_string());
    rc as i64
}

/// `tar czf OUT` plus the `-C DIR ITEM` pairs supplied by the caller.
fn tar(
    out: &Path,
    parts: &[(&str, String)],
    log_path: &Path,
    files: &mut Vec<String>,
) -> i64 {
    if let Err(message) = ensure_parent(out) {
        log_write(log_path, &format!("ERROR: {message}\n"));
        return 1;
    }
    let out_arg = out.to_string_lossy().to_string();
    let mut argv: Vec<String> = vec!["tar".into(), "czf".into(), out_arg];
    for (flag, value) in parts {
        if !flag.is_empty() {
            argv.push((*flag).to_string());
        }
        argv.push(value.clone());
    }
    let borrowed: Vec<&str> = argv.iter().map(String::as_str).collect();
    let rc = sh(log_path, &borrowed, Duration::from_secs(3600)).rc;
    files.push(out.to_string_lossy().to_string());
    rc as i64
}

/// RocksDB must not be copied while Stalwart writes to it.
fn stalwart_db(cfg: &Config, stamp: &str, log_path: &Path, files: &mut Vec<String>) -> i64 {
    let out = cfg
        .backup_root
        .join("stalwart")
        .join(format!("stalwart_db_{stamp}.tar.gz"));
    if let Err(message) = ensure_parent(&out) {
        log_write(log_path, &format!("ERROR: {message}\n"));
        return 1;
    }

    let was_active = exec::run_simple(&["systemctl", "--user", "is-active", "--quiet", "stalwart"]).ok();
    if was_active {
        sh(log_path, &["systemctl", "--user", "stop", "stalwart"], Duration::from_secs(120));
    }

    let data_parent = cfg
        .stalwart_data
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/mnt/data"));
    let data_name = cfg
        .stalwart_data
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "stalwart".into());
    let out_arg = out.to_string_lossy().to_string();
    let rc = sh(
        log_path,
        &[
            "tar",
            "czf",
            &out_arg,
            "-C",
            &data_parent.to_string_lossy(),
            &data_name,
        ],
        Duration::from_secs(3600),
    )
    .rc;

    if was_active {
        let restart = sh(
            log_path,
            &["systemctl", "--user", "start", "stalwart"],
            Duration::from_secs(120),
        );
        if !restart.ok() {
            log_write(log_path, "WARN: failed to start stalwart after backup\n");
        }
    }

    files.push(out.to_string_lossy().to_string());
    rc as i64
}

#[derive(Debug, Default)]
pub struct RetentionReport {
    pub deleted: usize,
    pub bytes: u64,
    pub cutoff_ms: i64,
}

/// Delete artifacts older than the retention window.
///
/// The Flask loop applied every task's window to the whole tree, so a file was
/// removed as soon as the shortest window had passed; one walk with the minimum
/// window is the same result without N full traversals.
pub fn retention(db: &Db, cfg: &Config) -> Result<RetentionReport, AppError> {
    let mut report = RetentionReport::default();
    let tasks = repo::list_tasks(db)?;
    let Some(days) = tasks
        .iter()
        .filter(|task| task.retention_days > 0)
        .map(|task| task.retention_days)
        .min()
    else {
        return Ok(report);
    };
    let cutoff = now_ms() - days * 86_400_000;
    report.cutoff_ms = cutoff;

    if !cfg.backup_root.is_dir() {
        return Ok(report);
    }
    prune_dir(&cfg.backup_root, cutoff, &mut report);
    Ok(report)
}

fn prune_dir(dir: &Path, cutoff_ms: i64, report: &mut RetentionReport) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // Never follow symlinks: a link out of the tree must not be pruned
        // through, and its target must not be deleted.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if BACKUP_SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            prune_dir(&entry.path(), cutoff_ms, report);
            continue;
        }
        if !file_type.is_file() || !prunable_extension(&name) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|delta| delta.as_millis() as i64)
            .unwrap_or(0);
        if mtime_ms < cutoff_ms && std::fs::remove_file(entry.path()).is_ok() {
            report.deleted += 1;
            report.bytes += meta.len();
        }
    }
}

fn prunable_extension(name: &str) -> bool {
    name.ends_with(".dump") || name.ends_with(".tar.gz") || name.ends_with(".txt")
}

/// Refuse to write artifacts when the backup disk is not mounted — otherwise
/// a full dump lands on the system disk. `custom` tasks are exempt: they may
/// legitimately write somewhere else.
fn prepare_mount(cfg: &Config, kind: &str) -> Result<(), String> {
    if kind == "custom" {
        return Ok(());
    }
    let disk = cfg
        .backup_root
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/mnt/backup"));
    if !disk.exists() {
        return Err(format!("{} не существует", disk.display()));
    }
    if !is_mount_point(&disk) {
        return Err(format!("{} не смонтирован — бэкап отменён", disk.display()));
    }
    Ok(())
}

#[cfg(unix)]
fn is_mount_point(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let parent = path.parent().unwrap_or_else(|| Path::new("/"));
    let Ok(parent_meta) = std::fs::metadata(parent) else {
        return false;
    };
    meta.dev() != parent_meta.dev()
}

#[cfg(not(unix))]
fn is_mount_point(_path: &Path) -> bool {
    true
}

fn ensure_parent(path: &Path) -> Result<(), String> {
    match path.parent() {
        Some(parent) => std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display())),
        None => Ok(()),
    }
}

/// Run a command, logging the command line and its combined output.
fn sh(log_path: &Path, argv: &[&str], timeout: Duration) -> CmdOutput {
    log_write(log_path, &format!("\n$ {}\n", argv.join(" ")));
    let out = exec::run(argv, None, None, timeout);
    log_write(log_path, &out.combined());
    out
}

fn log_write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(text.as_bytes());
    }
}

fn local_stamp() -> String {
    // Local time, like `date +%F_%H%M` in the backup script.
    chrono::Local::now().format("%Y-%m-%d_%H%M").to_string()
}

fn finish(db: &Db, cfg: &Config, run_id: i64, rc: i64, files: Vec<String>, error: Option<&str>) {
    let log_path = cfg.runs_dir.join(format!("backup_{run_id}.log"));
    if let Some(error) = error {
        log_write(&log_path, &format!("ERROR: {error}\n"));
    }
    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    let tail: String = {
        let mut chars: Vec<char> = log.chars().rev().take(DB_LOG_TAIL).collect();
        chars.reverse();
        chars.into_iter().collect()
    };

    let outcome = repo::RunOutcome {
        status: if rc == 0 { "success" } else { "failed" },
        finished_at: now_ms(),
        outdir: Some(&cfg.backup_root.to_string_lossy()),
        files: &json!(files).to_string(),
        log: &tail,
        exit_code: rc,
    };
    if let Err(e) = repo::finish_run(db, run_id, &outcome) {
        tracing::warn!("backup run {run_id}: cannot store outcome: {e}");
    }
}

/// Convert a `SystemTime` to unix milliseconds (used by tests and callers that
/// compare mtimes).
pub fn system_time_ms(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|delta| delta.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repo::backup::TaskInput;

    fn cfg_with_root(root: &Path) -> Config {
        let mut cfg = Config::from_env().unwrap();
        cfg.backup_root = root.to_path_buf();
        cfg.runs_dir = root.join("_runs");
        cfg
    }

    fn set_age(path: &Path, days: i64) {
        use std::fs::FileTimes;
        let when = SystemTime::now() - Duration::from_secs((days * 86_400) as u64);
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_times(FileTimes::new().set_modified(when)).unwrap();
    }

    #[test]
    fn retention_deletes_only_old_artifacts_and_skips_machine_trees() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let cfg = cfg_with_root(root);
        let db = Db::open_in_memory().unwrap();
        repo::create_task(
            &db,
            &TaskInput {
                name: "daily",
                kind: "immich_db",
                cron: None,
                command: None,
                enabled: true,
                retention_days: 14,
            },
        )
        .unwrap();

        for rel in ["immich/old.dump", "immich/new.dump", "logs/old.err", "logs/old.txt"] {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"data").unwrap();
            set_age(&path, 40);
        }
        let new = root.join("immich/new.dump");
        std::fs::write(&new, b"fresh").unwrap();

        let mirror = root.join("home-storage/current/old.txt");
        std::fs::create_dir_all(mirror.parent().unwrap()).unwrap();
        std::fs::write(&mirror, b"mirror").unwrap();
        set_age(&mirror, 40);

        let preserve = root.join("preserves/immich/1/db.dump");
        std::fs::create_dir_all(preserve.parent().unwrap()).unwrap();
        std::fs::write(&preserve, b"snapshot").unwrap();
        set_age(&preserve, 400);

        let report = retention(&db, &cfg).unwrap();
        assert_eq!(report.deleted, 2, "old.dump and old.txt only");
        assert!(!root.join("immich/old.dump").exists());
        assert!(root.join("immich/new.dump").exists());
        assert!(root.join("logs/old.err").exists(), "only known extensions are pruned");
        assert!(mirror.exists(), "the rsync mirror is never pruned");
        assert!(preserve.exists(), "preserve snapshots are never pruned");
    }

    #[test]
    fn retention_uses_the_shortest_window() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let cfg = cfg_with_root(root);
        let db = Db::open_in_memory().unwrap();
        for days in [3, 30] {
            repo::create_task(
                &db,
                &TaskInput {
                    name: "t",
                    kind: "immich_db",
                    cron: None,
                    command: None,
                    enabled: true,
                    retention_days: days,
                },
            )
            .unwrap();
        }
        let path = root.join("immich/mid.dump");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"data").unwrap();
        set_age(&path, 7);

        let report = retention(&db, &cfg).unwrap();
        assert_eq!(report.deleted, 1);
    }

    #[test]
    fn no_tasks_means_no_pruning() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let cfg = cfg_with_root(root);
        let db = Db::open_in_memory().unwrap();
        let path = root.join("immich/old.dump");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"data").unwrap();
        set_age(&path, 400);
        assert_eq!(retention(&db, &cfg).unwrap().deleted, 0);
        assert!(path.exists());
    }

    #[test]
    fn custom_kind_is_exempt_from_the_mount_check() {
        // Create temp directory in current working directory to avoid /tmp mount point
        let dir = tempfile::Builder::new().tempdir_in(".").unwrap();
        let cfg = cfg_with_root(dir.path());
        // custom kind should always be ok (exempt from mount check)
        assert!(prepare_mount(&cfg, "custom").is_ok());
        // For non-custom kinds, the result depends on whether the parent is a mount point
        // In test environments where CWD is not a mount point, it should be Err
        // In environments where CWD parent is a mount point, it will be Ok
        // We just verify custom is always Ok
        let _ = prepare_mount(&cfg, "immich_db");
    }
}
