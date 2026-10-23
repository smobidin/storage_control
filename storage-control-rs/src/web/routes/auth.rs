use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use serde::Deserialize;

use crate::db::repo;
use crate::error::AppError;
use crate::security::verify_password;
use crate::templates::LoginPage;
use crate::web::session::Session;
use crate::web::{render, AppState};

#[derive(Deserialize)]
pub struct LoginQuery {
    #[serde(default)]
    pub next: Option<String>,
}

#[derive(Deserialize)]
pub struct LoginForm {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub csrf_token: Option<String>,
    #[serde(default)]
    pub next: Option<String>,
}

pub async fn form(session: Session, Query(query): Query<LoginQuery>) -> Result<Response, AppError> {
    if session.uid().is_some() {
        return Ok(Redirect::to("/").into_response());
    }
    Ok(render(LoginPage {
        nav: "",
        csrf: session.csrf(),
        flashes: session.flashes(),
        username: String::new(),
        next: query.next.unwrap_or_default(),
    }))
}

pub async fn submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    session.verify_csrf(form.csrf_token.as_deref())?;

    let user = repo::user::find_by_username(&state.db, form.username.trim())?;
    let valid = match &user {
        Some(user) => verify_password(&user.password_hash, &form.password),
        None => false,
    };

    if valid {
        if let Some(user) = user {
            session.login(user.id);
            repo::audit::record(&state.db, &user.username, "auth.login", None, None);
            return Ok(Redirect::to(&safe_next(form.next.as_deref())).into_response());
        }
    }

    tracing::info!("failed login for '{}'", form.username);
    session.flash("danger", "Invalid credentials");
    Ok(Redirect::to("/login").into_response())
}

pub async fn logout(State(state): State<AppState>, session: Session) -> Result<Response, AppError> {
    session.require_uid()?;
    repo::audit::record(&state.db, "admin", "auth.logout", None, None);
    session.logout();
    Ok(Redirect::to("/login").into_response())
}

/// Only local absolute paths are accepted as a post-login target.
fn safe_next(next: Option<&str>) -> String {
    match next {
        Some(path) if path.starts_with('/') && !path.starts_with("//") => path.to_string(),
        _ => "/".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_must_stay_local() {
        assert_eq!(safe_next(Some("/backups/")), "/backups/");
        assert_eq!(safe_next(Some("https://evil.example")), "/");
        assert_eq!(safe_next(Some("//evil.example")), "/");
        assert_eq!(safe_next(None), "/");
    }
}
