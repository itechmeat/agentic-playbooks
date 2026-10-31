//! The run budget on the HTTP API: the estimate route, a start with a
//! budget, the 409 `budget_needs_decision` of a start the person must decide
//! first, the 409 `budget_paused` of a plain resume and a resume with a
//! raise, and the budget and estimate on the run detail. A stub agent that
//! reports 200 tokens per attempt seeds one history run; no model is called.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::budget::BudgetRequest;
use apb_engine::state::RunStatus;
use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::common;

const PLAYBOOK: &str = "schema: 2\nid: bud\nname: Budget\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: agent_task, prompt: a }\n  - { id: b, type: agent_task, prompt: b }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: b }\n  - { from: b, to: done }\n";

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(serde_json::to_vec(&b).unwrap())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Playbook `bud` (with `block` as its `budget:` line, if any), profile
/// `main` and the stub agent, in a fresh project; returns the stub path.
fn seed(root: &Path, block: &str) -> std::path::PathBuf {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/bud/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        PLAYBOOK.replace("nodes:\n", &format!("{block}nodes:\n")),
    )
    .unwrap();
    fs::write(root.join(".apb/playbooks/bud/current"), "1.0.0").unwrap();
    let pdir = root.join(".apb/profiles/main");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(
        pdir.join("profile.yaml"),
        "name: main\ndescription: test\nexecutor:\n  agent: claude-code\n  model: haiku\n",
    )
    .unwrap();
    fs::write(pdir.join("SOUL.md"), "").unwrap();
    let out = root.join("out.json");
    fs::write(
        &out,
        json!({
            "type": "result", "subtype": "success", "is_error": false,
            "result": "done\n\n```yaml\nstatus: success\nsummary: done\n```",
            "usage": {"input_tokens": 150, "output_tokens": 50}
        })
        .to_string(),
    )
    .unwrap();
    let stub = root.join("agent.sh");
    {
        use std::io::Write as _;
        let mut f = fs::File::create(&stub).unwrap();
        write!(
            f,
            "#!/bin/sh\n[ -n \"$APB_STATUS_FILE\" ] && printf '{{\"status\":\"success\"}}' > \"$APB_STATUS_FILE\"\ncat '{}'\n",
            out.display()
        )
        .unwrap();
        f.sync_all().unwrap();
    }
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    stub
}

fn run(root: &Path, budget: Option<BudgetRequest>) -> apb_engine::RunResult {
    apb_engine::run(
        root,
        "bud",
        None,
        apb_engine::RunOptions {
            budget,
            ..Default::default()
        },
    )
    .unwrap()
}

/// One history run, then a run with a 300-token budget paused before `b`.
fn paused_run(root: &Path) -> String {
    assert_eq!(run(root, None).outcome, RunStatus::Succeeded);
    let paused = run(
        root,
        Some(BudgetRequest {
            tokens: Some(300),
            ..BudgetRequest::default()
        }),
    );
    assert_eq!(paused.outcome, RunStatus::Paused);
    paused.run_id
}

#[tokio::test]
async fn estimate_route() {
    let _cfg = common::config_sandbox().await;
    apb_core::run_origin::ensure_key().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let stub = seed(dir.path(), "");
    let _agent = common::EnvVar::set("APB_AGENT_CMD", &stub);
    assert_eq!(run(dir.path(), None).outcome, RunStatus::Succeeded);
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, e) = call(&app, "GET", "/api/playbooks/bud/estimate", None).await;
    assert_eq!(status, StatusCode::OK, "{e}");
    assert_eq!(e["comparable"]["runs"], 1);
    assert_eq!(e["tokens"]["spread"]["p50"], 400);
    assert_eq!(e["nodes"][0]["node"], "a");
    assert!(e["summary"].as_str().unwrap().contains("tokens  ~400"));
    let (status, _) = call(&app, "GET", "/api/playbooks/ghost/estimate", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn run_with_budget() {
    let _cfg = common::config_sandbox().await;
    apb_core::run_origin::ensure_key().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let stub = seed(dir.path(), "");
    let _agent = common::EnvVar::set("APB_AGENT_CMD", &stub);
    assert_eq!(run(dir.path(), None).outcome, RunStatus::Succeeded);
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, out) = call(
        &app,
        "POST",
        "/api/playbooks/bud/run",
        Some(json!({ "budget": { "tokens": 1000 } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["budget"]["limit"]["tokens"], 1000);
    assert_eq!(out["budget"]["source"], "run");
    assert_eq!(out["estimate"]["comparable"]["runs"], 1);
    let (status, bad) = call(
        &app,
        "POST",
        "/api/playbooks/bud/run",
        Some(json!({ "budget": { "tokens": 0 } })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(bad["error"], "invalid_budget");
}

#[tokio::test]
async fn run_409_budget_needs_decision() {
    let _cfg = common::config_sandbox().await;
    apb_core::run_origin::ensure_key().unwrap();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "budget: { tokens: 1000 }\n");
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, out) = call(&app, "POST", "/api/playbooks/bud/run", Some(json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{out}");
    assert_eq!(out["error"], "budget_needs_decision");
    assert_eq!(out["unestimated"], json!(["a", "b"]));
    assert!(
        !dir.path().join(".apb/runs").exists()
            || fs::read_dir(dir.path().join(".apb/runs"))
                .unwrap()
                .next()
                .is_none()
    );
}

#[tokio::test]
async fn resume_body_budget() {
    let _cfg = common::config_sandbox().await;
    apb_core::run_origin::ensure_key().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let stub = seed(dir.path(), "");
    let _agent = common::EnvVar::set("APB_AGENT_CMD", &stub);
    let run_id = paused_run(dir.path());
    let _reaper = crate::driver_reaper::DriverReaper::new(dir.path(), &run_id);
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let uri = format!("/api/runs/{run_id}/resume");
    let (status, out) = call(&app, "POST", &uri, Some(json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{out}");
    assert_eq!(out["error"], "budget_paused");
    assert_eq!(out["budget"]["paused"]["node"], "b");
    let (status, out) = call(
        &app,
        "POST",
        &uri,
        Some(json!({ "budget": { "raise_tokens": 1000 } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["detached"], true);
}

#[tokio::test]
async fn run_detail_budget() {
    let _cfg = common::config_sandbox().await;
    apb_core::run_origin::ensure_key().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let stub = seed(dir.path(), "");
    let _agent = common::EnvVar::set("APB_AGENT_CMD", &stub);
    let run_id = paused_run(dir.path());
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, d) = call(&app, "GET", &format!("/api/runs/{run_id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(d["budget"]["limit"]["tokens"], 300);
    assert_eq!(d["budget"]["tokens"]["spent"], 200.0);
    assert_eq!(d["budget"]["paused"]["node"], "b");
    assert_eq!(d["pause"]["cause"], "budget");
    assert_eq!(d["estimate"]["comparable"]["runs"], 1);
}
