//! `/scheduler/` page: custom cron jobs, systemd timers, and unit file viewer.

use std::path::Path;
use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use serde::Deserialize;
use serde_json::json;
use tokio_util::io::ReaderStream;

use crate::collect::services as svc;
use crate::config::Config;
use crate::db::repo::audit;
use crate::db::repo::scheduler as repo;
use crate::db::{fmt_ms, fmt_ms_opt};
use crate::error::AppError;
use crate::runners::scheduler as sched;
use crate::templates::{
    SchedulerPage, JobRow, TimerRow, APSJobRow,
};
use crate::web::render;
use crate::web::session::Session;
use crate::web::AppState;

pub async fn index(State(state): State<AppState>, session: Session) -> Result<Response, AppError> {
    session.require_uid()?;

    let jobs = repo::list_jobs(&state.db)?;
    let timers = tokio::task::spawn_blocking(move || svc::list_timers())
        .await
        .map_err(|e| AppError::internal("list timers", e))?;

    // For APScheduler jobs, we'd need to query the scheduler state
    // For now, just show the croner-based jobs
    let aps_jobs: Vec<APSJobRow> = Vec::new();

    Ok(render(SchedulerPage {
        nav: "scheduler",
        csrf: session.csrf(),
        flashes: session.flashes(),
        jobs: jobs.into_iter().map(|j| JobRow {
            id: j.id,
            name: j.name,
            cron: j.cron.unwrap_or_else(|| "—".into()),
            enabled: j.enabled,
            last_run: j.last_run_at.map(fmt_ms).unwrap_or_else(|| "—".into()),
            last_status: j.last_status.unwrap_or_else(|| "—".into()),
        }).collect(),
        timers: timers.into_iter().map(|t| TimerRow {
            scope: t.scope,
            unit: t.unit,
            raw: t.raw,
        }).collect(),
        aps_jobs,
    }))
}

#[derive(Deserialize)]
pub struct JobForm {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub cron: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub enabled: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub csrf_token: Option<String>,
}

pub async fn create_job(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<JobForm>,
) -> Result<Response, AppError> {
    let uid = session.require_uid()?;
    session.verify_csrf(form.csrf_token.as_deref())?;

    let name = form.name.trim();
    if name.is_empty() {
        return Ok(back(&session, "danger", "Job name is required"));
    }

    let cron = form
        .cron
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    if let Some(ref expr) = cron {
        crate::sched::parse_cron(expr)
            .map_err(|e| AppError::BadRequest(format!("Invalid cron: {e}")))?;
    }

    let command = form
        .command
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    if cron.is_some() && command.is_none() {
        return Ok(back(&session, "danger", "A cron job needs a command"));
    }

    let input = repo::JobInput {
        name,
        job_type: "custom_shell",
        target: None,
        cron: cron.as_deref(),
        command: command.as_deref(),
        enabled: form.enabled.is_some(),
        notes: form.notes.as_deref(),
    };
    let id = repo::create_job(&state.db, &input)?;

    audit::record(
        &state.db,
        &format!("user:{uid}"),
        "scheduled_job.create",
        Some(name),
        Some(&format!("id={id} cron={:?}", cron)),
    );

    // If enabled with cron, register with the scheduler
    if input.enabled && input.cron.is_some() {
        let _ = sched::register_job(&state.db, &state.cfg, id);
    }

    Ok(back(&session, "success", "Job created"))
}

pub async fn delete_job(
    State(state): State<AppState>,
    session: Session,
    AxumPath(id): AxumPath<i64>,
    Form(form): Form<CsrfForm>,
) -> Result<Response, AppError> {
    let uid = session.require_uid()?;
    session.verify_csrf(form.csrf_token.as_deref())?;

    let job = repo::get_job(&state.db, id)?;
    if job.is_none() {
        return Err(AppError::NotFound);
    }
    let name = job.unwrap().name;

    repo::delete_job(&state.db, id)?;
    audit::record(
        &state.db,
        &format!("user:{uid}"),
        "scheduled_job.delete",
        Some(&name),
        None,
    );

    Ok(back(&session, "success", "Job deleted"))
}

