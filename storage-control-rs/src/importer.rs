//! One-time import of the legacy Flask database (`app.db`) into the new schema.
//!
//! The legacy DB is opened read-only and never modified. The target must be
//! empty unless `--force` is passed, so a second run cannot silently double the
//! history. Everything is imported inside one transaction: a failure leaves the
//! new DB untouched.
//!
//! Legacy timestamps are naive-UTC TEXT (`2026-10-14 10:02:42.596762`); they are
//! converted to unix milliseconds, which is the only timestamp format the new
//! schema stores.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::db::{now_ms, Db};
use crate::error::AppError;

pub struct Options {
    pub from: PathBuf,
    pub to: PathBuf,
    pub force: bool,
    pub backup_root: PathBuf,
}

#[derive(Debug, Default)]
pub struct Report {
    pub counts: BTreeMap<String, usize>,
    pub notes: Vec<String>,
}

impl Report {
    fn bump(&mut self, table: &str, n: usize) {
        *self.counts.entry(table.to_string()).or_insert(0) += n;
    }

    fn note(&mut self, text: impl Into<String>) {
        self.notes.push(text.into());
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("storage-control legacy import\n");
        out.push_str("==============================\n\n");
        for (table, count) in &self.counts {
            out.push_str(&format!("{table:<24} {count:>6} rows\n"));
        }
        if !self.notes.is_empty() {
            out.push_str("\nnotes\n-----\n");
            for note in &self.notes {
                out.push_str(&format!("- {note}\n"));
            }
        }
        out
    }
}

pub fn import(options: &Options) -> Result<Report, AppError> {
    if !options.from.is_file() {
        return Err(AppError::BadRequest(format!(
            "legacy database not found: {}",
            options.from.display()
        )));
    }

    let legacy = Connection::open_with_flags(&options.from, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| AppError::internal("open legacy database", e))?;

    let target = Db::open(&options.to)?;
    if !options.force {
        let existing = existing_rows(&target)?;
        if existing > 0 {
            return Err(AppError::BadRequest(format!(
                "target database {} already has {existing} rows; pass --force to import anyway",
                options.to.display()
            )));
        }
    }

    let mut report = Report::default();
    target.with(|conn| {
        let tx = conn.transaction()?;
        import_users(&legacy, &tx, &mut report)?;
        import_audit(&legacy, &tx, &mut report)?;
        import_smart_disks(&legacy, &tx, &mut report)?;
        import_smart_alerts(&legacy, &tx, &mut report)?;
        import_smart_samples(&legacy, &tx, &mut report)?;
        import_update_runs(&legacy, &tx, &mut report)?;
        import_backup_tasks(&legacy, &tx, &mut report)?;
        import_backup_runs(&legacy, &tx, &mut report)?;
        import_scheduled_jobs(&legacy, &tx, &mut report)?;
        import_preserves(&legacy, &tx, &mut report)?;
        tx.commit()?;
        Ok(())
    })?;

    import_orphan_preserves(&target, &options.backup_root, &mut report)?;

    let total: usize = report.counts.values().sum();
    crate::db::repo::audit::record(
        &target,
        "import",
        "import.legacy_db",
        Some(&options.from.to_string_lossy()),
        Some(&format!("{total} rows")),
    );

    if report.notes.is_empty() {
        report.note("no anomalies detected");
    }
    let report_path = options
        .to
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("import-report.txt");
    std::fs::write(&report_path, report.render())?;
    report.note(format!("report written to {}", report_path.display()));

    Ok(report)
}

fn existing_rows(db: &Db) -> Result<i64, AppError> {
    db.with(|conn| {
        const TABLES: [&str; 10] = [
            "user",
            "audit",
            "update_run",
            "backup_task",
            "backup_run",
            "scheduled_job",
            "preserve",
            "smart_disk",
            "smart_sample",
            "smart_alert",
        ];
        let mut total = 0i64;
        for table in TABLES {
            total += conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })?;
        }
        Ok(total)
    })
}

// ------------------------------------------------------------------ tables

