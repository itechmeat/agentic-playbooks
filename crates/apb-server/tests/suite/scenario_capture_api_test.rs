//! `POST /api/runs/{id}/scenario-capture`: the preview and the digest-bound
//! save over the shared tool layer; 409 from a run step and for a refusal,
//! 422 for an unknown field.

use crate::common;
use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::fs;
use tower::ServiceExt;

const PLAYBOOK: &str = "schema: 2\nid: cap\nname: cap\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: \"Fix {{run.instruction}}\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }\n";

/// A finished production run of `cap`, hand journaled.
fn seed_run(root: &std::path::Path) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/cap/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/cap/current"), "1.0.0").unwrap();
    let dir = root.join(".apb/runs/cap-1");
    fs::create_dir_all(&dir).unwrap();
    let lines = [
        r#"{"seq":1,"ts":1,"type":"run_started","playbook":"cap","version":"1.0.0"}"#,
        r#"{"seq":2,"ts":1,"type":"node_started","node":"start","attempt":1}"#,
        r#"{"seq":3,"ts":1,"type":"node_finished","node":"start","status":"succeeded","attempt":1,"output":""}"#,
        r#"{"seq":4,"ts":1,"type":"node_started","node":"work","attempt":1}"#,
        r#"{"seq":5,"ts":1,"type":"attempt_started","node":"work","attempt":1,"agent":"claude-code"}"#,
        r#"{"seq":6,"ts":1,"type":"attempt_finished","node":"work","attempt":1,"status":"succeeded","output":"fixed the parser"}"#,
        r#"{"seq":7,"ts":1,"type":"node_finished","node":"work","status":"succeeded","attempt":1,"output":"fixed the parser"}"#,
        r#"{"seq":8,"ts":1,"type":"node_finished","node":"done","status":"succeeded","attempt":1,"output":""}"#,
        r#"{"seq":9,"ts":1,"type":"run_finished","outcome":"succeeded"}"#,
    ];
    fs::write(dir.join("events.jsonl"), lines.join("\n") + "\n").unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(
        dir.join("run.yaml"),
        "params: {}\ninstruction: the parser\nunattended: false\n",
    )
    .unwrap();
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

