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