fn import_users(src: &Connection, tx: &Connection, report: &mut Report) -> rusqlite::Result<()> {
    let rows = collect(src, "SELECT id, username, password_hash, created_at FROM user", |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    for (id, username, hash, created_at) in &rows {
        tx.execute(
            "INSERT INTO user(id, username, password_hash, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(username) DO UPDATE SET password_hash = excluded.password_hash",
            (id, username, hash, ts_or_now(created_at.as_deref())),
        )?;
    }
    report.bump("user", rows.len());
    Ok(())
}

fn import_audit(src: &Connection, tx: &Connection, report: &mut Report) -> rusqlite::Result<()> {
    let rows = collect(
        src,
        "SELECT id, actor, action, obj, payload, ip, ts FROM audit_log",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        },
    )?;
    for (id, actor, action, object, payload, ip, ts) in &rows {
        tx.execute(
            "INSERT INTO audit(id, actor, action, object, payload, ip, ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (id, actor, action, object, payload, ip, ts_or_now(ts.as_deref())),
        )?;
    }
    report.bump("audit", rows.len());
    Ok(())
}

fn import_smart_disks(
    src: &Connection,
    tx: &Connection,
    report: &mut Report,
) -> rusqlite::Result<()> {
    let rows = collect(
        src,
        "SELECT id, device, model, serial, health, reallocated, pending,
                offline_uncorrectable, ata_error_count, temperature, power_on_hours,
                smartctl_rc, first_seen_at, last_checked_at, raw_summary, resolved_counters
         FROM smart_disk_state",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<i64>>(5)?.unwrap_or(0),
                row.get::<_, Option<i64>>(6)?.unwrap_or(0),
                row.get::<_, Option<i64>>(7)?.unwrap_or(0),
                row.get::<_, Option<i64>>(8)?.unwrap_or(0),
                row.get::<_, Option<i64>>(9)?,
                row.get::<_, Option<i64>>(10)?,
                row.get::<_, Option<i64>>(11)?,
                row.get::<_, Option<String>>(12)?,
                row.get::<_, Option<String>>(13)?,
                row.get::<_, Option<String>>(14)?,
                row.get::<_, Option<String>>(15)?,
            ))
        },
    )?;
    for row in &rows {
        let (
            id,
            device,
            model,
            serial,
            health,
            reallocated,
            pending,
            offline,
            ata,
            temperature,
            power_on_hours,
            rc,
            first_seen,
            last_checked,
            raw_summary,
            resolved,
        ) = row;
        tx.execute(
            "INSERT INTO smart_disk(id, device, model, serial, health, reallocated, pending,
                 offline_uncorrectable, ata_error_count, temperature, power_on_hours, smartctl_rc,
                 raw_summary, resolved_counters, first_seen_at, last_checked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            (
                id,
                device,
                model,
                serial,
                health,
                reallocated,
                pending,
                offline,
                ata,
                temperature,
                power_on_hours,
                rc,
                raw_summary,
                resolved,
                ts_or_now(first_seen.as_deref()),
                ts_or_now(last_checked.as_deref()),
            ),
        )?;
    }
    report.bump("smart_disk", rows.len());
    Ok(())
}

fn import_smart_alerts(
    src: &Connection,
    tx: &Connection,
    report: &mut Report,
) -> rusqlite::Result<()> {
    let rows = collect(
        src,
        "SELECT id, device, severity, message, active, created_at, resolved_at, alert_counters
         FROM smart_alert",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        },
    )?;
    for (id, device, severity, message, active, created_at, resolved_at, counters) in &rows {
        tx.execute(
            "INSERT INTO smart_alert(id, device, severity, message, active, created_at,
                 resolved_at, alert_counters)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            (
                id,
                device,
                severity_of(severity.as_deref()),
                message,
                active,
                ts_or_now(created_at.as_deref()),
                parse_ts(resolved_at.as_deref()),
                counters,
            ),
        )?;
    }
    report.bump("smart_alert", rows.len());
    Ok(())
}

