//! One error type for the whole app.
//!
//! The Flask original scattered `try/except` blocks across handlers; here every
//! failure funnels into `AppError`, which knows how to turn itself into a
//! response (404 page, redirect to /login, 400 text, 500 text).

use askama::Template;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};

use crate::templates::NotFoundPage;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    BadRequest(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("{0}")]
    Internal(String),
}

impl AppError {
    pub fn internal(context: &str, err: impl std::fmt::Display) -> Self {
        AppError::Internal(format!("{context}: {err}"))
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::Internal(format!("db: {e}"))
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::Internal(format!("io: {e}"))
    }
}

impl From<askama::Error> for AppError {
    fn from(e: askama::Error) -> Self {
        AppError::Internal(format!("template: {e}"))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::NotFound => {
                let page = NotFoundPage {
                    nav: "",
                    csrf: String::new(),
                    flashes: Vec::new(),
                };
                match page.render() {
                    Ok(body) => (StatusCode::NOT_FOUND, axum::response::Html(body)).into_response(),
                    Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
                }
            }
            AppError::Unauthorized => Redirect::to("/login").into_response(),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
            AppError::Internal(msg) => {
                tracing::error!("{msg}");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}
