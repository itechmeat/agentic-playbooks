//! /api/agents, /api/models, /api/skills - the read endpoints that feed the
//! profile form's agent/model combobox and skills toggle list. Mutates
//! process-wide env (APB_CONFIG_DIR / HOME / probe timeout), so it takes
//! `common::env_lock()` to serialize against other env-mutating tests in
//! this consolidated binary (see `crate::common`).

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

#[tokio::test]
async fn agents_models_and_skills_endpoints() {
    let _guard = crate::common::env_lock().await;
    let proj = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
        std::env::set_var("HOME", home.path());
        // Keep detection fast: a missing binary resolves instantly, but cap the
        // probe timeout so a present one cannot stall the test.
        std::env::set_var("APB_PROBE_TIMEOUT_MS", "300");
    }
    apb_core::registry::init_project(proj.path()).unwrap();

    // A project skill and a global skill, plus an invalid-named dir to confirm
    // it is filtered out.
    let proj_skills = proj.path().join(".agents/skills");
    std::fs::create_dir_all(proj_skills.join("proj-skill")).unwrap();
    std::fs::create_dir_all(proj_skills.join("Bad Name")).unwrap();
    let global_skills = home.path().join(".agents/skills");
    std::fs::create_dir_all(global_skills.join("glob-skill")).unwrap();

    let root = proj.path().to_path_buf();

    // /api/agents: the six built-in probes are always enumerated.
    let app = build_router(AppState::new(root.clone()));
    let (status, json) = get_json(app, "/api/agents").await;
    assert_eq!(status, StatusCode::OK);
    let agents = json["agents"].as_array().expect("agents array");
    assert_eq!(agents.len(), 6, "expected the six built-in probes: {json}");
    assert!(agents.iter().any(|a| a["agent"] == "claude"));

    // /api/models: the curated table and the claude static list.
    let app = build_router(AppState::new(root.clone()));
    let (status, json) = get_json(app, "/api/models").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !json["models"].as_array().expect("models array").is_empty(),
        "models table must not be empty: {json}"
    );
    assert!(!json["claude_static"].as_array().unwrap().is_empty());

    // /api/skills project scope: project skills first, then global; invalid
    // names filtered.
    let app = build_router(AppState::new(root.clone()));
    let (status, json) = get_json(app, "/api/skills?scope=project").await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = json["skills"]
        .as_array()
        .expect("skills array")
        .iter()
        .map(|s| s["name"].as_str().unwrap_or(""))
        .collect();
    assert!(
        names.contains(&"proj-skill"),
        "project skill listed: {json}"
    );
    assert!(
        names.contains(&"glob-skill"),
        "global skill visible in project scope: {json}"
    );
    assert!(
        !names.contains(&"Bad Name"),
        "invalid name filtered: {json}"
    );

    // /api/skills global scope: only global skills.
    let app = build_router(AppState::new(root.clone()));
    let (status, json) = get_json(app, "/api/skills?scope=global").await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = json["skills"]
        .as_array()
        .expect("skills array")
        .iter()
        .map(|s| s["name"].as_str().unwrap_or(""))
        .collect();
    assert!(names.contains(&"glob-skill"));
    assert!(
        !names.contains(&"proj-skill"),
        "project skill must not leak into global scope: {json}"
    );
}
