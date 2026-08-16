//! `/api/auth/login`, `/api/auth/logout`, `/api/auth/status`. Same in-process
//! router harness as the rest of the suite; the AppState is cloned per request
//! so the shared session store survives across calls.

use apb_core::config::ServerConfig;
use apb_core::server_auth;
use apb_server::auth::{AuthState, CSRF_HEADER, CSRF_VALUE, SESSION_COOKIE};
use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

fn seed() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    dir
}

fn authed_with(root: std::path::PathBuf, cfg: ServerConfig) -> (String, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server-auth.yaml");
    let (key, _record) = server_auth::issue_into(&path).unwrap();
    let file = server_auth::load_from(&path).unwrap();
    let auth = Arc::new(AuthState::new(None, file.keys, &cfg).unwrap());
    (key, AppState::new(root).with_auth(auth))
}

async fn send(
    state: &AppState,
    req: Request<Body>,
) -> (StatusCode, Option<String>, serde_json::Value) {
    let res = build_router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let cookie = res
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, cookie, json)
}

fn login_request(key: &str) -> Request<Body> {
    Request::post("/api/auth/login")
        .header("content-type", "application/json")
        .header(CSRF_HEADER, CSRF_VALUE)
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({ "key": key })).unwrap(),
        ))
        .unwrap()
}

#[tokio::test]
async fn status_reports_auth_off_for_a_keyless_server() {
    let dir = seed();
    let state = AppState::new(dir.path().to_path_buf());
    let (status, _, body) = send(
        &state,
        Request::get("/api/auth/status")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["auth_required"], false);
    assert_eq!(
        body["authenticated"], true,
        "with auth off every caller is authenticated by definition"
    );
}

#[tokio::test]
async fn status_reports_required_and_unauthenticated_before_login() {
    let dir = seed();
    let (_key, state) = authed_with(dir.path().to_path_buf(), ServerConfig::default());
    let (status, _, body) = send(
        &state,
        Request::get("/api/auth/status")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["auth_required"], true);
    assert_eq!(body["authenticated"], false);
}

#[tokio::test]
async fn login_mints_a_usable_session_cookie() {
    let dir = seed();
    let (key, state) = authed_with(dir.path().to_path_buf(), ServerConfig::default());

    let (status, set_cookie, body) = send(&state, login_request(&key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["authenticated"], true);
    let set_cookie = set_cookie.expect("a session cookie is set");
    assert!(set_cookie.starts_with(&format!("{SESSION_COOKIE}=")));
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
    assert!(set_cookie.contains("SameSite=Lax"), "{set_cookie}");
    assert!(set_cookie.contains("Path=/"), "{set_cookie}");
    assert!(
        !set_cookie.contains("Secure"),
        "plain http must not set a Secure cookie the browser would drop: {set_cookie}"
    );
    assert!(
        !set_cookie.contains("Max-Age") && !set_cookie.contains("Expires"),
        "a browser session cookie carries no lifetime; the sliding expiry is server-side: {set_cookie}"
    );
    assert!(
        !set_cookie.contains(&key),
        "the API key never rides in the cookie: {set_cookie}"
    );

    let token = set_cookie.split(';').next().unwrap().to_string();

    let (status, _, _) = send(
        &state,
        Request::get("/api/runs")
            .header("cookie", &token)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the cookie authenticates");

    let (status, _, body) = send(
        &state,
        Request::get("/api/auth/status")
            .header("cookie", &token)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["auth_required"], true);
    assert_eq!(body["authenticated"], true);

    // Logout drops the session and clears the cookie.
    let (status, cleared, _) = send(
        &state,
        Request::post("/api/auth/logout")
            .header("cookie", &token)
            .header(CSRF_HEADER, CSRF_VALUE)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cleared = cleared.expect("logout clears the cookie");
    assert!(cleared.contains("Max-Age=0"), "{cleared}");

    let (status, _, _) = send(
        &state,
        Request::get("/api/runs")
            .header("cookie", &token)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the revoked session no longer authenticates"
    );
}

#[tokio::test]
async fn a_wrong_key_is_401_and_the_eleventh_attempt_is_429() {
    let dir = seed();
    let (_key, state) = authed_with(dir.path().to_path_buf(), ServerConfig::default());
    for i in 0..10 {
        let (status, cookie, body) = send(&state, login_request("apb_wrong-key")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "attempt {i}: {body}");
        assert_eq!(body["error"], "auth");
        assert!(cookie.is_none(), "a failed login sets no cookie");
    }
    let (status, _, body) = send(&state, login_request("apb_wrong-key")).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"], "rate_limited");
}

#[tokio::test]
async fn a_public_https_origin_makes_the_cookie_secure() {
    let dir = seed();
    let cfg = ServerConfig {
        bind: None,
        public_base_url: Some("https://apb.example.com".to_string()),
        trusted_proxies: Vec::new(),
    };
    let (key, state) = authed_with(dir.path().to_path_buf(), cfg);
    let (status, set_cookie, _) = send(&state, login_request(&key)).await;
    assert_eq!(status, StatusCode::OK);
    let set_cookie = set_cookie.expect("a session cookie is set");
    assert!(set_cookie.contains("Secure"), "{set_cookie}");
}

#[tokio::test]
async fn login_on_a_keyless_server_is_a_bad_request_not_a_session() {
    let dir = seed();
    let state = AppState::new(dir.path().to_path_buf());
    let (status, cookie, body) = send(&state, login_request("apb_anything")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "auth_disabled");
    assert!(cookie.is_none());
}