fn import_smart_samples(
    src: &Connection,
    tx: &Connection,
    report: &mut Report,
) -> rusqlite::Result<()> {
    let rows = collect(
        src,
        "SELECT id, device, reallocated, pending, offline_uncorrectable, ata_error_count,
                temperature, health, smartctl_rc, checked_at, source
         FROM smart_disk_counter_history",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                row.get::<_, Option<i64>>(5)?.unwrap_or(0),
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
            ))
        },
    )?;
    for row in &rows {
        let (id, device, reallocated, pending, offline, ata, temperature, health, rc, checked_at, source) = row;
        tx.execute(
            "INSERT INTO smart_sample(id, device, reallocated, pending, offline_uncorrectable,
                 ata_error_count, temperature, health, smartctl_rc, checked_at, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            (
                id,
                device,
                reallocated,
                pending,
                offline,
                ata,
                temperature,
                health,
                rc,
                ts_or_now(checked_at.as_deref()),
                if source.as_deref() == Some("manual") { "manual" } else { "auto" },
            ),
        )?;
    }
    report.bump("smart_sample", rows.len());
    Ok(())
}

fn import_update_runs(
    src: &Connection,
    tx: &Connection,
    report: &mut Report,
) -> rusqlite::Result<()> {
    let rows = collect(
        src,
        "SELECT id, target, from_version, to_version, status, started_at, finished_at,
                log_path, backup_file, steps FROM update_run",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
            ))
        },
    )?;
    let mut skipped = 0;
    for (id, target, from, to, status, started, finished, log_path, backup_file, steps) in &rows {
        let Some(target) = target_of(target) else {
            skipped += 1;
            continue;
        };
        tx.execute(
            "INSERT INTO update_run(id, target, from_version, to_version, status, started_at,
                 finished_at, log_path, backup_file, steps)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            (
                id,
                target,
                from,
                to,
                run_status(status.as_deref()),
                ts_or_now(started.as_deref()),
                parse_ts(finished.as_deref()),
                log_path,
                backup_file,
                steps,
            ),
        )?;
    }
    report.bump("update_run", rows.len() - skipped);
    if skipped > 0 {
        report.note(format!("{skipped} update_run rows skipped: unknown target"));
    }
    Ok(())
}

fn import_backup_tasks(
    src: &Connection,
    tx: &Connection,
    report: &mut Report,
) -> rusqlite::Result<()> {
    let rows = collect(
        src,
        "SELECT id, name, kind, schedule_cron, command, enabled, retention_days,
                last_run_at, last_status, created_at, updated_at FROM backup_task",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<i64>>(5)?.unwrap_or(1),
                row.get::<_, Option<i64>>(6)?.unwrap_or(14),
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
            ))
        },
    )?;
    for row in &rows {
        let (id, name, kind, cron, command, enabled, retention, last_run, last_status, created, updated) = row;
        let created_ms = ts_or_now(created.as_deref());
        tx.execute(
            "INSERT INTO backup_task(id, name, kind, cron, command, enabled, retention_days,
                 last_run_at, last_status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            (
                id,
                name,
                backup_kind(kind),
                cron,
                command,
                enabled,
                retention,
                parse_ts(last_run.as_deref()),
                last_status,
                created_ms,
                parse_ts(updated.as_deref()).unwrap_or(created_ms),
            ),
        )?;
    }
    report.bump("backup_task", rows.len());
    Ok(())
}

fn import_backup_runs(
    src: &Connection,
    tx: &Connection,
    report: &mut Report,
) -> rusqlite::Result<()> {
    let rows = collect(
        src,
        "SELECT id, task_id, status, started_at, finished_at, outdir, files, log, exit_code
         FROM backup_run",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<i64>>(8)?,
            ))
        },
    )?;
    let mut orphaned = 0;
    for (id, task_id, status, started, finished, outdir, files, log, exit_code) in &rows {
        // The new schema enforces the FK; a run whose task was deleted becomes
        // a task-less run instead of a hard failure.
        let task_id = match task_id {
            Some(tid) if tx.query_row("SELECT 1 FROM backup_task WHERE id = ?1", [tid], |_| Ok(())).is_ok() => {
                Some(*tid)
            }
            Some(_) => {
                orphaned += 1;
                None
            }
            None => None,
        };
        tx.execute(
            "INSERT INTO backup_run(id, task_id, status, started_at, finished_at, outdir,
                 files, log, exit_code)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            (
                id,
                task_id,
                backup_status(status.as_deref()),
                ts_or_now(started.as_deref()),
                parse_ts(finished.as_deref()),
                outdir,
                files,
                log,
                exit_code,
            ),
        )?;
    }
    report.bump("backup_run", rows.len());
    if orphaned > 0 {
        report.note(format!("{orphaned} backup_run rows lost their task (task_id set to NULL)"));
    }
    Ok(())
}

