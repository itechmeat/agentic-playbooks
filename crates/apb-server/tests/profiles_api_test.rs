//! /api/profiles: the list of profiles with trust status and creation via
//! POST. The only test in this binary (its own cargo process), so the env
//! (APB_CONFIG_DIR/HOME) is set without a Mutex - there is no race with
//! other tests. POST auto-approves the bundle in the TrustStore, so we
//! isolate the config into a temp directory.

use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

async fn post_json(
    app: axum::Router,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn profiles_list_then_create_then_trusted() {
    let proj = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
        std::env::set_var("HOME", home.path());
    }
    apb_core::registry::init_project(proj.path()).unwrap();
    // Seed a profile on disk directly.
    let dir = proj.path().join(".apb/profiles/seeded");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("profile.yaml"),
        "name: seeded\ndescription: d\nexecutor:\n  agent: claude\n  model: haiku\n",
    )
    .unwrap();
    std::fs::write(dir.join("SOUL.md"), "").unwrap();

    let root = proj.path().to_path_buf();

    // GET lists the seeded profile.
    let app = build_router(AppState::new(root.clone()));
    let (status, json) = get_json(app, "/api/profiles").await;
    assert_eq!(status, StatusCode::OK);
    let profiles = json["profiles"].as_array().expect("profiles array");
    assert!(
        profiles.iter().any(|p| p["name"] == "seeded"),
        "GET must list the seeded profile: {json}"
    );

    // POST creates a new profile (auto-approves its bundle).
    let app = build_router(AppState::new(root.clone()));
    let (status, created) = post_json(
        app,
        "/api/profiles",
        serde_json::json!({
            "name": "made",
            "scope": "project",
            "agent": "claude",
            "model": "sonnet",
            "description": "via api"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "POST failed: {created}");
    assert!(
        proj.path()
            .join(".apb/profiles/made/profile.yaml")
            .is_file()
    );

    // Subsequent GET shows it as trusted (bundle auto-approved on create).
    let app = build_router(AppState::new(root.clone()));
    let (_s, json) = get_json(app, "/api/profiles").await;
    let made = json["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "made")
        .expect("made profile present");
    assert_eq!(
        made["trusted"],
        serde_json::json!(true),
        "created profile must be trusted: {made}"
    );

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
        std::env::remove_var("HOME");
    }
}
