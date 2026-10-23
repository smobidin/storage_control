//! `/updates/` page: version availability, running updates, and preserves.

use std::path::Path;
use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use serde::Deserialize;
use tokio_util::io::ReaderStream;

use crate::config::Config;
use crate::db::repo::audit;
use crate::db::repo::update as repo;
use crate::db::{fmt_ms, fmt_ms_opt};
use crate::error::AppError;
use crate::runners::{preserve, update_immich, update_nextcloud, update_stalwart};
use crate::templates::{
    PreserveGroup, PreserveRow, UpdateRunRow, UpdateGroup, UpdatesPage,
};
use crate::updates::versions::{add_candidate, immich_current, immich_release_info, immich_tags, nextcloud_current, nextcloud_release_info, nextcloud_tags, stalwart_current, stalwart_release_info, stalwart_tags};
use crate::web::render;
use crate::web::session::Session;
use crate::web::AppState;

pub async fn index(State(state): State<AppState>, session: Session) -> Result<Response, AppError> {
    session.require_uid()?;

    let runs = repo::recent_runs(&state.db, 30)?;
    let preserves = repo::list_preserves(&state.db, None, 50)?;

    let ic = immich_current(&state.cfg);
    let it = immich_tags(&state.cfg, 30);
    let nc = nextcloud_current(&state.cfg);
    let nt = nextcloud_tags(&state.cfg, 30);
    let sc = stalwart_current(&state.cfg);
    let st = stalwart_tags(&state.cfg, 30);

    let immich_info = immich_release_info(&state.cfg);
    let nextcloud_info = nextcloud_release_info(&state.cfg);
    let stalwart_info = stalwart_release_info(&state.cfg);

    Ok(render(UpdatesPage {
        nav: "updates",
        csrf: session.csrf(),
        flashes: session.flashes(),
    }))
}

#[derive(Deserialize)]
pub struct RunForm {
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub csrf_token: Option<String>,
}

pub async fn run_immich(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<RunForm>,
) -> Result<Response, AppError> {
    run_update(state, session, form, "immich".to_string(), update_immich::run_immich_update).await
}

pub async fn run_nextcloud(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<RunForm>,
) -> Result<Response, AppError> {
    run_update(state, session, form, "nextcloud".to_string(), update_nextcloud::run_nextcloud_update).await
}

pub async fn run_stalwart(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<RunForm>,
) -> Result<Response, AppError> {
    run_update(state, session, form, "stalwart".to_string(), update_stalwart::run_stalwart_update).await
}

async fn run_update<F>(
    state: AppState,
    session: Session,
    form: RunForm,
    target: String,
    runner: F,
) -> Result<Response, AppError>
where
    F: FnOnce(&Config, i64) -> Result<(), AppError> + Send + 'static,
{
    let uid = session.require_uid()?;
    session.verify_csrf(form.csrf_token.as_deref())?;

    let tag = form.tag.trim();
    if tag.is_empty() {
        return Ok(back(&session, "danger", "tag required"));
    }

    let current_fn = match target.as_str() {
        "immich" => immich_current,
        "nextcloud" => nextcloud_current,
        "stalwart" => stalwart_current,
        _ => return Ok(back(&session, "danger", "unknown target")),
    };
    let from_version = current_fn(&state.cfg);

    let log_path = state.cfg.runs_dir.join(format!("update_{target}_{run_id}.log", run_id = crate::db::now_ms()));
    let _ = std::fs::create_dir_all(&log_path.parent().unwrap());

    let run_id = repo::create_run(&state.db, &target, tag, &from_version, &log_path.to_string_lossy())?;

    audit::record(
        &state.db,
        &format!("user:{uid}"),
        &format!("update.{target}"),
        Some(tag),
        Some("started"),
    );

    let db = state.db.clone();
    let cfg: Arc<Config> = state.cfg.clone();
    let target_owned = target.clone();
    std::thread::spawn(move || {
        if let Err(e) = runner(&cfg, run_id) {
            tracing::error!("update {target_owned} run {run_id} failed: {e}");
        }
    });

    Ok(back(&session, "info", format!("{target} update started: {tag}")))
}

#[derive(Deserialize)]
pub struct PreserveForm {
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub csrf_token: Option<String>,
}