fn import_scheduled_jobs(
    src: &Connection,
    tx: &Connection,
    report: &mut Report,
) -> rusqlite::Result<()> {
    let rows = collect(
        src,
        "SELECT id, name, job_type, target, schedule_cron, enabled, notes, last_run_at,
                last_status, created_at FROM scheduled_job",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<i64>>(5)?.unwrap_or(1),
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
            ))
        },
    )?;
    for (id, name, job_type, target, cron, enabled, notes, last_run, last_status, created) in &rows {
        tx.execute(
            "INSERT INTO scheduled_job(id, name, job_type, target, cron, enabled, notes,
                 last_run_at, last_status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            (
                id,
                name,
                job_type,
                target,
                cron,
                enabled,
                notes,
                parse_ts(last_run.as_deref()),
                last_status,
                ts_or_now(created.as_deref()),
            ),
        )?;
    }
    report.bump("scheduled_job", rows.len());
    Ok(())
}

fn import_preserves(
    src: &Connection,
    tx: &Connection,
    report: &mut Report,
) -> rusqlite::Result<()> {
    let rows = collect(
        src,
        "SELECT id, target, from_version, status, storage_dir, db_dump, config_path,
                extra_files, git_before, git_stash, log_path, error, steps, created_at,
                finished_at, restored_at, note FROM update_preserve",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, Option<String>>(12)?,
                row.get::<_, Option<String>>(13)?,
                row.get::<_, Option<String>>(14)?,
                row.get::<_, Option<String>>(15)?,
                row.get::<_, Option<String>>(16)?,
            ))
        },
    )?;
    let mut skipped = 0;
    for row in &rows {
        let (id, target, from_version, status, storage_dir, db_dump, config_path, extra_files, git_before, git_stash, log_path, error, steps, created, finished, restored, note) = row;
        let Some(target) = target_of(target) else {
            skipped += 1;
            continue;
        };
        let target_owned = target.to_string();
        let created_ms = ts_or_now(created.as_deref());
        let finished_ms = parse_ts(finished.as_deref());
        let restored_ms = parse_ts(restored.as_deref());
        let status_owned = preserve_status(status.as_deref()).to_string();
        let params: Vec<&dyn rusqlite::ToSql> = vec![
            id,
            &target_owned,
            from_version,
            &status_owned,
            storage_dir,
            db_dump,
            config_path,
            extra_files,
            git_before,
            git_stash,
            log_path,
            error,
            steps,
            &created_ms,
            &finished_ms,
            &restored_ms,
            note,
        ];
        tx.execute(
            "INSERT INTO preserve(id, target, from_version, status, storage_dir, db_dump,
                 config_path, extra_files, git_before, git_stash, log_path, error, steps,
                 created_at, finished_at, restored_at, note)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params.as_slice(),
        )?;
    }
    report.bump("preserve", rows.len() - skipped);
    if skipped > 0 {
        report.note(format!("{skipped} preserve rows skipped: unknown target"));
    }
    Ok(())
}

