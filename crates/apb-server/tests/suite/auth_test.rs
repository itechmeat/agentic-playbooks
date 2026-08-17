//! The auth middleware: pass-through when no key exists, bearer and cookie
//! credentials, the CSRF second layer, exempt paths, and the failure rate
//! limiter. Requests go through the in-process router exactly like every other
//! suite here (`tower::ServiceExt::oneshot`), so no socket is opened; the
//! middleware falls back to a loopback client IP when `ConnectInfo` is absent,
//! which is precisely this situation.

use apb_core::config::ServerConfig;
use apb_core::server_auth;
use apb_server::auth::{AuthState, CSRF_HEADER, CSRF_VALUE, SESSION_COOKIE};
use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

/// Issues a key whose id is not an all-digit string, revoking and retrying
/// until it gets one.
///
/// A `KeyRecord` serializes to a fixed-width record, which is what makes a
/// revoke-then-issue reproduce the same file length. The one exception is the
/// id: it is the first 8 hex chars of the hash, and when those happen to be all
/// digits the YAML writer quotes the value to preserve its string type, adding
/// two bytes. Two keys that disagree on that make the file lengths differ for a
/// reason unrelated to what the same-tick test is about, so the ids are pinned
/// to the unquoted form rather than the precondition being left to a coin flip.
fn issue_unquoted_id(path: &std::path::Path) -> (String, apb_core::server_auth::KeyRecord) {
    loop {
        let (key, record) = server_auth::issue_into(path).unwrap();
        if !record.id.bytes().all(|b| b.is_ascii_digit()) {
            return (key, record);
        }
        server_auth::revoke_in(path, &record.id).unwrap();
    }
}

/// A pinned-root state whose auth layer holds exactly one freshly issued key.
/// The key file lives in a tempdir that is dropped immediately and the auth
/// state is built with no watched path, so these tests never take the reload
/// branch; the dedicated reload test below keeps its own file alive.
fn authed(root: std::path::PathBuf) -> (String, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server-auth.yaml");
    let (key, _record) = server_auth::issue_into(&path).unwrap();
    let file = server_auth::load_from(&path).unwrap();
    let auth = Arc::new(AuthState::new(None, file.keys, &ServerConfig::default()).unwrap());
    (key, AppState::new(root).with_auth(auth))
}

async fn send(state: &AppState, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let res = build_router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

fn seed() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    dir
}

