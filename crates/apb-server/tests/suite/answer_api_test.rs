//! Task 12 (spec 2026-07-20-interactive-nodes): the web facade for
//! interactive-node questions. Mirrors `runs_api_test.rs`'s `post_review_*`
//! tests exactly, but for `POST /api/runs/{id}/answer` and the run detail's
//! `progress.pending_question`.

use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::fs;
use tower::ServiceExt;

const INTERACTIVE_WF: &str = r#"
schema: 2
id: iq
name: IQ
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: start, type: start }
  - { id: ask, type: agent_task, prompt: "ask something", interactive: true }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: ask }
  - { from: ask, to: done }
"#;

/// A run dir seeded directly (no live agent needed): a playbook snapshot plus
/// a `RunStarted` event, mirroring what `apb_engine::run` would have written
/// up to the point drive parks on `ask`'s question. `questions.jsonl` is
/// seeded through the real `post_question` channel writer, exactly the shape
/// `progress::pending_question_for_run` reads (spec: visible before drive
/// ever journals `QuestionAsked`).
fn seed_with_pending_question() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let run_id = "run-iq-1".to_string();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("playbook.yaml"), INTERACTIVE_WF).unwrap();

    let mut log = apb_engine::event::EventLog::create(&run_dir).unwrap();
    log.append(apb_engine::event::EventPayload::RunStarted {
        playbook: "iq".into(),
        version: "1.0.0".into(),
    })
    .unwrap();

    apb_engine::question::post_question(
        &run_dir,
        "ask",
        1,
        "which way",
        vec!["left".into(), "right".into()],
    )
    .unwrap();

    (dir, run_id)
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
async fn run_detail_exposes_pending_question() {
    let (dir, run_id) = seed_with_pending_question();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, &format!("/api/runs/{run_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["progress"]["waiting_kind"], "question");
    let pq = &json["progress"]["pending_question"];
    assert_eq!(pq["node"], "ask");
    assert_eq!(pq["question"], "which way");
    assert_eq!(pq["options"], serde_json::json!(["left", "right"]));
    assert_eq!(pq["answer_by"], "human");
}

#[tokio::test]
async fn post_answer_writes_channel_and_clears_pending_question() {
    let (dir, run_id) = seed_with_pending_question();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = post_json(
        app.clone(),
        &format!("/api/runs/{run_id}/answer"),
        serde_json::json!({ "node": "ask", "answer": "left" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["posted_seq"].is_number());

    let channel = fs::read_to_string(
        dir.path()
            .join(".apb/runs")
            .join(&run_id)
            .join("answers.jsonl"),
    )
    .unwrap();
    assert!(channel.contains("left"));
    assert!(channel.contains("human"));

    let (_, detail) = get_json(app, &format!("/api/runs/{run_id}")).await;
    assert_eq!(detail["progress"]["waiting_kind"], serde_json::Value::Null);
    assert_eq!(
        detail["progress"]["pending_question"],
        serde_json::Value::Null
    );
}

#[tokio::test]
async fn post_answer_omitted_node_resolves_the_single_pending_node() {
    let (dir, run_id) = seed_with_pending_question();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        &format!("/api/runs/{run_id}/answer"),
        serde_json::json!({ "answer": "right" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["posted_seq"].is_number());
    let channel = fs::read_to_string(
        dir.path()
            .join(".apb/runs")
            .join(&run_id)
            .join("answers.jsonl"),
    )
    .unwrap();
    assert!(channel.contains("\"node\":\"ask\""));
    assert!(channel.contains("right"));
}

#[tokio::test]
async fn post_answer_with_no_pending_question_surfaces_engine_message() {
    // No `post_question` seeded: `resolve_pending_node` has nothing to
    // default to, so `apb_engine::post_answer` returns `EngineError::NotFound`
    // and the handler must surface that message verbatim as the body, exactly
    // like `post_review_handler` does for its own engine errors.
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let run_id = "run-empty-1".to_string();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("playbook.yaml"), INTERACTIVE_WF).unwrap();

    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/runs/{run_id}/answer"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({ "answer": "left" })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(
        text.contains("no pending question"),
        "the engine's policy/resolution message must be surfaced verbatim: {text}"
    );
}

#[tokio::test]
async fn post_answer_unknown_run_404() {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = post_json(
        app,
        "/api/runs/ghost-1/answer",
        serde_json::json!({ "answer": "left" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn post_answer_path_traversal_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = post_json(
        app,
        "/api/runs/..%2F..%2Fetc/answer",
        serde_json::json!({ "answer": "left" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
