//! Dashboard page and the `/api/health/*` endpoints behind it.
//!
//! The Flask app kept these routes in `dashboard/routes.py` because the page
//! JavaScript calls them; the same grouping is kept here for the same reason.
//! Every collector shells out or touches /proc, so it runs in `spawn_blocking`.

use std::sync::Mutex;

use axum::extract::{Path, State};
use axum::response::Response;
use axum::Json;
use serde_json::{json, Value};
use sysinfo::System;

use crate::collect::{raid, services, smart, system};
use crate::db::repo;
use crate::db::{now_ms, Db};
use crate::error::AppError;
use crate::runners::smart_monitor::{self, Source};
use crate::templates::{DashboardPage, HelpPage};
use crate::web::session::Session;
use crate::web::{render, AppState};

pub async fn index(session: Session) -> Result<Response, AppError> {
    session.require_uid()?;
    Ok(render(DashboardPage {
        nav: "dashboard",
        csrf: session.csrf(),
        flashes: session.flashes(),
    }))
}

/// `/help` renders the repository README, like the Flask app did.
pub async fn help(State(state): State<AppState>, session: Session) -> Result<Response, AppError> {
    session.require_uid()?;

    let path = state.cfg.repo_root.join("README.md");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| AppError::internal(&format!("read {}", path.display()), e))?;

    let parser = pulldown_cmark::Parser::new_ext(&text, pulldown_cmark::Options::all());
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);

    Ok(render(HelpPage {
        nav: "help",
        csrf: session.csrf(),
        flashes: session.flashes(),
        readme_html: html,
    }))
}

pub async fn api_summary(
    State(state): State<AppState>,
    session: Session,
) -> Result<Json<Value>, AppError> {
    session.require_uid()?;

    let sys: std::sync::Arc<Mutex<System>> = state.sys.clone();
    let mut summary = tokio::task::spawn_blocking(move || system::summary(&sys))
        .await
        .map_err(|e| AppError::internal("system collector", e))?;
    let raid = tokio::task::spawn_blocking(raid::summary)
        .await
        .map_err(|e| AppError::internal("raid collector", e))?;

    summary["raid"] = raid;
    Ok(Json(summary))
}

/// Live SMART data, one `smartctl -a` per disk. The dashboard does not poll
/// this endpoint; it is there for manual inspection and the refresh action.
pub async fn api_smart(
    session: Session,
) -> Result<Json<Value>, AppError> {
    session.require_uid()?;
    let info = tokio::task::spawn_blocking(smart::summary)
        .await
        .map_err(|e| AppError::internal("smart collector", e))?;
    Ok(Json(serde_json::to_value(info).unwrap_or(Value::Null)))
}

pub async fn api_services(
    session: Session,
) -> Result<Json<Value>, AppError> {
    session.require_uid()?;
    let statuses = tokio::task::spawn_blocking(services::all_status)
        .await
        .map_err(|e| AppError::internal("services collector", e))?;
    Ok(Json(serde_json::to_value(statuses).unwrap_or(Value::Null)))
}

pub async fn api_smart_status(
    State(state): State<AppState>,
    session: Session,
) -> Result<Json<Value>, AppError> {
    session.require_uid()?;

    let states = repo::smart::list_states(&state.db)?;
    let alerts = repo::smart::active_alerts(&state.db)?;

    let states: Vec<Value> = states
        .iter()
        .map(|s| {
            json!({
                "device": s.device,
                "model": s.model,
                "serial": s.serial,
                "health": s.health,
                "reallocated": s.reallocated,
                "pending": s.pending,
                "offline_uncorrectable": s.offline_uncorrectable,
                "ata_error_count": s.ata_error_count,
                "temperature": s.temperature,
                "power_on_hours": s.power_on_hours,
                "first_seen": iso(Some(s.first_seen_at)),
                "last_checked": iso(Some(s.last_checked_at)),
                "baseline": baseline_value(s.resolved_counters.as_deref()),
            })
        })
        .collect();

    let alerts: Vec<Value> = alerts
        .iter()
        .map(|a| {
            json!({
                "id": a.id,
                "device": a.device,
                "severity": a.severity,
                "message": a.message,
                "created_at": iso(Some(a.created_at)),
            })
        })
        .collect();

    Ok(Json(json!({ "states": states, "alerts": alerts })))
}

/// Manual SMART refresh. CSRF-exempt in the Flask app (fetch POST, no token);
/// kept that way, still login-gated.
pub async fn api_smart_refresh(
    State(state): State<AppState>,
    session: Session,
) -> Result<Json<Value>, AppError> {
    let uid = session.require_uid()?;
    let db: Db = state.db.clone();
    let results = tokio::task::spawn_blocking(move || smart_monitor::run_check(&db, Source::Manual))
        .await
        .map_err(|e| AppError::internal("smart monitor", e))??;

    repo::audit::record(
        &state.db,
        &format!("user:{uid}"),
        "smart.refresh",
        None,
        None,
    );
    Ok(Json(json!({ "ok": true, "results": results })))
}

/// Accept the current counters as the new baseline and close the alert.
pub async fn api_smart_alert_resolve(
    State(state): State<AppState>,
    Path(alert_id): Path<i64>,
    session: Session,
) -> Result<Json<Value>, AppError> {
    let uid = session.require_uid()?;

    let alert = repo::smart::get_alert(&state.db, alert_id)?.ok_or(AppError::NotFound)?;
    let now = now_ms();
    repo::smart::resolve_alert(&state.db, alert_id, now)?;

    if let Some(disk) = repo::smart::find_state(&state.db, &alert.device)? {
        repo::smart::save_baseline(
            &state.db,
            &alert.device,
            disk.reallocated,
            disk.pending,
            disk.offline_uncorrectable,
            disk.ata_error_count,
        )?;
    }

    repo::audit::record(
        &state.db,
        &format!("user:{uid}"),
        "smart.alert.resolve",
        Some(&alert.device),
        Some(&alert.message),
    );

    Ok(Json(json!({ "ok": true, "resolved_at": iso(Some(now)) })))
}

pub async fn api_smart_history(
    State(state): State<AppState>,
    Path(device): Path<String>,
    session: Session,
) -> Result<Json<Value>, AppError> {
    session.require_uid()?;
    let rows = repo::smart::history(&state.db, &device, 100)?;
    let rows: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "checked_at": iso(Some(row.checked_at)),
                "reallocated": row.reallocated,
                "pending": row.pending,
                "offline_uncorrectable": row.offline_uncorrectable,
                "ata_error_count": row.ata_error_count,
                "temperature": row.temperature,
                "health": row.health,
                "smartctl_rc": row.smartctl_rc,
                "source": row.source,
            })
        })
        .collect();
    Ok(Json(Value::Array(rows)))
}

fn iso(ms: Option<i64>) -> Option<String> {
    let ms = ms?;
    chrono::DateTime::from_timestamp_millis(ms).map(|dt| dt.to_rfc3339())
}

fn baseline_value(raw: Option<&str>) -> Value {
    match raw {
        Some(text) => serde_json::from_str(text).unwrap_or(Value::Null),
        None => Value::Null,
    }
}
