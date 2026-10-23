//! `/backups/` page: task CRUD, manual runs, artifact listing, download,
//! verification and deletion.
//!
//! A manual run inserts the `backup_run` row synchronously and then executes in
//! a background thread — unlike the Flask app, where the row appeared only once
//! the thread got scheduled, which made "did my click do anything?" hard to
//! answer.

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde::Deserialize;
use tokio_util::io::ReaderStream;

use crate::collect::backups as files;
use crate::config::Config;
use crate::db::repo::audit;
use crate::db::repo::backup::{self as repo, TaskInput};
use crate::db::{fmt_ms, fmt_ms_opt};
use crate::error::AppError;
use crate::runners::backup as runner;
use crate::templates::{
    BackupsPage, FileGroup, FileRow, KindOption, RunGroup, BackupRunRow as RunRow, TaskGroup, TaskRow,
};
use crate::web::render;
use crate::web::session::Session;
use crate::web::AppState;

const GROUP_ORDER: [&str; 4] = ["immich", "nextcloud", "stalwart", "other"];
const FILE_GROUP_ORDER: [&str; 5] = ["immich", "nextcloud", "stalwart", "logs", "other"];

pub async fn index(State(state): State<AppState>, session: Session) -> Result<Response, AppError> {
    session.require_uid()?;

    let tasks = repo::list_tasks(&state.db)?;
    let runs = repo::recent_runs(&state.db, 50)?;
    let root = state.cfg.backup_root.clone();
    let listed = tokio::task::spawn_blocking(move || files::list(&root))
        .await
        .map_err(|e| AppError::internal("backup listing", e))?;

    Ok(render(BackupsPage {
        nav: "backups",
        csrf: session.csrf(),
        flashes: session.flashes(),
        task_groups: task_groups(&tasks),
        run_groups: run_groups(&runs, &tasks),
        file_groups: file_groups(&listed),
        kind_options: kind_options(""),
    }))
}

#[derive(Deserialize)]
pub struct TaskForm {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub cron: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub enabled: Option<String>,
    #[serde(default)]
    pub retention_days: Option<String>,
    #[serde(default)]
    pub csrf_token: Option<String>,
}

/// Validated, owned task fields (the repo wants borrowed ones).
struct TaskValues {
    name: String,
    kind: String,
    cron: Option<String>,
    command: Option<String>,
    enabled: bool,
    retention_days: i64,
}

impl TaskValues {
    fn input(&self) -> TaskInput<'_> {
        TaskInput {
            name: &self.name,
            kind: &self.kind,
            cron: self.cron.as_deref(),
            command: self.command.as_deref(),
            enabled: self.enabled,
            retention_days: self.retention_days,
        }
    }
}

impl TaskForm {
    fn validate(&self) -> Result<TaskValues, String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("Task name is required".into());
        }
        let kind = self.kind.trim();
        if !repo::valid_kind(kind) {
            return Err(format!("Unknown task kind: {kind}"));
        }
        let cron = self
            .cron
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if let Some(expression) = cron.as_deref() {
            crate::sched::parse_cron(expression)
                .map_err(|e| format!("Invalid cron '{expression}': {e}"))?;
        }
        let command = self
            .command
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if kind == "custom" && command.is_none() {
            return Err("A custom task needs a command".into());
        }
        let retention_days = match self.retention_days.as_deref().map(str::trim) {
            Some(value) if !value.is_empty() => value
                .parse::<i64>()
                .map_err(|_| "retention_days must be a number".to_string())?,
            _ => 14,
        };
        if !(1..=3650).contains(&retention_days) {
            return Err("retention_days must be between 1 and 3650".into());
        }

        Ok(TaskValues {
            name: name.to_string(),
            kind: kind.to_string(),
            cron,
            command,
            enabled: self.enabled.is_some(),
            retention_days,
        })
    }
}

pub async fn create_task(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<TaskForm>,
) -> Result<Response, AppError> {
    let uid = session.require_uid()?;
    session.verify_csrf(form.csrf_token.as_deref())?;

    let values = match form.validate() {
        Ok(values) => values,
        Err(message) => return Ok(back(&session, "danger", message)),
    };
    let id = repo::create_task(&state.db, &values.input())?;
    audit::record(
        &state.db,
        &format!("user:{uid}"),
        "backup_task.create",
        Some(&values.name),
        Some(&format!("id={id} kind={}", values.kind)),
    );
    Ok(back(&session, "success", "Task created"))
}

