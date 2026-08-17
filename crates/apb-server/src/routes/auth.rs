//! The three endpoints the browser login flow needs (spec
//! 2026-08-16-server-mode-design).
//!
//! `login` is the only place a raw API key crosses the HTTP boundary inbound.
//! It is exchanged for an opaque session token whose SHA-256 is what the
//! server keeps; the SPA never stores the key itself. `status` exists so the
//! SPA can decide whether to render the login screen without provoking a 401,
//! and it is exempt from the gate for exactly that reason.

use crate::auth::{
    ClientCtx, Credential, SESSION_COOKIE, cookie_value, evaluate, log_auth_failure, rate_limited,
};
use crate::state::AppState;

use apb_core::server_auth;
use axum::Extension;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct LoginBody {
    key: String,
}

/// The session cookie exactly as the browser must store it. `Secure` is added
/// only when the browser actually reached apb over https, because a Secure
/// cookie sent over plain http is discarded and the loopback dashboard would
/// then never be able to log in.
///
/// Deliberately carries no `Max-Age` or `Expires`, so it is a browser session
/// cookie. The 7-day sliding expiry lives server-side in the session store,
/// which is the only place that can see activity: a cookie lifetime would
/// either outlive an evicted store entry (a cookie the server no longer
/// honors) or expire under a still-active session.
fn session_cookie(token: &str, https: bool) -> String {
    let mut cookie = format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax");
    if https {
        cookie.push_str("; Secure");
    }
    cookie
}

fn cleared_cookie(https: bool) -> String {
    let mut cookie = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    if https {
        cookie.push_str("; Secure");
    }
    cookie
}

fn with_cookie(mut res: Response, cookie: &str) -> Response {
    if let Ok(value) = header::HeaderValue::from_str(cookie) {
        res.headers_mut().insert(header::SET_COOKIE, value);
    }
    res
}

/// POST /api/auth/login: one API key in, one session cookie out.
pub(crate) async fn login_handler(
    State(state): State<AppState>,
    Extension(ctx): Extension<ClientCtx>,
    Json(body): Json<LoginBody>,
) -> Response {
    let auth = state.auth.clone();
    if !auth.enabled() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "auth_disabled" })),
        )
            .into_response();
    }
    let now = apb_core::clock::now_ms();
    // Verify the key first: a valid key always logs in, even from an IP whose
    // failure budget is spent, so a bad-request flood from a shared NAT/proxy
    // cannot lock out a legitimate operator behind it. verify_key_with_reload
    // force-reloads the key file on every attempt (not only after a failure),
    // so an operator who has just issued the very first key can sign in
    // immediately without restarting the dashboard.
    let key_id = match auth.verify_key_with_reload(body.key.trim(), now) {
        Some(id) => id,
        None => {
            // A bad key: only now consult and feed the limiter, so repeated
            // guessing from one IP is still throttled.
            let blocked = {
                let failures = auth.failures();
                failures.is_blocked(ctx.ip, now)
            };
            if blocked {
                return rate_limited();
            }
            let over_budget = {
                let mut failures = auth.failures();
                failures.record_failure(ctx.ip, now)
            };
            log_auth_failure(ctx.ip, "/api/auth/login");
            return if over_budget {
                rate_limited()
            } else {
                crate::auth::unauthorized()
            };
        }
    };
    let token = match server_auth::random_token() {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "random", "message": e.to_string() })),
            )
                .into_response();
        }
    };
    {
        let mut sessions = auth.sessions();
        sessions.insert(server_auth::hash_hex(&token), now, key_id);
    }
    let res = Json(serde_json::json!({ "authenticated": true })).into_response();
    with_cookie(res, &session_cookie(&token, ctx.https))
}

/// POST /api/auth/logout: drops this session and clears the cookie. Goes
/// through the gate like any other write, so it carries the marker header.
pub(crate) async fn logout_handler(
    State(state): State<AppState>,
    Extension(ctx): Extension<ClientCtx>,
    headers: HeaderMap,
) -> Response {
    let auth = state.auth.clone();
    if let Some(token) = cookie_value(&headers, SESSION_COOKIE) {
        let mut sessions = auth.sessions();
        sessions.remove(&server_auth::hash_hex(&token));
    }
    let res = Json(serde_json::json!({ "authenticated": false })).into_response();
    with_cookie(res, &cleared_cookie(ctx.https))
}

/// GET /api/auth/status: what the SPA needs to decide between the login screen
/// and the app, without probing a protected route and swallowing a 401.
pub(crate) async fn status_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let auth = state.auth.clone();
    let required = auth.enabled();
    let authenticated = if required {
        evaluate(&auth, &headers, apb_core::clock::now_ms()) != Credential::None
    } else {
        true
    };
    Json(serde_json::json!({
        "auth_required": required,
        "authenticated": authenticated,
    }))
    .into_response()
}
