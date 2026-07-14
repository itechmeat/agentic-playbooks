use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::fs;
use tower::ServiceExt;

const NOAGENT: &str = r#"
schema: 1
id: noagent
name: No Agent
version: 1.0.0
params:
  - { name: who, type: text }
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hi {{params.who}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

fn seed_with_run() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".apb/playbooks/noagent/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), NOAGENT).unwrap();
    fs::write(dir.path().join(".apb/playbooks/noagent/current"), "1.0.0").unwrap();
    // a real run without an agent, through the engine
    let mut opts = apb_engine::RunOptions::default();
    opts.params.insert("who".into(), "world".into());
    apb_engine::run(dir.path(), "noagent", None, opts).unwrap();
    dir
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
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
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn post_review_writes_channel() {
    let dir = seed_with_run();
    let run_id = apb_engine::list_runs(dir.path()).unwrap()[0].run_id.clone();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        &format!("/api/runs/{run_id}/review"),
        serde_json::json!({ "node": "gate", "decision": "approved", "note": "ok" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["posted_seq"].is_number());
    let channel = fs::read_to_string(
        dir.path()
            .join(".apb/runs")
            .join(&run_id)
            .join("reviews.jsonl"),
    )
    .unwrap();
    assert!(channel.contains("approved"));
}

const WEBHOOK_WF: &str = r#"
schema: 1
id: hooky
name: Hooky
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: wait, type: wait, wait_for: { type: webhook, key: ci }, timeout_seconds: 60 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: wait }
  - { from: wait, to: done }
"#;

// A run parked on webhook-wait: prepare generates hooks.json, drive waits
// for the signal in the background (we don't send it - a running run is
// enough for the endpoint test).
fn seed_webhook_run() -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".apb/playbooks/hooky/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), WEBHOOK_WF).unwrap();
    fs::write(dir.path().join(".apb/playbooks/hooky/current"), "1.0.0").unwrap();
    let root = dir.path().to_path_buf();
    std::thread::spawn(move || {
        let _ = apb_engine::run(&root, "hooky", None, apb_engine::RunOptions::default());
    });
    // Wait for the run and its hooks.json to appear.
    let run_id = loop {
        let found = fs::read_dir(dir.path().join(".apb/runs"))
            .ok()
            .and_then(|rd| {
                rd.filter_map(|e| e.ok())
                    .find(|e| e.path().is_dir() && e.path().join("hooks.json").is_file())
                    .map(|e| e.file_name().to_string_lossy().to_string())
            });
        if let Some(id) = found {
            break id;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let hooks: std::collections::BTreeMap<String, String> = serde_json::from_str(
        &fs::read_to_string(
            dir.path()
                .join(".apb/runs")
                .join(&run_id)
                .join("hooks.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let secret = hooks.get("ci").unwrap().clone();
    (dir, run_id, secret)
}

#[tokio::test]
async fn post_hook_with_valid_secret_signals() {
    let (dir, run_id, secret) = seed_webhook_run();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        &format!("/api/hooks/{run_id}/{secret}"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["signalled"], "ci");
    let channel = fs::read_to_string(
        dir.path()
            .join(".apb/runs")
            .join(&run_id)
            .join("signals.jsonl"),
    )
    .unwrap();
    assert!(channel.contains("ci"));
}

#[tokio::test]
async fn post_hook_with_wrong_secret_404() {
    let (dir, run_id, _secret) = seed_webhook_run();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = post_json(
        app,
        &format!("/api/hooks/{run_id}/deadbeef"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn run_detail_exposes_hooks() {
    let (dir, run_id, _secret) = seed_webhook_run();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, &format!("/api/runs/{run_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        json["hooks"]["ci"]
            .as_str()
            .unwrap()
            .starts_with(&format!("/api/hooks/{run_id}/"))
    );
}

#[tokio::test]
async fn post_review_unknown_run_404() {
    let dir = seed_with_run();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = post_json(
        app,
        "/api/runs/ghost-1/review",
        serde_json::json!({ "node": "gate", "decision": "approved" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn post_review_path_traversal_is_rejected() {
    let dir = seed_with_run();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = post_json(
        app,
        "/api/runs/..%2F..%2Fetc/review",
        serde_json::json!({ "node": "gate", "decision": "approved" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn lists_runs() {
    let dir = seed_with_run();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/runs").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json[0]["playbook"], "noagent");
    assert_eq!(json[0]["status"], "succeeded");
}

#[tokio::test]
async fn run_detail_has_statuses_and_events() {
    let dir = seed_with_run();
    let run_id = apb_engine::list_runs(dir.path()).unwrap()[0].run_id.clone();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, &format!("/api/runs/{run_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["run_status"], "succeeded");
    assert_eq!(json["nodes"]["note"], "succeeded");
    assert_eq!(json["model"]["nodes"][0]["type"], "start");
    assert!(json["events"].as_array().unwrap().len() >= 3);
}

#[tokio::test]
async fn unknown_run_404() {
    let dir = seed_with_run();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = get_json(app, "/api/runs/ghost-1").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn run_id_path_traversal_is_rejected() {
    let dir = seed_with_run();

    // A target file outside the project directory that must not be accessible.
    let secret_dir = dir.path().parent().unwrap().join("etc");
    fs::create_dir_all(&secret_dir).unwrap();
    fs::write(secret_dir.join("playbook.yaml"), "schema: 1\nid: leaked\n").unwrap();

    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = get_json(app.clone(), "/api/runs/..%2F..%2Fetc").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = get_json(app.clone(), "/api/runs/%2Fetc%2Fpasswd").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A legitimate id keeps working as before.
    let run_id = apb_engine::list_runs(dir.path()).unwrap()[0].run_id.clone();
    let (status, _) = get_json(app, &format!("/api/runs/{run_id}")).await;
    assert_eq!(status, StatusCode::OK);
}
