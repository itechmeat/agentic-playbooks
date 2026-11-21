//! The template routes over HTTP: `GET /api/templates`, one template, and
//! `POST /api/templates/{id}/install` into either scope through the one
//! install entry the CLI uses.

use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::json;
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
async fn get_templates_lists_them() {
    let app = build_router(AppState::new_global());
    let (status, json) = get_json(app, "/api/templates").await;
    assert_eq!(status, StatusCode::OK);
    let list = json["templates"].as_array().unwrap();
    assert!(list.len() >= 15, "{} templates", list.len());
    assert!(list.iter().any(|t| t["id"] == "dependency-security-fix"));
}

#[tokio::test]
async fn get_template_shows_the_yaml() {
    let app = build_router(AppState::new_global());
    let (status, json) = get_json(app, "/api/templates/inbox-triage").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["template"]["id"], "inbox-triage");
    assert!(json["yaml"].as_str().unwrap().contains("id: inbox-triage"));
}

/// A project server: the install answers the draft, created under the
/// project's `.apb`.
#[tokio::test]
async fn post_install_creates_a_draft() {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = post_json(app, "/api/templates/inbox-triage/install", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["lifecycle"], "draft");
    assert_eq!(json["scope"], "project");
    assert!(
        dir.path()
            .join(".apb/playbooks/inbox-triage/1.0.0/playbook.yaml")
            .is_file()
    );
}

#[tokio::test]
async fn post_install_twice_is_409() {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let body = json!({});
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = post_json(app, "/api/templates/inbox-triage/install", body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = post_json(app, "/api/templates/inbox-triage/install", body).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json["rejected"], "duplicate_id");
}

#[tokio::test]
async fn an_unknown_template_is_404() {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = get_json(app, "/api/templates/no-such-template").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = post_json(app, "/api/templates/no-such-template/install", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// No pinned root and no registered project: a global install still
/// answers, without a fit report, and says no project was named.
#[tokio::test]
async fn a_global_install_with_no_project_has_no_fit_report() {
    let _cfg = crate::common::config_sandbox().await;
    let app = build_router(AppState::new_global());
    let (status, json) = post_json(
        app,
        "/api/templates/inbox-triage/install",
        json!({ "scope": "global" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert!(json["fit"].is_null());
    assert_eq!(json["scope"], "global");
    let warnings = json["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| w
            .as_str()
            .is_some_and(|s| s.contains("no project was named"))),
        "{warnings:?}"
    );
}

#[tokio::test]
async fn a_global_install_lands_in_the_global_store() {
    let cfg = crate::common::config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        "/api/templates/inbox-triage/install",
        json!({ "scope": "global" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["scope"], "global");
    assert!(
        cfg.path()
            .join("playbooks/inbox-triage/1.0.0/playbook.yaml")
            .is_file()
    );
    assert!(!dir.path().join(".apb/playbooks/inbox-triage").exists());
}
