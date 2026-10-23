pub mod routes;
pub mod session;

use std::sync::{Arc, Mutex};

use askama::Template;
use axum::extract::Path;
use axum::http::header;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{middleware, Router};
use sysinfo::System;
use tower_http::trace::TraceLayer;

use crate::config::Config;
use crate::db::{repo, Db};
use crate::error::AppError;
use crate::security::hash_password;

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub db: Db,
    /// One long-lived `System` so CPU percentages are deltas between requests
    /// (the same semantics as psutil's cached `cpu_percent`).
    pub sys: Arc<Mutex<System>>,
}

/// Directories, database, schema and the admin bootstrap.
pub fn bootstrap(cfg: Config) -> Result<AppState, AppError> {
    std::fs::create_dir_all(&cfg.data_dir)?;
    std::fs::create_dir_all(&cfg.runs_dir)?;
    let db = Db::open(&cfg.db_path)?;

    // Same contract as the Flask `_ensure_admin`: ADMIN_PASSWORD is
    // authoritative on every start and resets an existing hash.
    if let Some(password) = cfg.admin_password.clone() {
        let hash = hash_password(&password)?;
        repo::user::upsert_admin(&db, &cfg.admin_username, &hash)?;
        tracing::info!(
            "admin '{}' password (re)set from ADMIN_PASSWORD",
            cfg.admin_username
        );
    }

    let mut sys = System::new();
    sys.refresh_cpu_all();
    sys.refresh_memory();

    Ok(AppState {
        cfg: Arc::new(cfg),
        db,
        sys: Arc::new(Mutex::new(sys)),
    })
}

pub async fn serve(cfg: Config, bind: &str) -> Result<(), AppError> {
    let state = bootstrap(cfg)?;
    
    // Initialize scheduler in background after server starts
    let scheduler_state = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        if let Err(e) = crate::runners::scheduler::init_scheduler(&scheduler_state.db, &scheduler_state.cfg) {
            tracing::error!("Failed to initialize scheduler: {e}");
        }
    });
    
    let app = router(state.clone());
    
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| AppError::internal(&format!("bind {bind}"), e))?;
    tracing::info!(
        "storage-control listening on http://{bind} (db: {})",
        state.cfg.db_path.display()
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| AppError::internal("http server", e))
}

pub fn router(state: AppState) -> Router {
    use routes::dashboard as dash;

    Router::new()
        .route("/", get(dash::index))
        .route("/help", get(dash::help))
        .route("/login", get(routes::auth::form).post(routes::auth::submit))
        .route("/logout", post(routes::auth::logout))
        .route("/services/", get(routes::services::index))
        .route("/services/manage", post(routes::services::manage))
        .route("/services/group", post(routes::services::group))
        .route("/services/logs", get(routes::services::logs))
        .route("/backups/", get(routes::backups::index))
        .route("/backups/tasks", post(routes::backups::create_task))
        .route("/backups/tasks/{id}/update", post(routes::backups::update_task))
        .route("/backups/tasks/{id}/delete", post(routes::backups::delete_task))
        .route("/backups/tasks/{id}/run", post(routes::backups::run_task))
        .route("/backups/files/download", get(routes::backups::download))
        .route("/backups/files/delete", post(routes::backups::delete_file))
        .route("/backups/files/verify", post(routes::backups::verify_file))
        .route("/scheduler/", get(routes::scheduler::index))
        .route("/scheduler/jobs", post(routes::scheduler::create_job))
        .route("/scheduler/jobs/{id}/run", post(routes::scheduler::run_job))
        .route("/scheduler/jobs/{id}/delete", post(routes::scheduler::delete_job))
        .route("/scheduler/timer/toggle", post(routes::scheduler::toggle_timer))
        .route("/scheduler/unit/{scope}/{unit}", get(routes::scheduler::unit_text))
        .route("/scheduler/timers", get(routes::scheduler::timer_list))
        .route("/updates/", get(routes::updates::index))
        .route("/updates/immich/run", post(routes::updates::run_immich))
        .route("/updates/nextcloud/run", post(routes::updates::run_nextcloud))
        .route("/updates/stalwart/run", post(routes::updates::run_stalwart))
        .route("/updates/preserve/prepare", post(routes::updates::prepare_preserve))
        .route("/updates/preserve/{id}/restore", post(routes::updates::restore_preserve))
        .route("/updates/preserve/{id}/delete", post(routes::updates::delete_preserve))
        .route("/updates/api/runs/{id}", get(routes::updates::run_status))
        .route("/updates/api/runs/{id}/log", get(routes::updates::run_log))
        .route("/updates/preserve/{id}/api", get(routes::updates::preserve_status))
        .route("/updates/preserve/{id}/log", get(routes::updates::preserve_log))
        .route("/updates/api/availability", get(routes::updates::api_availability))
        .route("/api/health/summary", get(dash::api_summary))
        .route("/api/health/smart", get(dash::api_smart))
        .route("/api/health/services", get(dash::api_services))
        .route("/api/health/smart/status", get(dash::api_smart_status))
        .route("/api/health/smart/refresh", post(dash::api_smart_refresh))
        .route(
            "/api/health/smart/alert/{id}/resolve",
            post(dash::api_smart_alert_resolve),
        )
        .route(
            "/api/health/smart/history/{device}",
            get(dash::api_smart_history),
        )
        .route("/static/{*path}", get(static_file))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            session::middleware,
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Render an Askama template, turning template failures into a 500.
pub fn render<T: Template>(template: T) -> Response {
    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

async fn not_found() -> AppError {
    AppError::NotFound
}

const FAVICON: &[u8] = include_bytes!("../../static/favicon.svg");
const MANIFEST: &[u8] = include_bytes!("../../static/site.webmanifest");
const DASHBOARD_JS: &[u8] = include_bytes!("../../static/dashboard.js");

/// Static assets are embedded in the binary: deployment is one file, and there
/// is no way to end up with a missing `dashboard.js` after an upgrade.
async fn static_file(Path(path): Path<String>) -> Result<Response, AppError> {
    let (body, content_type): (&'static [u8], &'static str) = match path.as_str() {
        "favicon.svg" => (FAVICON, "image/svg+xml"),
        "site.webmanifest" => (MANIFEST, "application/manifest+json"),
        "dashboard.js" => (DASHBOARD_JS, "text/javascript; charset=utf-8"),
        _ => return Err(AppError::NotFound),
    };
    Ok(([(header::CONTENT_TYPE, content_type)], body).into_response())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}
