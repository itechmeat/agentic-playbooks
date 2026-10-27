//! `GET|PUT|DELETE /api/playbooks/{id}/memory`: the remembered run input of
//! a playbook in this project, and `remember` on a dashboard run start.

use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::fs;
use tower::ServiceExt;

const PLAYBOOK: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nparams:\n  - { name: channel, type: enum, options: [beta, stable] }\n  - { name: token, type: text, secret: true }\nnodes:\n  - { id: s, type: start }\nedges: []\n";

fn seed() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".apb/playbooks/p/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(dir.path().join(".apb/playbooks/p/current"), "1.0.0").unwrap();
    dir
}

async fn call(router: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn put(uri: &str, body: Value) -> Request<Body> {
    Request::put(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

#[tokio::test]
async fn memory_round_trips_and_refuses_secrets() {
    let dir = seed();
    let router = build_router(AppState::new(dir.path().to_path_buf()));

    let (st, v) = call(&router, get("/api/playbooks/p/memory")).await;
    assert_eq!(st, StatusCode::OK);
    assert!(v["instruction"].is_null());
    assert_eq!(v["instruction_source"], "none");
    assert_eq!(v["version"], "1.0.0");

    let (st, v) = call(
        &router,
        put(
            "/api/playbooks/p/memory",
            json!({ "params": { "channel": "beta", "token": "plain" }, "instruction": "since the last tag" }),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["stored"], json!(["channel", "instruction"]));
    assert_eq!(
        v["refused"],
        json!([{ "name": "token", "reason": "secret_param" }])
    );

    let (_, v) = call(&router, get("/api/playbooks/p/memory")).await;
    assert_eq!(v["instruction"], "since the last tag");
    assert_eq!(v["instruction_source"], "memory");
    assert_eq!(
        v["values"],
        json!([{ "name": "channel", "value": "beta", "status": { "status": "compatible" } }])
    );
    let stored = fs::read_to_string(dir.path().join(".apb/memory/project/p.json")).unwrap();
    assert!(
        !stored.contains("plain"),
        "a secret param never reaches disk"
    );

    let (st, v) = call(
        &router,
        Request::delete("/api/playbooks/p/memory?what=instruction")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["cleared"], "instruction");
    let (_, v) = call(&router, get("/api/playbooks/p/memory")).await;
    assert!(v["instruction"].is_null());
    assert_eq!(v["values"].as_array().unwrap().len(), 1, "params stay");
}

#[tokio::test]
async fn bad_requests_are_answered_not_stored() {
    let dir = seed();
    let router = build_router(AppState::new(dir.path().to_path_buf()));
    let (st, _) = call(&router, get("/api/playbooks/missing/memory")).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call(&router, get("/api/playbooks/p/memory?scope=elsewhere")).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(
        &router,
        Request::delete("/api/playbooks/p/memory?what=everything")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(&router, get("/api/playbooks/p/input-draft")).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "the input-draft route is gone");
    // A save is always checked against the current version.
    let (st, _) = call(
        &router,
        put(
            "/api/playbooks/p/memory?version=1.0.0",
            json!({ "instruction": "x" }),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(!dir.path().join(".apb/memory/project/p.json").exists());
}
