//! `/services/` page, unit control, group actions and journal tail.
//!
//! Same surface as the Flask blueprint: a grouped catalog table, five control
//! actions, "all" actions per group and a plain-text journal for one unit.
//! Everything that shells out runs in `spawn_blocking`; the database lock is
//! never held across a subprocess call.

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use serde::Deserialize;

use crate::collect::services::{self, ServiceStatus};
use crate::db::repo;
use crate::error::AppError;
use crate::templates::{ServiceGroupBlock, ServiceRow, ServicesPage};
use crate::web::render;
use crate::web::session::Session;
use crate::web::AppState;

/// Rendering order of the group cards (the Flask template's fixed list).
const GROUP_ORDER: [&str; 5] = ["immich", "stalwart", "nextcloud", "infra", "backup"];

pub async fn index(session: Session) -> Result<Response, AppError> {
    session.require_uid()?;

    let statuses = tokio::task::spawn_blocking(services::all_status)
        .await
        .map_err(|e| AppError::internal("services collector", e))?;

    Ok(render(ServicesPage {
        nav: "services",
        csrf: session.csrf(),
        flashes: session.flashes(),
        groups: group_blocks(statuses),
    }))
}

#[derive(Deserialize)]
pub struct ManageForm {
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub csrf_token: Option<String>,
}

pub async fn manage(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Form(form): Form<ManageForm>,
) -> Result<Response, AppError> {
    let uid = session.require_uid()?;
    session.verify_csrf(form.csrf_token.as_deref())?;

    let scope = form
        .scope
        .as_deref()
        .map(str::trim)
        .filter(|scope| !scope.is_empty() && *scope != "None")
        .map(str::to_string)
        .unwrap_or_else(|| services::scope_of(&form.unit).to_string());

    let (ok, message) = services::manage_one(&form.unit, &scope, &form.action)
        .map_err(AppError::BadRequest)?;

    repo::audit::record(
        &state.db,
        &format!("user:{uid}"),
        &format!("service.{}", form.action),
        Some(&form.unit),
        Some(&message),
    );

    let text = format!("{}: {}", form.unit, if message.is_empty() { "ok" } else { &message });
    Ok(action_response(
        &session,
        is_xhr(&headers),
        ok,
        &text,
    ))
}

#[derive(Deserialize)]
pub struct GroupForm {
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub csrf_token: Option<String>,
}

pub async fn group(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Form(form): Form<GroupForm>,
) -> Result<Response, AppError> {
    let uid = session.require_uid()?;
    session.verify_csrf(form.csrf_token.as_deref())?;

    let results = services::group_action(&form.group, &form.action).map_err(AppError::BadRequest)?;
    let all_ok = results.iter().all(|(_, ok, _)| *ok);
    let failures: Vec<&str> = results
        .iter()
        .filter(|(_, ok, _)| !ok)
        .map(|(name, _, _)| name.as_str())
        .collect();

    let summary = if all_ok {
        format!("{}: {}/{} ok", form.group, results.len(), results.len())
    } else {
        format!(
            "{}: {} of {} failed ({})",
            form.group,
            failures.len(),
            results.len(),
            failures.join(", ")
        )
    };

    repo::audit::record(
        &state.db,
        &format!("user:{uid}"),
        &format!("group.{}", form.action),
        Some(&form.group),
        Some(&summary),
    );

    if is_xhr(&headers) {
        let results: Vec<serde_json::Value> = results
            .iter()
            .map(|(name, ok, msg)| {
                serde_json::json!({ "name": name, "ok": ok, "msg": msg })
            })
            .collect();
        return Ok(axum::Json(serde_json::json!({
            "ok": all_ok,
            "results": results,
        }))
        .into_response());
    }

    session.flash(if all_ok { "success" } else { "danger" }, summary);
    Ok(Redirect::to("/services/").into_response())
}

#[derive(Deserialize)]
pub struct LogsQuery {
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub scope: Option<String>,
}

pub async fn logs(session: Session, Query(query): Query<LogsQuery>) -> Result<Response, AppError> {
    session.require_uid()?;

    let unit = query.unit;
    let scope = query.scope.unwrap_or_else(|| "system".to_string());
    let body = tokio::task::spawn_blocking(move || services::logs(&unit, &scope))
        .await
        .map_err(|e| AppError::internal("journalctl", e))?
        .ok_or_else(|| AppError::BadRequest("invalid unit".into()))?;

    Ok((
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body,
    )
        .into_response())
}

/// A normal form post redirects back with a flash; a fetch() call gets JSON so
/// an inline action can report its own result.
fn action_response(session: &Session, xhr: bool, ok: bool, text: &str) -> Response {
    if xhr {
        return axum::Json(serde_json::json!({ "ok": ok, "msg": text })).into_response();
    }
    session.flash(if ok { "success" } else { "danger" }, text);
    Redirect::to("/services/").into_response()
}