pub async fn update_task(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<i64>,
    session: Session,
    Form(form): Form<TaskForm>,
) -> Result<Response, AppError> {
    let uid = session.require_uid()?;
    session.verify_csrf(form.csrf_token.as_deref())?;

    if repo::get_task(&state.db, id)?.is_none() {
        return Err(AppError::NotFound);
    }
    let values = match form.validate() {
        Ok(values) => values,
        Err(message) => return Ok(back(&session, "danger", message)),
    };
    repo::update_task(&state.db, id, &values.input())?;
    audit::record(
        &state.db,
        &format!("user:{uid}"),
        "backup_task.update",
        Some(&values.name),
        Some(&format!("id={id} kind={}", values.kind)),
    );
    Ok(back(&session, "success", "Task updated"))
}

pub async fn delete_task(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<i64>,
    session: Session,
    Form(form): Form<CsrfForm>,
) -> Result<Response, AppError> {
    let uid = session.require_uid()?;
    session.verify_csrf(form.csrf_token.as_deref())?;

    let Some(task) = repo::get_task(&state.db, id)? else {
        return Err(AppError::NotFound);
    };
    repo::delete_task(&state.db, id)?;
    audit::record(
        &state.db,
        &format!("user:{uid}"),
        "backup_task.delete",
        Some(&task.name),
        None,
    );
    Ok(back(&session, "success", "Task deleted"))
}

pub async fn run_task(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<i64>,
    session: Session,
    Form(form): Form<CsrfForm>,
) -> Result<Response, AppError> {
    let uid = session.require_uid()?;
    session.verify_csrf(form.csrf_token.as_deref())?;

    let Some(task) = repo::get_task(&state.db, id)? else {
        return Err(AppError::NotFound);
    };
    let run_id = repo::create_run(&state.db, id)?;
    audit::record(
        &state.db,
        &format!("user:{uid}"),
        "backup_task.run",
        Some(&task.name),
        Some(&format!("run={run_id}")),
    );

    let db = state.db.clone();
    let cfg: Arc<Config> = state.cfg.clone();
    std::thread::spawn(move || {
        if let Err(e) = runner::execute(&db, &cfg, run_id) {
            tracing::error!("backup run {run_id} failed to execute: {e}");
        }
    });

    Ok(back(
        &session,
        "info",
        format!("Task '{}' started (run {run_id})", task.name),
    ))
}

#[derive(Deserialize)]
pub struct RelQuery {
    #[serde(default)]
    pub rel: String,
}

#[derive(Deserialize)]
pub struct CsrfForm {
    #[serde(default)]
    pub rel: Option<String>,
    #[serde(default)]
    pub csrf_token: Option<String>,
}

pub async fn download(
    State(state): State<AppState>,
    session: Session,
    Query(query): Query<RelQuery>,
) -> Result<Response, AppError> {
    session.require_uid()?;

    let path = files::safe_join(&state.cfg.backup_root, &query.rel)
        .filter(|path| path.is_file())
        .ok_or(AppError::NotFound)?;

    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| AppError::internal(&format!("open {}", path.display()), e))?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".into());
    let disposition = format!(
        "attachment; filename*=UTF-8''{}",
        utf8_percent_encode(&name, NON_ALPHANUMERIC)
    );

    let body = Body::from_stream(ReaderStream::new(file));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_DISPOSITION, disposition)
        .body(body)
        .map_err(|e| AppError::internal("download response", e))
}

pub async fn delete_file(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<CsrfForm>,
) -> Result<Response, AppError> {
    let uid = session.require_uid()?;
    session.verify_csrf(form.csrf_token.as_deref())?;

    let rel = form.rel.unwrap_or_default();
    let path = files::safe_join(&state.cfg.backup_root, &rel)
        .filter(|path| path.is_file())
        .ok_or(AppError::NotFound)?;
    std::fs::remove_file(&path)?;
    audit::record(
        &state.db,
        &format!("user:{uid}"),
        "backup_file.delete",
        Some(&rel),
        None,
    );
    Ok(back(&session, "success", format!("Deleted {rel}")))
}