#[tokio::test]
async fn the_preview_answers_and_the_save_writes_with_the_digest() {
    let _e = common::env_lock().await;
    let dir = tempfile::tempdir().unwrap();
    seed_run(dir.path());
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        "/api/runs/cap-1/scenario-capture",
        serde_json::json!({ "name": "green-after-one-fix" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json:#}");
    assert_eq!(json["playbook"], "cap");
    assert!(
        json["yaml"]
            .as_str()
            .unwrap()
            .contains("green-after-one-fix")
    );
    let digest = json["digest"].as_str().unwrap().to_string();
    assert!(
        !dir.path()
            .join(".apb/playbooks/cap/evals/scenarios/green-after-one-fix.yaml")
            .exists()
    );

    // The save with the preview's digest writes the file.
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        "/api/runs/cap-1/scenario-capture",
        serde_json::json!({ "name": "green-after-one-fix", "save": true, "digest": digest }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json:#}");
    let saved = json["saved"].as_str().unwrap().to_string();
    assert!(saved.ends_with("green-after-one-fix.yaml"), "{saved}");
    assert!(
        fs::read_to_string(&saved)
            .unwrap()
            .contains("id: green-after-one-fix")
    );
}

/// A save whose digest is not the capture's is 409 `stale_preview`, and a
/// second save of one name is 409 `scenario_exists`.
#[tokio::test]
async fn stale_and_existing_saves_are_conflicts() {
    let _e = common::env_lock().await;
    let dir = tempfile::tempdir().unwrap();
    seed_run(dir.path());
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        "/api/runs/cap-1/scenario-capture",
        serde_json::json!({ "name": "green-after-one-fix", "save": true, "digest": "sha256:other" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json:#}");
    assert!(
        json["error"]
            .as_str()
            .unwrap_or_default()
            .contains("stale_preview")
            || json.to_string().contains("stale_preview"),
        "{json:#}"
    );

    // A first save through the tool layer, then a second one over HTTP.
    let preview = apb_mcp::tools::scenario_capture::capture(
        dir.path(),
        "cap-1",
        &apb_mcp::tools::scenario_capture::CaptureArgs {
            name: "green-after-one-fix".into(),
            nodes: Vec::new(),
            save: false,
            digest: None,
        },
        apb_core::launch::floor::SaveChannel::Person,
        &apb_core::step_env::StepCaller::from_env(),
    )
    .unwrap();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        "/api/runs/cap-1/scenario-capture",
        serde_json::json!({
            "name": "green-after-one-fix",
            "save": true,
            "digest": preview.digest,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json:#}");
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        "/api/runs/cap-1/scenario-capture",
        serde_json::json!({
            "name": "green-after-one-fix",
            "save": true,
            "digest": preview.digest,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json:#}");
    assert!(json.to_string().contains("scenario_exists"), "{json:#}");
}

/// An unknown field is 422; a refusal is 409 with the gap list.
#[tokio::test]
async fn unknown_field_is_422_and_a_refusal_is_409() {
    let _e = common::env_lock().await;
    let dir = tempfile::tempdir().unwrap();
    seed_run(dir.path());
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = post_json(
        app,
        "/api/runs/cap-1/scenario-capture",
        serde_json::json!({ "name": "green-after-one-fix", "what": "?" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // A stopped run is refused `aborted` with every gap named.
    fs::write(
        dir.path().join(".apb/runs/cap-1/events.jsonl"),
        r#"{"seq":1,"ts":1,"type":"run_started","playbook":"cap","version":"1.0.0"}
{"seq":2,"ts":1,"type":"run_aborted","reason":"person","by":"cli"}
"#,
    )
    .unwrap();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        "/api/runs/cap-1/scenario-capture",
        serde_json::json!({ "name": "green-after-one-fix" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json:#}");
    let text = json.to_string();
    assert!(
        text.contains("not_capturable") && text.contains("aborted"),
        "{text}"
    );
}

/// A run step's capture is 409.
#[tokio::test]
async fn a_run_step_is_refused() {
    let _e = common::env_lock().await;
    let _g = common::EnvVar::set("APB_RUN_ID", "cap-1");
    let dir = tempfile::tempdir().unwrap();
    seed_run(dir.path());
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        "/api/runs/cap-1/scenario-capture",
        serde_json::json!({ "name": "green-after-one-fix" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json:#}");
    assert!(
        json.to_string().contains("capturing a scenario"),
        "{json:#}"
    );
}

/// The dashboard posts with `?workspace=<id>`: a run of a registered
/// project that is not the server's own root is captured there.
#[tokio::test]
async fn the_capture_follows_the_workspace_query() {
    let sandbox = common::config_sandbox().await;
    let _cfg = sandbox.path();
    // Registration is off on CI and with APB_NO_REGISTRY; turn both off
    // for this test and restore them after.
    let prior: Vec<_> = ["CI", "APB_NO_REGISTRY"]
        .into_iter()
        .map(|k| (k, std::env::var_os(k)))
        .collect();
    for (k, _) in &prior {
        // SAFETY: the sandbox holds the env lock for this test's whole life.
        unsafe { std::env::remove_var(k) };
    }
    let dir = tempfile::tempdir().unwrap();
    seed_run(dir.path());
    apb_core::projects::touch(dir.path());
    let ws = fs::read_to_string(dir.path().join(".apb/workspace.local"))
        .unwrap()
        .trim()
        .to_string();
    let app = build_router(AppState::new_global());
    let (status, json) = post_json(
        app,
        &format!("/api/runs/cap-1/scenario-capture?workspace={ws}"),
        serde_json::json!({ "name": "from-the-workspace" }),
    )
    .await;
    for (k, v) in prior {
        if let Some(v) = v {
            // SAFETY: still under the sandbox's env lock.
            unsafe { std::env::set_var(k, v) };
        }
    }
    assert_eq!(status, StatusCode::OK, "{json:#}");
    assert_eq!(json["run_id"], "cap-1");
}