pub async fn prepare_preserve(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<PreserveForm>,
) -> Result<Response, AppError> {
    let uid = session.require_uid()?;
    session.verify_csrf(form.csrf_token.as_deref())?;

    let target = form.target.trim();
    if !matches!(target, "immich" | "nextcloud" | "stalwart") {
        return Ok(back(&session, "danger", "invalid target"));
    }

    let log_path = state.cfg.runs_dir.join(format!("prepare_{target}_preserve_{}.log", crate::db::now_ms()));
    let storage_dir = state.cfg.backup_root.join("preserves").join(target).join(crate::db::now_ms().to_string());
    let _ = std::fs::create_dir_all(&storage_dir);

    let preserve_id = repo::create_preserve(&state.db, target, &log_path.to_string_lossy(), &storage_dir.to_string_lossy())?;

    audit::record(
        &state.db,
        &format!("user:{uid}"),
        "update.prepare",
        Some(target),
        Some(&preserve_id.to_string()),
    );

    let db = state.db.clone();
    let cfg: Arc<Config> = state.cfg.clone();
    std::thread::spawn(move || {
        if let Err(e) = preserve::prepare_preserve(&cfg, preserve_id) {
            tracing::error!("prepare preserve {preserve_id} failed: {e}");
        }
    });

    Ok(back(&session, "info", format!("Prepare started: {target}")))
}

pub async fn restore_preserve(
    State(state): State<AppState>,
    session: Session,
    AxumPath(id): AxumPath<i64>,
    Form(form): Form<CsrfForm>,
) -> Result<Response, AppError> {
    let uid = session.require_uid()?;
    session.verify_csrf(form.csrf_token.as_deref())?;

    let Some(preserve) = repo::get_preserve(&state.db, id)? else {
        return Err(AppError::NotFound);
    };

    audit::record(
        &state.db,
        &format!("user:{uid}"),
        "update.restore",
        Some(&preserve.target),
        Some(&id.to_string()),
    );

    let db = state.db.clone();
    let cfg: Arc<Config> = state.cfg.clone();
    std::thread::spawn(move || {
        if let Err(e) = preserve::restore_preserve(&cfg, id) {
            tracing::error!("restore preserve {id} failed: {e}");
        }
    });

    Ok(back(&session, "info", format!("Restore started for preserve {id}")))
}

pub async fn delete_preserve(
    State(state): State<AppState>,
    session: Session,
    AxumPath(id): AxumPath<i64>,
    Form(form): Form<CsrfForm>,
) -> Result<Response, AppError> {
    let uid = session.require_uid()?;
    session.verify_csrf(form.csrf_token.as_deref())?;

    preserve::delete_preserve(&state.cfg, id)?;
    audit::record(
        &state.db,
        &format!("user:{uid}"),
        "update.preserve.delete",
        None,
        Some(&id.to_string()),
    );
    Ok(back(&session, "success", format!("Preserve {id} deleted")))
}

pub async fn run_log(
    State(state): State<AppState>,
    session: Session,
    AxumPath(id): AxumPath<i64>,
) -> Result<Response, AppError> {
    session.require_uid()?;

    let run = repo::get_run(&state.db, id)?.ok_or(AppError::NotFound)?;
    let log_path = run.log_path.ok_or(AppError::NotFound)?;
    let path = Path::new(&log_path);
    if !path.is_file() {
        return Err(AppError::NotFound);
    }

    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| AppError::internal(&format!("open {}", path.display()), e))?;
    let body = Body::from_stream(ReaderStream::new(file));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(body)
        .map_err(|e| AppError::internal("log response", e))
}

pub async fn run_status(
    State(state): State<AppState>,
    session: Session,
    AxumPath(id): AxumPath<i64>,
) -> Result<axum::Json<serde_json::Value>, AppError> {
    session.require_uid()?;

    let run = repo::get_run(&state.db, id)?.ok_or(AppError::NotFound)?;
    let log_tail = run.log_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|text| {
            let mut chars: Vec<char> = text.chars().rev().take(4000).collect();
            chars.reverse();
            chars.into_iter().collect::<String>()
        })
        .unwrap_or_default();

    let steps: Vec<serde_json::Value> = run.steps
        .as_ref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    Ok(axum::Json(serde_json::json!({
        "id": run.id,
        "status": run.status,
        "steps": steps,
        "log_tail": log_tail,
        "finished_at": run.finished_at.map(fmt_ms).unwrap_or_default(),
    })))
}

pub async fn preserve_log(
    State(state): State<AppState>,
    session: Session,
    AxumPath(id): AxumPath<i64>,
) -> Result<Response, AppError> {
    session.require_uid()?;

    let preserve = repo::get_preserve(&state.db, id)?.ok_or(AppError::NotFound)?;
    let log_path = preserve.log_path.ok_or(AppError::NotFound)?;
    let path = Path::new(&log_path);
    if !path.is_file() {
        return Err(AppError::NotFound);
    }

    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| AppError::internal(&format!("open {}", path.display()), e))?;
    let body = Body::from_stream(ReaderStream::new(file));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(body)
        .map_err(|e| AppError::internal("log response", e))
}

