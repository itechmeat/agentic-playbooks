use apb_server::{AppState, build_router};
use std::fs;
use std::time::Duration;

#[tokio::test]
async fn watcher_publishes_on_file_change() {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let state = AppState::new(dir.path().to_path_buf());
    let mut rx = state.events.subscribe();
    let _watcher =
        apb_server::watch::spawn_watcher(dir.path().to_path_buf(), state.events.clone()).unwrap();
    // give the watcher time to initialize
    tokio::time::sleep(Duration::from_millis(300)).await;
    fs::create_dir_all(dir.path().join(".apb/playbooks/demo/1.0.0")).unwrap();
    fs::write(
        dir.path().join(".apb/playbooks/demo/1.0.0/playbook.yaml"),
        "id: demo",
    )
    .unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timeout waiting for event")
        .expect("channel closed");
    assert!(msg.contains("playbooks_changed"));
}

#[tokio::test]
async fn ws_route_exists() {
    // sanity: the /api/ws route responds with an upgrade error to a plain GET, not 404
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let res = app
        .oneshot(Request::get("/api/ws").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::NOT_FOUND);
}

/// A well-formed upgrade handshake, optionally carrying an `Origin`.
fn upgrade_request(origin: Option<&str>) -> axum::http::Request<axum::body::Body> {
    use axum::body::Body;
    use axum::http::Request;
    let mut b = Request::get("/api/ws")
        .header("host", "example.com")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==");
    if let Some(o) = origin {
        b = b.header("origin", o);
    }
    b.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn ws_upgrade_rejects_a_cross_origin_handshake() {
    use axum::http::StatusCode;
    use tower::ServiceExt;
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();

    // A mismatched Origin is refused before the upgrade.
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let res = app
        .oneshot(upgrade_request(Some("http://evil.example")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // A same-origin Origin passes the origin gate and reaches the upgrade
    // extractor. Under `oneshot` there is no upgradable connection, so that
    // extractor answers 426 rather than 101; the point here is only that the
    // origin gate did not refuse it.
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let res = app
        .oneshot(upgrade_request(Some("http://example.com")))
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(res.status(), StatusCode::UPGRADE_REQUIRED);

    // An absent Origin (a non-browser client) also passes the origin gate.
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let res = app.oneshot(upgrade_request(None)).await.unwrap();
    assert_ne!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(res.status(), StatusCode::UPGRADE_REQUIRED);
}