pub async fn verify_file(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<CsrfForm>,
) -> Result<Response, AppError> {
    session.require_uid()?;
    session.verify_csrf(form.csrf_token.as_deref())?;

    let rel = form.rel.unwrap_or_default();
    let path = files::safe_join(&state.cfg.backup_root, &rel)
        .filter(|path| path.is_file())
        .ok_or(AppError::NotFound)?;
    if path.extension().and_then(|ext| ext.to_str()) != Some("dump") {
        return Ok(back(&session, "info", "Verify is only supported for .dump files"));
    }

    let check = path.clone();
    let result = tokio::task::spawn_blocking(move || {
        crate::exec::run(
            &["pg_restore", "--list", &check.to_string_lossy()],
            None,
            None,
            std::time::Duration::from_secs(300),
        )
    })
    .await
    .map_err(|e| AppError::internal("pg_restore", e))?;

    if result.ok() {
        Ok(back(&session, "success", format!("Verify OK: {rel}")))
    } else {
        let detail = tail_chars(&result.trimmed(), 300);
        Ok(back(&session, "danger", format!("Verify failed: {detail}")))
    }
}

/// Last `limit` characters — log tails are cut from the end, never by bytes
/// (pg_restore output can contain non-ASCII object names).
fn tail_chars(text: &str, limit: usize) -> String {
    let mut chars: Vec<char> = text.chars().rev().take(limit).collect();
    chars.reverse();
    chars.into_iter().collect()
}

fn back(session: &Session, class: &str, message: impl Into<String>) -> Response {
    session.flash(class, message);
    Redirect::to("/backups/").into_response()
}

// ---------------------------------------------------------------------------
// View models
// ---------------------------------------------------------------------------

fn kind_group(kind: &str) -> &'static str {
    match kind {
        "immich_db" | "immich_config" => "immich",
        "nextcloud_db" | "nextcloud_config" => "nextcloud",
        "stalwart_db" | "stalwart_config" => "stalwart",
        _ => "other",
    }
}

fn task_groups(tasks: &[repo::BackupTask]) -> Vec<TaskGroup> {
    GROUP_ORDER
        .iter()
        .filter_map(|group| {
            let rows: Vec<TaskRow> = tasks
                .iter()
                .filter(|task| kind_group(&task.kind) == *group)
                .map(|task| TaskRow {
                    id: task.id,
                    name: task.name.clone(),
                    kind: task.kind.clone(),
                    cron: task.cron.clone().unwrap_or_else(|| "manual".into()),
                    enabled: task.enabled,
                    retention_days: task.retention_days,
                    command: task.command.clone().unwrap_or_default(),
                    last_run: task.last_run_at.map(fmt_ms).unwrap_or_else(|| "—".into()),
                    last_status: task.last_status.clone().unwrap_or_else(|| "—".into()),
                    kind_options: kind_options(&task.kind),
                })
                .collect();
            if rows.is_empty() {
                return None;
            }
            Some(TaskGroup {
                label: group.to_uppercase(),
                count: rows.len(),
                tasks: rows,
            })
        })
        .collect()
}

fn run_groups(runs: &[repo::BackupRun], tasks: &[repo::BackupTask]) -> Vec<RunGroup> {
    GROUP_ORDER
        .iter()
        .filter_map(|group| {
            let rows: Vec<RunRow> = runs
                .iter()
                .filter(|run| {
                    let kind = run
                        .task_id
                        .and_then(|id| tasks.iter().find(|task| task.id == id))
                        .map(|task| task.kind.as_str())
                        .unwrap_or("custom");
                    kind_group(kind) == *group
                })
                .map(|run| RunRow {
                    id: run.id,
                    task: run
                        .task_id
                        .and_then(|id| tasks.iter().find(|task| task.id == id))
                        .map(|task| task.name.clone())
                        .unwrap_or_else(|| {
                            run.task_id
                                .map(|id| format!("#{id} (deleted)"))
                                .unwrap_or_else(|| "—".into())
                        }),
                    status: run.status.clone(),
                    badge: match run.status.as_str() {
                        "success" => "active",
                        "failed" => "inactive",
                        _ => "unknown",
                    },
                    started: fmt_ms(run.started_at),
                    files: run.files.clone().unwrap_or_default(),
                })
                .collect();
            if rows.is_empty() {
                return None;
            }
            Some(RunGroup {
                label: group.to_uppercase(),
                count: rows.len(),
                runs: rows,
            })
        })
        .collect()
}