pub async fn preserve_status(
    State(state): State<AppState>,
    session: Session,
    AxumPath(id): AxumPath<i64>,
) -> Result<axum::Json<serde_json::Value>, AppError> {
    session.require_uid()?;

    let preserve = repo::get_preserve(&state.db, id)?.ok_or(AppError::NotFound)?;
    let log_tail = preserve.log_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|text| {
            let mut chars: Vec<char> = text.chars().rev().take(4000).collect();
            chars.reverse();
            chars.into_iter().collect::<String>()
        })
        .unwrap_or_default();

    let steps: Vec<serde_json::Value> = preserve.steps
        .as_ref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    Ok(axum::Json(serde_json::json!({
        "id": preserve.id,
        "status": preserve.status,
        "steps": steps,
        "log_tail": log_tail,
        "target": preserve.target,
        "finished_at": preserve.finished_at.map(fmt_ms).unwrap_or_default(),
    })))
}

#[derive(Deserialize)]
pub struct AvailabilityQuery {
    #[serde(default)]
    pub force: bool,
}

pub async fn api_availability(
    State(state): State<AppState>,
    session: Session,
    Query(_query): Query<AvailabilityQuery>,
) -> Result<axum::Json<serde_json::Value>, AppError> {
    session.require_uid()?;

    let ic = immich_current(&state.cfg);
    let nc = nextcloud_current(&state.cfg);
    let sc = stalwart_current(&state.cfg);

    let i = immich_release_info(&state.cfg);
    let n = nextcloud_release_info(&state.cfg);
    let s = stalwart_release_info(&state.cfg);

    Ok(axum::Json(serde_json::json!({
        "immich": {
            "current": i.current,
            "latest": i.latest,
            "available": i.available,
            "error": i.error,
            "url": i.url,
        },
        "nextcloud": {
            "current": n.current,
            "latest": n.latest,
            "available": n.available,
            "error": n.error,
            "url": n.url,
        },
        "stalwart": {
            "current": s.current,
            "latest": s.latest,
            "available": s.available,
            "error": s.error,
            "url": s.url,
        },
    })))
}

#[derive(Deserialize)]
pub struct CsrfForm {
    #[serde(default)]
    pub csrf_token: Option<String>,
}

fn back(session: &Session, class: &str, message: impl Into<String>) -> Response {
    session.flash(class, message);
    Redirect::to("/updates/").into_response()
}

fn preserve_groups(preserves: &[repo::Preserve]) -> Vec<PreserveGroup> {
    let mut groups = std::collections::HashMap::new();
    for p in preserves {
        groups.entry(p.target.clone()).or_insert_with(Vec::new).push(p);
    }
    ["immich", "nextcloud", "stalwart"]
        .iter()
        .filter_map(|target| groups.get(*target).map(|ps| PreserveGroup {
            label: target.to_uppercase(),
            count: ps.len(),
            preserves: ps.iter().map(|p| PreserveRow {
                id: p.id,
                target: p.target.clone(),
                from_version: p.from_version.clone().unwrap_or_else(|| "-".into()),
                status: p.status.clone(),
                badge: match p.status.as_str() {
                    "ready" | "restored" => "active",
                    "failed" => "inactive",
                    _ => "unknown",
                },
                created: fmt_ms(p.created_at),
                finished: p.finished_at.map(fmt_ms).unwrap_or_else(|| "—".into()),
            }).collect(),
        }))
        .collect()
}



use axum::body::Body;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserve_groups_only_known_targets() {
        let db = crate::db::Db::open_in_memory().unwrap();
        let _ = crate::db::repo::update::create_preserve(&db, "immich", "/tmp/log", "/mnt/backup/preserves/immich/1").unwrap();
        let _ = crate::db::repo::update::create_preserve(&db, "nextcloud", "/tmp/log", "/mnt/backup/preserves/nextcloud/1").unwrap();
        let _ = crate::db::repo::update::create_preserve(&db, "stalwart", "/tmp/log", "/mnt/backup/preserves/stalwart/1").unwrap();

        let preserves = repo::list_preserves(&db, None, 50).unwrap();
        let groups = preserve_groups(&preserves);
        let labels: Vec<&str> = groups.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(labels, vec!["IMMICH", "NEXTCLOUD", "STALWART"]);
    }
}