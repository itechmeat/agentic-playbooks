//! `GET /api/playbooks/{id}/fit`: the "will it fit here?" report of a
//! project or global playbook for a run in the workspace's project.

use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use std::fs;
use std::path::Path;
use tower::ServiceExt;

use crate::common;

fn write_playbook(parent: &Path, id: &str, head: &str) {
    let vdir = parent.join("playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        format!(
            "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\n{head}nodes:\n  - {{ id: s, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: s, to: done }}\n"
        ),
    )
    .unwrap();
    fs::write(parent.join("playbooks").join(id).join("current"), "1.0.0").unwrap();
}

async fn get(router: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let res = router
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn reports_the_four_sections_for_project_and_global_playbooks() {
    let sandbox = common::config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    write_playbook(&dir.path().join(".apb"), "p", "");
    write_playbook(sandbox.path(), "g", "requires:\n  files: [CHANGELOG.md]\n");
    let router = build_router(AppState::new(dir.path().to_path_buf()));

    let (st, v) = get(&router, "/api/playbooks/p/fit").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["scope"], "project");
    assert_eq!(v["ready"], true);
    assert_eq!(v["trusted"], false, "nobody approved it");
    for section in ["compatibility", "setup", "trust"] {
        assert!(v[section].is_array(), "{section}");
    }
    assert!(v["consent"]["irreversible"].is_array());

    let (st, v) = get(&router, "/api/playbooks/g/fit?scope=global").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["scope"], "global");
    assert_eq!(v["ready"], false);
    assert_eq!(v["compatibility"][0]["code"], "requires_unmet");
}

#[tokio::test]
async fn unknown_playbooks_and_scopes_are_refused() {
    let _sandbox = common::config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let router = build_router(AppState::new(dir.path().to_path_buf()));
    let (st, _) = get(&router, "/api/playbooks/nope/fit").await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = get(&router, "/api/playbooks/nope/fit?scope=team").await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

/// A start that leaves a param without a value is refused with the same
/// start action the fit report shows for it, and nothing is written.
#[tokio::test]
async fn a_start_without_a_needed_param_is_refused_like_the_fit_report_says() {
    let _sandbox = common::config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    write_playbook(
        &dir.path().join(".apb"),
        "p",
        "params:\n  - { name: target, type: text }\n",
    );
    let router = build_router(AppState::new(dir.path().to_path_buf()));

    let (st, fit) = get(&router, "/api/playbooks/p/fit").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(fit["setup"][0]["code"], "param_needs_value");
    let fit_action = &fit["start_actions"][0];
    assert_eq!(fit_action["remedy"]["remedy"], "provide_param");

    let res = router
        .clone()
        .oneshot(
            Request::post("/api/playbooks/p/run")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let refusal: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(refusal["policy"], "param_needs_value", "{refusal}");
    assert_eq!(refusal["missing"], serde_json::json!(["target"]));
    let action = &refusal["start_actions"][0];
    for field in ["code", "remedy", "args", "principals"] {
        assert_eq!(action[field], fit_action[field], "{field}");
    }
    let runs = dir.path().join(".apb/runs");
    assert!(!runs.exists() || fs::read_dir(&runs).unwrap().next().is_none());
}
