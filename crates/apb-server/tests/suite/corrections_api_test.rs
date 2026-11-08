//! The corrections ledger on the HTTP API: the read, a person's fixed,
//! dismiss and reopen, a run step's refusal, and the unknown action or
//! body field.

use crate::common;
use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::fs;
use tower::ServiceExt;

const PLAYBOOK: &str = "schema: 2\nid: corr\nname: Corr\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: do the work }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n";

/// Seeds the project and a ledger with one proposed entry.
fn seed() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".apb/playbooks/corr/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(dir.path().join(".apb/playbooks/corr/current"), "1.0.0").unwrap();
    // A verifiable origin, so the ledger file is this installation's.
    apb_core::run_origin::ensure_key().unwrap();
    let id = "t:0123456789abcdef";
    apb_core::signed_store::update::<
        apb_core::corrections::CorrectionFile,
        _,
        apb_core::signed_store::StoreError,
    >(dir.path(), apb_core::scope::Scope::Project, "corr", |f| {
        let s = apb_core::corrections::Sighting {
            kind: apb_core::corrections::CorrectionKind::ReviewNote,
            node: Some("work".into()),
            gate: None,
            key: id.into(),
            text: Some("add tests for the parser".into()),
            run_id: "corr-1".into(),
            seq: 5,
            at_ms: 1000,
            version: "1.0.0".into(),
            via_supervisor: false,
        };
        f.observe(
            apb_core::corrections::ObservedRun {
                run_id: "corr-1".into(),
                version: "1.0.0".into(),
                started_ms: 1000,
            },
            vec![s.clone()],
            1000,
        );
        f.observe(
            apb_core::corrections::ObservedRun {
                run_id: "corr-2".into(),
                version: "1.0.0".into(),
                started_ms: 2000,
            },
            vec![s],
            2000,
        );
        Ok(((), true))
    })
    .unwrap();
    dir
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
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).to_string()));
    (status, json)
}

async fn get(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).to_string()));
    (status, json)
}

#[tokio::test]
async fn the_read_answers_the_entries_and_a_fixed_action_writes() {
    let _e = common::env_lock().await;
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get(app, "/api/playbooks/corr/corrections").await;
    assert_eq!(status, StatusCode::OK, "{json:#}");
    assert_eq!(json["signed"], true);
    assert_eq!(json["entries"].as_array().map(Vec::len), Some(1));
    let entry = json["entries"][0]["correction"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(json["entries"][0]["correction"]["status"], "proposed");
    // The draft names the node and quotes the wording.
    assert_eq!(json["entries"][0]["draft"]["node"], "work");

    // Mark the fix on the version the store has.
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        &format!("/api/playbooks/corr/corrections/{entry}/fixed"),
        serde_json::json!({ "version": "1.0.0" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json:#}");
    assert_eq!(json["correction"]["status"], "fixed");
    assert_eq!(json["rate"]["base_runs"], 2);

    // The ledger file holds the fix record.
    let bytes = fs::read_to_string(dir.path().join(".apb/corrections/project/corr.json")).unwrap();
    assert!(bytes.contains("1.0.0"));
}

/// A run step is refused (409) on the read and the write.
#[tokio::test]
async fn a_run_step_is_refused() {
    let _e = common::env_lock().await;
    let _g = common::EnvVar::set("APB_RUN_ID", "corr-9");
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get(app, "/api/playbooks/corr/corrections").await;
    assert_eq!(status, StatusCode::CONFLICT, "{json:#}");
    assert!(json.to_string().contains("reading or deciding corrections"));

    // The entry id is `c-` + the first 12 hex of sha256(key): deterministic
    // without a read (the read is refused here).
    let entry = apb_core::corrections::entry_id("t:0123456789abcdef");
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        &format!("/api/playbooks/corr/corrections/{entry}/dismiss"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json:#}");
    assert!(
        json.to_string().contains("deciding corrections")
            && json.to_string().contains("do it outside a run"),
        "{json:#}"
    );
}

/// An unknown action is 422; an unknown body field is 422; an unknown
/// version for fixed is 409.
#[tokio::test]
async fn unknown_action_field_and_version() {
    let _e = common::env_lock().await;
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        "/api/playbooks/corr/corrections/e1/promote",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{json:#}");
    assert!(json.to_string().contains("unknown action"), "{json:#}");

    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = post_json(
        app,
        "/api/playbooks/corr/corrections/e1/dismiss",
        serde_json::json!({ "reasons": "no" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // fixed with a version the store does not have: 409.
    let entry = {
        let app = build_router(AppState::new(dir.path().to_path_buf()));
        let (_, json) = get(app, "/api/playbooks/corr/corrections").await;
        json["entries"][0]["correction"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        &format!("/api/playbooks/corr/corrections/{entry}/fixed"),
        serde_json::json!({ "version": "9.9.9" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json:#}");
    assert!(json.to_string().contains("unknown_version"), "{json:#}");

    // fixed without a version: 422.
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = post_json(
        app,
        &format!("/api/playbooks/corr/corrections/{entry}/fixed"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}