#[tokio::test]
async fn with_no_keys_every_route_stays_open() {
    let dir = seed();
    let state = AppState::new(dir.path().to_path_buf());
    let (status, _) = send(
        &state,
        Request::get("/api/runs").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the local dashboard is unchanged");
}

#[tokio::test]
async fn a_valid_bearer_key_passes_and_an_invalid_one_is_401() {
    let dir = seed();
    let (key, state) = authed(dir.path().to_path_buf());

    let (status, _) = send(
        &state,
        Request::get("/api/runs")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(
        &state,
        Request::get("/api/runs")
            .header("authorization", "Bearer apb_not-a-real-key")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body["error"], "auth",
        "the 401 body shape is stable: {body}"
    );

    let (status, body) = send(
        &state,
        Request::get("/api/runs").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "auth");
}

#[tokio::test]
async fn exempt_paths_are_reachable_without_a_credential() {
    let dir = seed();
    let (_key, state) = authed(dir.path().to_path_buf());

    let (status, _) = send(
        &state,
        Request::get("/api/health").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the health probe stays open");

    // The SPA shell must load so it can render the login screen.
    let res = build_router(state.clone())
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_ne!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "the static fallback is never gated"
    );

    // The run-hook endpoint carries its own path secret; an unknown one is a
    // 404 from the handler, never a 401 from the gate.
    let (status, _) = send(
        &state,
        Request::post("/api/hooks/run-1/deadbeef")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_session_cookie_authenticates_and_writes_need_the_csrf_header() {
    let dir = seed();
    let (_key, state) = authed(dir.path().to_path_buf());
    let token = server_auth::random_token().unwrap();
    {
        let key_id = state.auth.live_key_ids().into_iter().next().unwrap();
        state.auth.sessions().insert(
            server_auth::hash_hex(&token),
            apb_core::clock::now_ms(),
            key_id,
        );
    }
    let cookie = format!("{SESSION_COOKIE}={token}");

    let (status, _) = send(
        &state,
        Request::get("/api/runs")
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "a GET needs no marker header");

    let (status, body) = send(
        &state,
        Request::post("/api/connectors/demo/call")
            .header("cookie", &cookie)
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a cookie-authenticated write without the marker header is refused"
    );
    assert_eq!(body["error"], "csrf");

    let res = build_router(state.clone())
        .oneshot(
            Request::post("/api/connectors/demo/call")
                .header("cookie", &cookie)
                .header(CSRF_HEADER, CSRF_VALUE)
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        res.status(),
        StatusCode::FORBIDDEN,
        "with the marker header the request reaches the handler"
    );
}

#[tokio::test]
async fn a_bearer_write_needs_no_csrf_header() {
    let dir = seed();
    let (key, state) = authed(dir.path().to_path_buf());
    let res = build_router(state.clone())
        .oneshot(
            Request::post("/api/connectors/demo/call")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::FORBIDDEN);
    assert_ne!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn eleven_failures_in_a_window_earn_a_429() {
    let dir = seed();
    let (_key, state) = authed(dir.path().to_path_buf());
    for i in 0..10 {
        let (status, _) = send(
            &state,
            Request::get("/api/runs").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "attempt {i} is a plain 401"
        );
    }
    let (status, body) = send(
        &state,
        Request::get("/api/runs").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"], "rate_limited");

    let (status, _) = send(
        &state,
        Request::get("/api/health").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "exempt paths stay reachable");
}

#[tokio::test]
async fn a_valid_key_still_works_when_the_limiter_is_tripped() {
    let dir = seed();
    let (key, state) = authed(dir.path().to_path_buf());

    // Prove the key works before the limiter is tripped.
    let (status, _) = send(
        &state,
        Request::get("/api/runs")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Burn the budget from the same client IP with unauthenticated requests.
    for _ in 0..11 {
        send(
            &state,
            Request::get("/api/runs").body(Body::empty()).unwrap(),
        )
        .await;
    }

    // An unauthenticated request from this IP is now blocked.
    let (status, body) = send(
        &state,
        Request::get("/api/runs").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"], "rate_limited");

    // But a valid key is verified first and always passes, whatever the
    // limiter's state: a bad-request flood from a shared NAT/proxy IP must not
    // lock out a legitimate operator behind it.
    let (status, _) = send(
        &state,
        Request::get("/api/runs")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a valid key escapes the tripped limiter"
    );
}

#[tokio::test]
async fn a_session_stops_authenticating_once_its_minting_key_is_revoked() {
    let dir = seed();
    // Keep the key file alive so the auth state can reload it after a revoke.
    // Two keys, so revoking the one that minted the session leaves auth
    // enabled (otherwise a zero-key set would flip the dashboard back to its
    // keyless pass-through and mask the revocation).
    let keydir = tempfile::tempdir().unwrap();
    let path = keydir.path().join("server-auth.yaml");
    let (_key_a, record_a) = server_auth::issue_into(&path).unwrap();
    let (_key_b, _record_b) = server_auth::issue_into(&path).unwrap();
    let file = server_auth::load_from(&path).unwrap();
    let auth =
        Arc::new(AuthState::new(Some(path.clone()), file.keys, &ServerConfig::default()).unwrap());
    let state = AppState::new(dir.path().to_path_buf()).with_auth(auth.clone());

    // Mint a session bound to key A (as login does).
    let token = server_auth::random_token().unwrap();
    {
        auth.sessions().insert(
            server_auth::hash_hex(&token),
            apb_core::clock::now_ms(),
            record_a.id.clone(),
        );
    }
    let cookie = format!("{SESSION_COOKIE}={token}");

    // The session authenticates while its key is live.
    let (status, _) = send(
        &state,
        Request::get("/api/runs")
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "a fresh session authenticates");

    // Revoke key A on disk and force the key set to reflect it, the way any
    // bearer request or the once-a-minute background check would. Key B stays,
    // so auth remains enabled.
    server_auth::revoke_in(&path, &record_a.id).unwrap();
    auth.maybe_reload(apb_core::clock::now_ms(), true);

    // The session no longer authenticates: it cannot outlive its key.
    let (status, body) = send(
        &state,
        Request::get("/api/runs")
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "auth");
}

/// A cookie session minted by a key that is then revoked via a same-length,
/// same-mtime-tick rewrite is rejected on its next use, with no bearer traffic
/// and no explicit reload to trigger the content-hash check. The cookie path
/// forces that reload itself; the throttled stat-only background reload cannot
/// see a change the pinned `(mtime, len)` hides.
#[tokio::test]
async fn a_cookie_session_is_rejected_after_a_same_tick_revoke() {
    let dir = seed();
    let keydir = tempfile::tempdir().unwrap();
    let path = keydir.path().join("server-auth.yaml");
    let (_key_a, record_a) = issue_unquoted_id(&path);
    let file = server_auth::load_from(&path).unwrap();
    let auth =
        Arc::new(AuthState::new(Some(path.clone()), file.keys, &ServerConfig::default()).unwrap());
    let state = AppState::new(dir.path().to_path_buf()).with_auth(auth.clone());

    let original_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    let original_len = std::fs::metadata(&path).unwrap().len();

    // Mint a session bound to key A (as login does).
    let token = server_auth::random_token().unwrap();
    auth.sessions().insert(
        server_auth::hash_hex(&token),
        apb_core::clock::now_ms(),
        record_a.id.clone(),
    );
    let cookie = format!("{SESSION_COOKIE}={token}");

    let (status, _) = send(
        &state,
        Request::get("/api/runs")
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "a fresh session authenticates");

    // Revoke A and issue B: a single-key file stays the same length. Pin the
    // mtime back so `(mtime, len)` is unchanged, the exact case the stat-only
    // reload cannot see. Nothing here calls maybe_reload; the cookie path must
    // catch it on its own.
    server_auth::revoke_in(&path, &record_a.id).unwrap();
    let (_key_b, _record_b) = issue_unquoted_id(&path);
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        original_len,
        "a single-key file stays the same length across a revoke+issue"
    );
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_modified(original_mtime).unwrap();

    let (status, body) = send(
        &state,
        Request::get("/api/runs")
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the revoked key's cookie session is rejected on its next use: {body}"
    );
}

#[tokio::test]
async fn the_websocket_upgrade_is_gated_like_every_other_api_route() {
    let dir = seed();
    let (_key, state) = authed(dir.path().to_path_buf());
    let (status, body) = send(&state, Request::get("/api/ws").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "auth");
}

#[tokio::test]
async fn every_response_denies_framing() {
    let dir = seed();

    // With auth off, which is the local default.
    let open_state = AppState::new(dir.path().to_path_buf());
    let res = build_router(open_state)
        .oneshot(Request::get("/api/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        res.headers()
            .get("x-frame-options")
            .and_then(|v| v.to_str().ok()),
        Some("DENY"),
        "the panel must not be frameable even on a keyless dashboard"
    );

    // And on a rejection, where the response never reaches a handler.
    let (_key, state) = authed(dir.path().to_path_buf());
    let res = build_router(state.clone())
        .oneshot(Request::get("/api/runs").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        res.headers()
            .get("x-frame-options")
            .and_then(|v| v.to_str().ok()),
        Some("DENY")
    );

    // A CSRF rejection is also produced before the handler runs.
    let token = server_auth::random_token().unwrap();
    let key_id = state.auth.live_key_ids().into_iter().next().unwrap();
    state.auth.sessions().insert(
        server_auth::hash_hex(&token),
        apb_core::clock::now_ms(),
        key_id,
    );
    let res = build_router(state.clone())
        .oneshot(
            Request::post("/api/connectors/demo/call")
                .header("cookie", format!("{SESSION_COOKIE}={token}"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        res.headers()
            .get("x-frame-options")
            .and_then(|v| v.to_str().ok()),
        Some("DENY"),
        "a CSRF rejection must still deny framing"
    );

    // And a 429 from the rate limiter. The UNAUTHORIZED check above already
    // spent one failure on this same state; nine more unauthenticated
    // requests bring the count to ten (still plain 401s), and the eleventh
    // trips the limiter.
    for _ in 0..9 {
        send(
            &state,
            Request::get("/api/runs").body(Body::empty()).unwrap(),
        )
        .await;
    }
    let res = build_router(state.clone())
        .oneshot(Request::get("/api/runs").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        res.headers()
            .get("x-frame-options")
            .and_then(|v| v.to_str().ok()),
        Some("DENY"),
        "a rate-limited response must still deny framing"
    );
}

#[tokio::test]
async fn a_revoked_key_stops_working_and_a_new_one_starts_without_a_restart() {
    let dir = seed();
    // This test owns the key file for its whole duration, so the tempdir is
    // bound rather than dropped.
    let keydir = tempfile::tempdir().unwrap();
    let path = keydir.path().join("server-auth.yaml");
    let (key_a, record_a) = server_auth::issue_into(&path).unwrap();
    let file = server_auth::load_from(&path).unwrap();
    let auth =
        Arc::new(AuthState::new(Some(path.clone()), file.keys, &ServerConfig::default()).unwrap());
    let state = AppState::new(dir.path().to_path_buf()).with_auth(auth);

    let bearer = |key: &str| {
        Request::get("/api/runs")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap()
    };

    let (status, _) = send(&state, bearer(&key_a)).await;
    assert_eq!(status, StatusCode::OK, "key A works to begin with");

    // Rotate the file underneath the running server, exactly as
    // `apb server key revoke` and `apb server key issue` would.
    server_auth::revoke_in(&path, &record_a.id).unwrap();
    let (key_b, _record_b) = server_auth::issue_into(&path).unwrap();
    // The stamp is (mtime, len); make sure the mtime moved even on a
    // coarse-grained filesystem clock.
    filetime_bump(&path);

    // The failing request forces the reload, which is what the spec asks for:
    // no restart, effect on the very next request.
    let (status, body) = send(&state, bearer(&key_a)).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the revoked key is rejected: {body}"
    );

    let (status, _) = send(&state, bearer(&key_b)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the newly issued key is accepted without a restart"
    );

    assert_eq!(
        state.auth.key_count(),
        1,
        "the reloaded set holds only key B"
    );
}

/// Makes sure the key file's mtime differs from whatever the auth state
/// recorded, on filesystems whose mtime granularity is a full second. Rewrites
/// the file's own bytes through the same atomic-private path the real writer
/// uses, so nothing about the content changes.
fn filetime_bump(path: &std::path::Path) {
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let raw = std::fs::read(path).unwrap();
    apb_core::fsutil::atomic_write_private(path, &raw).unwrap();
}

#[tokio::test]
async fn a_session_cookie_is_accepted_at_the_websocket_upgrade() {
    let dir = seed();
    let (_key, state) = authed(dir.path().to_path_buf());
    let token = server_auth::random_token().unwrap();
    {
        let key_id = state.auth.live_key_ids().into_iter().next().unwrap();
        state.auth.sessions().insert(
            server_auth::hash_hex(&token),
            apb_core::clock::now_ms(),
            key_id,
        );
    }
    // A plain GET cannot complete an upgrade, so the assertion is about the
    // gate: with a session cookie the request reaches the ws handler (which
    // answers with an upgrade error), without one it is stopped at 401.
    let res = build_router(state.clone())
        .oneshot(
            Request::get("/api/ws")
                .header("cookie", format!("{SESSION_COOKIE}={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::UNAUTHORIZED);
    assert_ne!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_bearer_key_is_accepted_at_the_websocket_upgrade() {
    let dir = seed();
    let (key, state) = authed(dir.path().to_path_buf());
    let res = build_router(state.clone())
        .oneshot(
            Request::get("/api/ws")
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::UNAUTHORIZED);
}

/// `check_bind_allowed` only enforces "non-loopback needs a key" once, at
/// startup. A server that starts that way but ends up with zero keys (either
/// because it was misconfigured, or because the last key was revoked while
/// running, see the transition test below) must fail closed rather than fall
/// back to the keyless pass-through: an RCE-equivalent panel reachable from
/// the network with no credential at all.
#[tokio::test]
async fn a_key_requiring_server_refuses_every_request_when_the_key_set_is_empty() {
    let dir = seed();
    let auth = Arc::new(
        AuthState::new(None, Vec::new(), &ServerConfig::default())
            .unwrap()
            .require_keys("0.0.0.0".parse().unwrap()),
    );
    let state = AppState::new(dir.path().to_path_buf()).with_auth(auth);

    let (status, body) = send(
        &state,
        Request::get("/api/projects").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "auth");
}

#[tokio::test]
async fn revoking_the_last_key_on_a_key_requiring_server_stops_the_keyless_pass_through() {
    let dir = seed();
    let keydir = tempfile::tempdir().unwrap();
    let path = keydir.path().join("server-auth.yaml");
    let (key, record) = server_auth::issue_into(&path).unwrap();
    let file = server_auth::load_from(&path).unwrap();
    let auth = Arc::new(
        AuthState::new(Some(path.clone()), file.keys, &ServerConfig::default())
            .unwrap()
            .require_keys("0.0.0.0".parse().unwrap()),
    );
    let state = AppState::new(dir.path().to_path_buf()).with_auth(auth);

    let bearer = |k: &str| {
        Request::get("/api/runs")
            .header("authorization", format!("Bearer {k}"))
            .body(Body::empty())
            .unwrap()
    };

    let (status, _) = send(&state, bearer(&key)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the only key works while it is live"
    );

    server_auth::revoke_in(&path, &record.id).unwrap();

    // The stale-but-cached key is still what a bearer attempt with it forces
    // a (content-hash) reload against, which is what drops the in-memory key
    // count to zero as a side effect of this one request; the ordinary
    // "unauthenticated" 401 path answers it either way.
    let (status, _) = send(&state, bearer(&key)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The real regression check: once the key set is empty, a request that
    // carries NO credential at all must still be refused. Under the bug this
    // fix closes, `auth.enabled()` reading false here would take the
    // keyless-pass-through branch and this would come back 200.
    let (status, body) = send(
        &state,
        Request::get("/api/projects").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "an empty key set on a key-requiring server refuses rather than passing \
         through: {body}"
    );
    assert_eq!(body["error"], "auth");
}
