//! Signed-cookie sessions, CSRF tokens and flash messages.
//!
//! Replaces Flask-Login + Flask-WTF + `flash()` with one small middleware:
//! the session lives in a single HMAC-signed cookie, the CSRF token is a field
//! inside it (stable for the session, rotated on login), and flashes are a list
//! that is drained by the next rendered page.

use std::sync::{Arc, Mutex};

use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderValue, Request};
use axum::middleware::Next;
use axum::response::Response;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::security::{sign, verify_signature};
use crate::web::AppState;

pub const COOKIE_NAME: &str = "sc_session";
const MAX_AGE_SECS: i64 = 14 * 24 * 3600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flash {
    pub class: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionData {
    pub uid: Option<i64>,
    pub csrf: String,
    pub flashes: Vec<Flash>,
}

#[derive(Default)]
struct Inner {
    data: SessionData,
    dirty: bool,
}

#[derive(Clone)]
pub struct Session {
    inner: Arc<Mutex<Inner>>,
    secret: Arc<Vec<u8>>,
}

impl Session {
    fn parse(secret: &[u8], cookie_header: Option<&str>) -> (Self, bool) {
        if let Some(raw) = cookie_header.and_then(|h| cookie_value(h, COOKIE_NAME)) {
            if let Some(data) = decode(secret, &raw) {
                return (
                    Self {
                        inner: Arc::new(Mutex::new(Inner { data, dirty: false })),
                        secret: Arc::new(secret.to_vec()),
                    },
                    false,
                );
            }
        }
        let data = SessionData {
            uid: None,
            csrf: new_csrf(),
            flashes: Vec::new(),
        };
        (
            Self {
                inner: Arc::new(Mutex::new(Inner { data, dirty: true })),
                secret: Arc::new(secret.to_vec()),
            },
            true,
        )
    }

    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }

    pub fn uid(&self) -> Option<i64> {
        self.with(|s| s.data.uid)
    }

    pub fn require_uid(&self) -> Result<i64, AppError> {
        self.uid().ok_or(AppError::Unauthorized)
    }

    pub fn csrf(&self) -> String {
        self.with(|s| s.data.csrf.clone())
    }

    pub fn flashes(&self) -> Vec<Flash> {
        self.with(|s| {
            let flashes = std::mem::take(&mut s.data.flashes);
            if !flashes.is_empty() {
                s.dirty = true;
            }
            flashes
        })
    }

    pub fn flash(&self, class: &str, message: impl Into<String>) {
        self.with(|s| {
            s.data.flashes.push(Flash {
                class: class.to_string(),
                message: message.into(),
            });
            s.dirty = true;
        });
    }

    /// Mark the session authenticated and rotate the CSRF token (session fixation).
    pub fn login(&self, uid: i64) {
        self.with(|s| {
            s.data.uid = Some(uid);
            s.data.csrf = new_csrf();
            s.dirty = true;
        });
    }

    pub fn logout(&self) {
        self.with(|s| {
            s.data.uid = None;
            s.data.csrf = new_csrf();
            s.data.flashes.clear();
            s.dirty = true;
        });
    }

    pub fn verify_csrf(&self, token: Option<&str>) -> Result<(), AppError> {
        let expected = self.csrf();
        let given = token.unwrap_or_default();
        if !expected.is_empty() && constant_time_eq(given.as_bytes(), expected.as_bytes()) {
            Ok(())
        } else {
            Err(AppError::BadRequest("CSRF token missing or invalid".into()))
        }
    }

    fn is_dirty(&self) -> bool {
        self.with(|s| s.dirty)
    }

    fn to_cookie(&self) -> String {
        let data = self.with(|s| s.data.clone());
        let payload = serde_json::to_string(&data).unwrap_or_else(|_| "{}".into());
        let encoded = encode(&self.secret, &payload);
        format!(
            "{COOKIE_NAME}={encoded}; Path=/; HttpOnly; SameSite=Lax; Max-Age={MAX_AGE_SECS}"
        )
    }
}

impl FromRequestParts<AppState> for Session {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Session>()
            .cloned()
            .ok_or_else(|| AppError::Internal("session middleware is not installed".into()))
    }
}

/// Session middleware: parse (or mint) the cookie, expose `Session` to handlers,
/// persist it when it changed.
pub async fn middleware(State(state): State<AppState>, mut req: Request<axum::body::Body>, next: Next) -> Response {
    let header = req
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let secret = state.cfg.secret_key.as_bytes().to_vec();
    let (session, fresh) = Session::parse(&secret, header.as_deref());
    req.extensions_mut().insert(session.clone());

    let mut response = next.run(req).await;
    if fresh || session.is_dirty() {
        if let Ok(value) = HeaderValue::from_str(&session.to_cookie()) {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
    }
    response
}

fn cookie_value(header: &str, name: &str) -> Option<String> {
    header.split(';').find_map(|part| {
        let part = part.trim();
        let (key, value) = part.split_once('=')?;
        (key.trim() == name).then(|| value.trim().to_string())
    })
}

fn encode(secret: &[u8], payload: &str) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let body = URL_SAFE_NO_PAD.encode(payload.as_bytes());
    let signature = sign(secret, &body);
    format!("{body}.{signature}")
}

fn decode(secret: &[u8], raw: &str) -> Option<SessionData> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let (body, signature) = raw.rsplit_once('.')?;
    if !verify_signature(secret, body, signature) {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(body).ok()?;
    let data: SessionData = serde_json::from_slice(&bytes).ok()?;
    if data.csrf.is_empty() {
        return None;
    }
    Some(data)
}

fn new_csrf() -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use rand::RngCore;
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"test-secret";

    #[test]
    fn roundtrip_preserves_data() {
        let (session, _) = Session::parse(SECRET, None);
        session.login(7);
        session.flash("info", "hello");
        let raw = session.to_cookie();
        let value = raw.split_once('=').unwrap().1.split_once(';').unwrap().0;
        let (restored, fresh) = Session::parse(SECRET, Some(&format!("sc_session={value}")));
        assert!(!fresh);
        assert_eq!(restored.uid(), Some(7));
        assert_eq!(restored.csrf(), session.csrf());
        assert_eq!(restored.flashes().len(), 1);
    }

    #[test]
    fn tampered_cookie_is_rejected() {
        let (session, _) = Session::parse(SECRET, None);
        session.login(1);
        let raw = session.to_cookie();
        let value = raw.split_once('=').unwrap().1.split_once(';').unwrap().0;
        let mut tampered = value.to_string();
        tampered.push('A');
        let (fresh_session, fresh) =
            Session::parse(SECRET, Some(&format!("sc_session={tampered}")));
        assert!(fresh);
        assert_eq!(fresh_session.uid(), None);
    }

    #[test]
    fn logout_clears_identity() {
        let (session, _) = Session::parse(SECRET, None);
        session.login(3);
        session.logout();
        assert_eq!(session.uid(), None);
    }

    #[test]
    fn csrf_check() {
        let (session, _) = Session::parse(SECRET, None);
        let token = session.csrf();
        assert!(session.verify_csrf(Some(&token)).is_ok());
        assert!(session.verify_csrf(Some("wrong")).is_err());
        assert!(session.verify_csrf(None).is_err());
    }
}