fn is_xhr(headers: &HeaderMap) -> bool {
    headers
        .get("x-requested-with")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.eq_ignore_ascii_case("XMLHttpRequest"))
        .unwrap_or(false)
}

/// Group the catalog into the fixed card order, formatting everything the
/// template needs (badge class, human timestamp) here rather than in Askama.
fn group_blocks(statuses: Vec<ServiceStatus>) -> Vec<ServiceGroupBlock> {
    let mut labels: Vec<&'static str> = GROUP_ORDER.to_vec();
    for status in &statuses {
        if !labels.contains(&status.group) {
            labels.push(status.group);
        }
    }

    labels
        .into_iter()
        .filter_map(|group| {
            let mut rows: Vec<&ServiceStatus> = statuses
                .iter()
                .filter(|status| status.group == group)
                .collect();
            if rows.is_empty() {
                return None;
            }
            rows.sort_by_key(|status| status.order);

            Some(ServiceGroupBlock {
                name: group.to_string(),
                label: group.to_uppercase(),
                rows: rows.iter().map(|status| row(status)).collect(),
            })
        })
        .collect()
}

fn row(status: &ServiceStatus) -> ServiceRow {
    let display = if status.display_status.is_empty() {
        status.active.clone()
    } else {
        status.display_status.clone()
    };
    let badge = match display.as_str() {
        "active" | "success" => "active",
        "inactive" | "failed" => "inactive",
        _ => "unknown",
    };

    // Prefer when the unit last finished, then when it started, then the
    // timer's next eligible run.
    let last_run = status
        .last_run
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            status
                .active_exit
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
        });
    let when = if let Some(value) = last_run {
        services::display_timestamp(Some(value))
    } else if let Some(value) = status.active_since.as_deref() {
        services::display_timestamp(Some(value))
    } else if let Some(value) = status.next_run.as_deref() {
        format!("next: {}", services::display_timestamp(Some(value)))
    } else {
        "—".to_string()
    };

    ServiceRow {
        name: status.name.to_string(),
        scope: status.scope.to_string(),
        desc: status.desc.to_string(),
        status: display,
        badge,
        oneshot: status.unit_type.as_deref() == Some("oneshot") && status.active_exit.is_some(),
        last_run: when,
        enabled: if status.enabled.trim().is_empty() {
            "—".to_string()
        } else {
            status.enabled.clone()
        },
        critical: status.critical,
        actions: crate::templates::SERVICE_ACTIONS
            .iter()
            .map(|(name, class)| crate::templates::ServiceAction { name, class })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(name: &'static str, group: &'static str, order: u32, display: &str) -> ServiceStatus {
        ServiceStatus {
            name,
            scope: "user",
            group,
            order,
            critical: false,
            desc: "test",
            active: "active".into(),
            enabled: "enabled".into(),
            display_status: display.into(),
            active_since: None,
            active_exit: None,
            last_run: None,
            memory_current: None,
            exec_status: None,
            exec_code: None,
            unit_type: None,
            result: None,
            next_run: None,
        }
    }

    #[test]
    fn groups_follow_the_fixed_order_and_skip_empty_ones() {
        let blocks = group_blocks(vec![
            status("nginx.service", "infra", 40, "active"),
            status("stalwart.service", "stalwart", 10, "active"),
        ]);
        let names: Vec<&str> = blocks.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, vec!["stalwart", "infra"]);
        assert_eq!(blocks[0].label, "STALWART");
    }

    #[test]
    fn rows_are_sorted_and_badges_match_status() {
        let blocks = group_blocks(vec![
            status("immich-server.service", "immich", 20, "active"),
            status("immich-ml.service", "immich", 10, "failed"),
        ]);
        let rows = &blocks[0].rows;
        assert_eq!(rows[0].name, "immich-ml.service");
        assert_eq!(rows[0].badge, "inactive");
        assert_eq!(rows[1].badge, "active");
    }

    #[test]
    fn oneshot_marker_requires_exit_timestamp() {
        let mut item = status("immich-nightfix.service", "immich", 30, "success");
        item.unit_type = Some("oneshot".into());
        assert!(!row(&item).oneshot);
        item.active_exit = Some("Wed 2026-10-21 03:36:44 MSK".into());
        assert!(row(&item).oneshot);
        assert_eq!(row(&item).last_run, "2026-10-21 03:36:44");
    }

    #[test]
    fn timer_fallback_reads_next_run() {
        let mut item = status("certbot-renew.timer", "infra", 50, "active");
        item.next_run = Some("Thu 2026-10-22 00:00:00 MSK".into());
        assert_eq!(row(&item).last_run, "next: 2026-10-22 00:00:00");
    }

    #[test]
    fn unknown_groups_are_appended() {
        let blocks = group_blocks(vec![status("other.service", "other", 1, "inactive")]);
        assert_eq!(blocks.last().unwrap().name, "other");
    }
}
