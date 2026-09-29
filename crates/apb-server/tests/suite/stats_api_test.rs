//! `GET /api/stats` (C3): the cross-run report over one project's runs.

use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::fs;
use tower::ServiceExt;

use crate::common;

const NOAGENT: &str = r#"
schema: 2
id: noagent
name: No Agent
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hi" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn stats_reports_the_runs_of_a_playbook_and_an_empty_project() {
    let _cfg = common::config_sandbox().await;
    // The stamp key: only runs apb created on this machine are counted.
    apb_core::run_origin::ensure_key().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    apb_core::registry::init_project(root).unwrap();
    let app = build_router(AppState::new(root.to_path_buf()));
    let (status, empty) = get_json(app.clone(), "/api/stats?playbook=noagent").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty["runs"], 0);
    assert_eq!(empty["note"], "no runs recorded");

    let vdir = root.join(".apb/playbooks/noagent/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), NOAGENT).unwrap();
    fs::write(root.join(".apb/playbooks/noagent/current"), "1.0.0").unwrap();
    for _ in 0..2 {
        apb_engine::run(root, "noagent", None, apb_engine::RunOptions::default()).unwrap();
    }
    let (status, report) = get_json(app.clone(), "/api/stats?playbook=noagent&since=30d").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(report["runs"], 2);
    let v = &report["versions"][0];
    assert_eq!(v["version"], "1.0.0");
    assert_eq!(
        v["success"],
        serde_json::json!({"count": 2, "of": 2, "rate": 1.0})
    );
    assert_eq!(v["first_pass"]["count"], 2);
    let (status, _) = get_json(app, "/api/stats?since=soon").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