/// Directories under `preserves/<target>/<id>` that the legacy DB never recorded
/// (a crashed prepare leaves the data on disk). They are imported as `ready`
/// with a note so the operator can see and delete them.
fn import_orphan_preserves(
    db: &Db,
    backup_root: &Path,
    report: &mut Report,
) -> Result<(), AppError> {
    let root = backup_root.join("preserves");
    if !root.is_dir() {
        return Ok(());
    }
    let mut known: Vec<String> = db.with(|conn| {
        let mut stmt = conn.prepare("SELECT storage_dir FROM preserve WHERE storage_dir IS NOT NULL")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect()
    })?;
    known.sort();

    let mut added = 0;
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&root)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    entries.sort();
    for target_dir in entries {
        if !target_dir.is_dir() {
            continue;
        }
        let Some(target_name) = target_dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !matches!(target_name, "immich" | "nextcloud" | "stalwart") {
            continue;
        }
        let mut id_dirs: Vec<PathBuf> = std::fs::read_dir(&target_dir)?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_dir())
            .collect();
        id_dirs.sort();
        for id_dir in id_dirs {
            let storage_dir = id_dir.to_string_lossy().to_string();
            if known.iter().any(|known_dir| known_dir == &storage_dir) {
                continue;
            }
            let created = id_dir
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis() as i64)
                .unwrap_or_else(now_ms);
            db.with(|conn| {
                conn.execute(
                    "INSERT INTO preserve(target, status, storage_dir, created_at, note)
                     VALUES (?1, 'ready', ?2, ?3, ?4)",
                    (
                        target_name,
                        &storage_dir,
                        created,
                        "imported from disk (no legacy DB row)",
                    ),
                )
                .map(|_| ())
            })?;
            added += 1;
        }
    }
    if added > 0 {
        report.bump("preserve (orphans)", added);
        report.note(format!("{added} preserve directories found on disk had no DB row"));
    }
    Ok(())
}

// ------------------------------------------------------------------ helpers

fn collect<T>(
    src: &Connection,
    sql: &str,
    map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> rusqlite::Result<Vec<T>> {
    let mut stmt = src.prepare(sql)?;
    let rows = stmt.query_map([], map)?;
    rows.collect()
}

pub fn parse_ts(value: Option<&str>) -> Option<i64> {
    let text = value?.trim();
    if text.is_empty() {
        return None;
    }
    for format in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d",
    ] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(text, format) {
            return Some(naive.and_utc().timestamp_millis());
        }
        if let Ok(date) = chrono::NaiveDate::parse_from_str(text, format) {
            return Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis());
        }
    }
    None
}

fn ts_or_now(value: Option<&str>) -> i64 {
    parse_ts(value).unwrap_or_else(now_ms)
}

fn target_of(value: &str) -> Option<&'static str> {
    match value {
        "immich" => Some("immich"),
        "nextcloud" => Some("nextcloud"),
        "stalwart" => Some("stalwart"),
        _ => None,
    }
}

fn severity_of(value: Option<&str>) -> &'static str {
    match value {
        Some("critical") => "critical",
        _ => "warning",
    }
}

fn run_status(value: Option<&str>) -> &'static str {
    match value {
        Some("running") => "running",
        Some("success") => "success",
        Some("failed") => "failed",
        _ => "pending",
    }
}

fn backup_status(value: Option<&str>) -> &'static str {
    match value {
        Some("success") => "success",
        Some("failed") => "failed",
        _ => "running",
    }
}

fn backup_kind(value: &str) -> &'static str {
    match value {
        "immich_db" => "immich_db",
        "nextcloud_db" => "nextcloud_db",
        "immich_config" => "immich_config",
        "nextcloud_config" => "nextcloud_config",
        "stalwart_db" => "stalwart_db",
        "stalwart_config" => "stalwart_config",
        "all" => "all",
        _ => "custom",
    }
}