fn file_groups(listed: &[files::BackupFile]) -> Vec<FileGroup> {
    let group_of = |rel: &str| -> &'static str {
        match rel.split_once('/').map(|(head, _)| head) {
            Some("immich") => "immich",
            Some("nextcloud") => "nextcloud",
            Some("stalwart") => "stalwart",
            Some("logs") => "logs",
            _ => "other",
        }
    };

    FILE_GROUP_ORDER
        .iter()
        .filter_map(|group| {
            let rows: Vec<FileRow> = listed
                .iter()
                .filter(|file| group_of(&file.rel) == *group)
                .map(|file| FileRow {
                    rel: file.rel.clone(),
                    size: format!("{:.2} MB", file.size as f64 / (1024.0 * 1024.0)),
                    mtime: fmt_ms(file.mtime_ms),
                    is_dump: file.name.ends_with(".dump"),
                })
                .collect();
            if rows.is_empty() {
                return None;
            }
            Some(FileGroup {
                label: group.to_uppercase(),
                count: rows.len(),
                files: rows,
            })
        })
        .collect()
}

fn kind_options(selected: &str) -> Vec<KindOption> {
    repo::KINDS
        .iter()
        .map(|kind| KindOption {
            value: kind,
            selected: *kind == selected,
        })
        .collect()
}

/// Kept for unit tests: the grouping rules are the part most likely to drift.
fn group_of_kind(kind: &str) -> &'static str {
    kind_group(kind)
}

/// Path helper re-exported for tests.
fn is_inside(root: &Path, rel: &str) -> bool {
    files::safe_join(root, rel).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_groups_cover_every_kind() {
        assert_eq!(group_of_kind("immich_db"), "immich");
        assert_eq!(group_of_kind("immich_config"), "immich");
        assert_eq!(group_of_kind("nextcloud_db"), "nextcloud");
        assert_eq!(group_of_kind("stalwart_config"), "stalwart");
        assert_eq!(group_of_kind("all"), "other");
        assert_eq!(group_of_kind("custom"), "other");
        for kind in repo::KINDS {
            assert!(GROUP_ORDER.contains(&group_of_kind(kind)));
        }
    }

    fn form(kind: &str) -> TaskForm {
        TaskForm {
            name: "nightly".into(),
            kind: kind.into(),
            cron: Some("0 4 * * *".into()),
            command: None,
            enabled: Some("on".into()),
            retention_days: Some("14".into()),
            csrf_token: None,
        }
    }

    #[test]
    fn task_form_validates() {
        let values = form("immich_db").validate().unwrap();
        assert!(values.enabled);
        assert_eq!(values.retention_days, 14);

        let mut bad = form("immich_db");
        bad.name = "  ".into();
        assert!(bad.validate().is_err());

        let mut bad = form("bogus");
        bad.name = "x".into();
        assert!(bad.validate().is_err());

        let mut bad = form("custom");
        bad.name = "x".into();
        assert!(bad.validate().is_err(), "custom needs a command");
        bad.command = Some("echo hi".into());
        assert!(bad.validate().is_ok());

        let mut bad = form("immich_db");
        bad.cron = Some("not a cron".into());
        assert!(bad.validate().is_err());

        let mut bad = form("immich_db");
        bad.retention_days = Some("0".into());
        assert!(bad.validate().is_err());

        let mut ok = form("immich_db");
        ok.cron = Some("  ".into());
        assert_eq!(ok.validate().unwrap().cron, None);
        assert!(!form("immich_db").validate().unwrap().cron.is_none());
    }

    #[test]
    fn checkbox_absence_means_disabled() {
        let mut form = form("immich_db");
        form.enabled = None;
        assert!(!form.validate().unwrap().enabled);
    }

    #[test]
    fn path_guard_is_delegated_to_the_collector() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x.dump"), b"x").unwrap();
        assert!(is_inside(dir.path(), "x.dump"));
        assert!(!is_inside(dir.path(), "../x.dump"));
    }
}