pub async fn run_job(
    State(state): State<AppState>,
    session: Session,
    AxumPath(id): AxumPath<i64>,
    Form(form): Form<CsrfForm>,
) -> Result<Response, AppError> {
    let uid = session.require_uid()?;
    session.verify_csrf(form.csrf_token.as_deref())?;

    let _ = sched::trigger_job(&state.db, &state.cfg, id)?;
    audit::record(
        &state.db,
        &format!("user:{uid}"),
        "scheduled_job.manual_run",
        None,
        Some(&id.to_string()),
    );

    Ok(back(&session, "info", "Job triggered"))
}

#[derive(Deserialize)]
pub struct TimerForm {
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub csrf_token: Option<String>,
}

pub async fn toggle_timer(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<TimerForm>,
) -> Result<Response, AppError> {
    let uid = session.require_uid()?;
    session.verify_csrf(form.csrf_token.as_deref())?;

    let (ok, msg) = svc::manage_one(&form.unit, &form.scope, &form.action)
        .map_err(AppError::BadRequest)?;

    audit::record(
        &state.db,
        &format!("user:{uid}"),
        &format!("timer.{}", form.action),
        Some(&form.unit),
        Some(&msg),
    );

    Ok(back(&session, if ok { "success" } else { "danger" }, format!("{}: {}", form.unit, msg)))
}

pub async fn unit_text(
    State(state): State<AppState>,
    session: Session,
    AxumPath((scope, unit)): AxumPath<(String, String)>,
) -> Result<axum::Json<serde_json::Value>, AppError> {
    session.require_uid()?;

    if !svc::validate_unit(&unit) {
        return Ok(axum::Json(json!({"ok": false, "error": "недопустимое имя юнита"})));
    }
    if scope != "user" && scope != "system" {
        return Ok(axum::Json(json!({"ok": false, "error": "недопустимый scope"})));
    }

    let base = if scope == "user" {
        vec!["systemctl", "--user", "cat"]
    } else {
        vec!["sudo", "systemctl", "cat"]
    };
    let wanted = if unit.ends_with(".timer") {
        vec![unit.clone(), unit.trim_end_matches(".timer").to_string() + ".service"]
    } else {
        vec![unit.clone()]
    };

    let mut blocks = Vec::new();
    for name in wanted {
        let mut argv = base.clone();
        argv.push(name.as_str());
        let out = crate::exec::run(&argv, None, None, std::time::Duration::from_secs(20));
        let text = out.combined().trim().to_string();
        if out.ok() && !text.is_empty() {
            blocks.push(text);
        }
    }

    if blocks.is_empty() {
        return Ok(axum::Json(json!({"ok": false, "error": format!("файл юнита не найден: {}", unit)})));
    }
    Ok(axum::Json(json!({"ok": true, "unit": unit, "text": blocks.join("\n\n")})))
}

pub async fn timer_list(
    session: Session,
) -> Result<axum::Json<serde_json::Value>, AppError> {
    session.require_uid()?;

    let timers = tokio::task::spawn_blocking(move || svc::list_timers())
        .await
        .map_err(|e| AppError::internal("list timers", e))?;

    let timers: Vec<serde_json::Value> = timers.into_iter().map(|t| json!({
        "scope": t.scope,
        "unit": t.unit,
        "raw": t.raw,
    })).collect();

    Ok(axum::Json(json!({"timers": timers})))
}

#[derive(Deserialize)]
pub struct CsrfForm {
    #[serde(default)]
    pub csrf_token: Option<String>,
}

fn back(session: &Session, class: &str, message: impl Into<String>) -> Response {
    session.flash(class, message);
    Redirect::to("/scheduler/").into_response()
}

use axum::body::Body;