fn preserve_status(value: Option<&str>) -> &'static str {
    match value {
        Some("pending") => "pending",
        Some("preparing") => "preparing",
        Some("ready") => "ready",
        Some("restoring") => "restoring",
        Some("restored") => "restored",
        _ => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_legacy_timestamps() {
        let ms = parse_ts(Some("2026-10-14 08:55:51.757685")).unwrap();
        assert_eq!(ms, 1_791_968_151_757);
        assert_eq!(parse_ts(Some("2026-10-14 08:55:51")), Some(1_791_968_151_000));
        assert_eq!(parse_ts(Some("2026-10-14T08:55:51")), Some(1_791_968_151_000));
        assert_eq!(parse_ts(Some("2026-10-14")), Some(1_791_936_000_000));
        assert_eq!(parse_ts(Some("")), None);
        assert_eq!(parse_ts(None), None);
        assert_eq!(parse_ts(Some("garbage")), None);
    }

    #[test]
    fn statuses_are_sanitised() {
        assert_eq!(severity_of(Some("critical")), "critical");
        assert_eq!(severity_of(Some("whatever")), "warning");
        assert_eq!(run_status(Some("success")), "success");
        assert_eq!(run_status(None), "pending");
        assert_eq!(backup_status(Some("failed")), "failed");
        assert_eq!(backup_kind("nonsense"), "custom");
        assert_eq!(preserve_status(Some("ready")), "ready");
        assert_eq!(preserve_status(Some("bogus")), "failed");
    }

    #[test]
    fn import_into_memory_roundtrip() {
        // Build a miniature legacy DB in a temp file with just enough rows.
        let dir = tempfile::tempdir().unwrap();
        let legacy_path = dir.path().join("app.db");
        let target_path = dir.path().join("storage-control.db");
        {
            let conn = Connection::open(&legacy_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE user (id INTEGER PRIMARY KEY, username TEXT, password_hash TEXT, created_at DATETIME);
                 CREATE TABLE audit_log (id INTEGER PRIMARY KEY, actor TEXT, action TEXT, obj TEXT, payload TEXT, ip TEXT, ts DATETIME);
                 CREATE TABLE smart_disk_state (id INTEGER PRIMARY KEY, device TEXT, model TEXT, serial TEXT,
                     health TEXT, reallocated INTEGER, pending INTEGER, offline_uncorrectable INTEGER,
                     ata_error_count INTEGER, temperature INTEGER, power_on_hours INTEGER, smartctl_rc INTEGER,
                     first_seen_at DATETIME, last_checked_at DATETIME, raw_summary TEXT, resolved_counters TEXT);
                 CREATE TABLE smart_alert (id INTEGER PRIMARY KEY, device TEXT, severity TEXT, message TEXT,
                     active BOOLEAN, created_at DATETIME, resolved_at DATETIME, alert_counters TEXT);
                 CREATE TABLE smart_disk_counter_history (id INTEGER PRIMARY KEY, device TEXT, reallocated INTEGER,
                     pending INTEGER, offline_uncorrectable INTEGER, ata_error_count INTEGER, temperature INTEGER,
                     health TEXT, smartctl_rc INTEGER, checked_at DATETIME, source TEXT);
                 CREATE TABLE update_run (id INTEGER PRIMARY KEY, target TEXT, from_version TEXT, to_version TEXT,
                     status TEXT, started_at DATETIME, finished_at DATETIME, log_path TEXT, backup_file TEXT, steps TEXT);
                 CREATE TABLE backup_task (id INTEGER PRIMARY KEY, name TEXT, kind TEXT, schedule_cron TEXT,
                     command TEXT, enabled BOOLEAN, retention_days INTEGER, last_run_at DATETIME, last_status TEXT,
                     created_at DATETIME, updated_at DATETIME);
                 CREATE TABLE backup_run (id INTEGER PRIMARY KEY, task_id INTEGER, status TEXT, started_at DATETIME,
                     finished_at DATETIME, outdir TEXT, files TEXT, log TEXT, exit_code INTEGER);
                 CREATE TABLE scheduled_job (id INTEGER PRIMARY KEY, name TEXT, job_type TEXT, target TEXT,
                     schedule_cron TEXT, enabled BOOLEAN, notes TEXT, last_run_at DATETIME, last_status TEXT,
                     created_at DATETIME);
                 CREATE TABLE update_preserve (id INTEGER PRIMARY KEY, target TEXT, from_version TEXT, status TEXT,
                     storage_dir TEXT, db_dump TEXT, config_path TEXT, extra_files TEXT, git_before TEXT,
                     git_stash TEXT, log_path TEXT, error TEXT, steps TEXT, created_at DATETIME,
                     finished_at DATETIME, restored_at DATETIME, note TEXT);
                 INSERT INTO user VALUES (1, 'admin', 'hash', '2026-10-14 08:55:51.757685');
                 INSERT INTO audit_log VALUES (1, 'admin', 'backup_task.create', 'test', NULL, NULL, '2026-10-14 09:01:31.062620');
                 INSERT INTO smart_disk_state VALUES (1, '/dev/sda', 'Model', 'Serial', 'PASSED', 0, 0, 0, 0, 32, 256, 0,
                     '2026-10-14 10:43:17.826262', '2026-10-23 04:26:55.901722', 'Model', NULL);
                 INSERT INTO smart_alert VALUES (42, '/dev/sdc', 'critical', 'msg', 0,
                     '2026-10-18 13:28:26.704053', '2026-10-18 22:00:00.024436', NULL);
                 INSERT INTO smart_disk_counter_history VALUES (1, '/dev/sda', 0, 0, 0, 0, 32, 'PASSED', 0,
                     '2026-10-20 04:00:00.000000', 'auto');
                 INSERT INTO update_run VALUES (1, 'nextcloud', 'v34.0.4', 'v35.0.1', 'success',
                     '2026-10-14 10:25:26.501203', '2026-10-14 10:27:11.595865', '/tmp/log', NULL, '[]');
                 INSERT INTO backup_task VALUES (1, 'Immich DB daily', 'immich_db', NULL, NULL, 1, 14, NULL, NULL,
                     '2026-10-20 15:20:31.711963', '2026-10-20 15:20:31.711967');
                 INSERT INTO backup_run VALUES (1, 99, 'success', '2026-10-20 04:00:00', '2026-10-20 04:01:00', '/tmp/out', '[]', 'log', 0);
                 INSERT INTO scheduled_job VALUES (1, 'job', 'update', 'immich', '0 4 * * *', 1, NULL, NULL, NULL,
                     '2026-10-20 15:20:31.711963');
                 INSERT INTO update_preserve VALUES (1, 'nextcloud', 'v34.0.4', 'ready', '/mnt/backup/backups/preserves/nextcloud/1',
                     '/mnt/backup/backups/preserves/nextcloud/1/db.dump', '/mnt/backup/backups/preserves/nextcloud/1/config.php',
                     '{}', 'v34.0.4', NULL, '/tmp/log', NULL, '[]', '2026-10-14 10:02:42.596762',
                     '2026-10-14 10:02:43.857346', NULL, NULL);",
            )
            .unwrap();
        }

        let dir_root = dir.path().join("backups");
        std::fs::create_dir_all(dir_root.join("preserves/immich/7")).unwrap();

        let report = import(&Options {
            from: legacy_path,
            to: target_path.clone(),
            force: false,
            backup_root: dir_root,
        })
        .unwrap();

        assert_eq!(report.counts.get("user"), Some(&1));
        assert_eq!(report.counts.get("audit"), Some(&1));
        assert_eq!(report.counts.get("smart_disk"), Some(&1));
        assert_eq!(report.counts.get("smart_alert"), Some(&1));
        assert_eq!(report.counts.get("smart_sample"), Some(&1));
        assert_eq!(report.counts.get("update_run"), Some(&1));
        assert_eq!(report.counts.get("backup_task"), Some(&1));
        assert_eq!(report.counts.get("backup_run"), Some(&1));
        assert_eq!(report.counts.get("preserve (orphans)"), Some(&1));

        let db = Db::open(&target_path).unwrap();
        let run_task: Option<i64> = db
            .with(|c| c.query_row("SELECT task_id FROM backup_run WHERE id = 1", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(run_task, None, "orphan run must lose its task");
        let orphans: i64 = db
            .with(|c| c.query_row("SELECT count(*) FROM preserve WHERE storage_dir LIKE '%/7'", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(orphans, 1);

        // Second import without --force must refuse.
        let again = import(&Options {
            from: dir.path().join("app.db"),
            to: target_path,
            force: false,
            backup_root: dir.path().join("backups"),
        });
        assert!(again.is_err());
    }
}
