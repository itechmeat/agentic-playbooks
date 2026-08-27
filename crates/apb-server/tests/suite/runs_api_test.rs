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

/// A playbook with a `script` node: the only kind (besides `agent_task`,
/// `finish`-with-prompt, and `playbook`) that takes the workdir lock
/// (`NodeKind::takes_workdir_lock`), so starting it is the minimal shape that
/// exercises `workdir::acquire` at all. The script never actually runs in the
/// #102.5 busy-lock test below - `acquire` fails before execution reaches it.
const SCRIPT_PLAYBOOK: &str = r#"
schema: 1
id: scripted
name: Scripted
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/work.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

fn seed_script_playbook(root: &std::path::Path) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/scripted/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), SCRIPT_PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/scripted/current"), "1.0.0").unwrap();
}

fn seed_run_in(root: &std::path::Path) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/noagent/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), NOAGENT).unwrap();
    fs::write(root.join(".apb/playbooks/noagent/current"), "1.0.0").unwrap();
    // a real run without an agent, through the engine
    let mut opts = apb_engine::RunOptions::default();
    opts.params.insert("who".into(), "world".into());
    apb_engine::run(root, "noagent", None, opts).unwrap();
}

fn seed_with_run() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    seed_run_in(dir.path());
    dir
}

const GATE: &str = r#"
schema: 1
id: gated
name: Gated
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: gate, type: human_review, options: [approved, rejected] }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: gate }
  - { from: gate, to: ok, condition: { type: review_status, equals: approved } }
  - { from: gate, to: no, condition: { type: review_status, equals: rejected } }
"#;

const GATE_PROMPT: &str = r#"
schema: 1
id: gated
name: Gated
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: gate, type: human_review, options: [approved, rejected], prompt: "Check the changelog first." }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: gate }
  - { from: gate, to: ok, condition: { type: review_status, equals: approved } }
  - { from: gate, to: no, condition: { type: review_status, equals: rejected } }
"#;

/// A run dir carrying the gate playbook's snapshot plus the given journal,
/// built by hand under an existing project. The review-validation cases need
/// exact journal shapes (a gate with an open request, a gate without one) that
/// a live drive would race, and the snapshot plus `events.jsonl` is all
/// `post_review` reads.
fn seed_gate_run(
    root: &std::path::Path,
    run_id: &str,
    payloads: &[apb_engine::event::EventPayload],
) {
    seed_gate_run_yaml(root, run_id, GATE, payloads);
}

fn seed_gate_run_yaml(
    root: &std::path::Path,
    run_id: &str,
    yaml: &str,
    payloads: &[apb_engine::event::EventPayload],
) {
    let run_dir = root.join(".apb/runs").join(run_id);
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("playbook.yaml"), yaml).unwrap();
    let mut log = apb_engine::event::EventLog::open(&run_dir).unwrap();
    log.append(apb_engine::event::EventPayload::RunStarted {
        playbook: "gated".into(),
        version: "1.0.0".into(),
    })
    .unwrap();
    for p in payloads {
        log.append(p.clone()).unwrap();
    }
}

fn review_requested(node: &str) -> apb_engine::event::EventPayload {
    apb_engine::event::EventPayload::ReviewRequested {
        node: node.into(),
        options: vec!["approved".into(), "rejected".into()],
        title: None,
        instruction: String::new(),
        prompt: None,
    }
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
    seed_gate_run(dir.path(), "gate-1", &[review_requested("gate")]);
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        "/api/runs/gate-1/review",
        serde_json::json!({ "node": "gate", "decision": "approved", "note": "ok" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["posted_seq"].is_number());
    let channel =
        fs::read_to_string(dir.path().join(".apb/runs/gate-1").join("reviews.jsonl")).unwrap();
    assert!(channel.contains("approved"));
}

/// issue #102.9: the run detail's `progress.pending_review` block carries the
/// gate node's optional `prompt:` field, on the same terms it already carries
/// `options` (the HTTP surface reads `apb_engine::progress::from_run_dir`
/// directly, so this is the same struct MCP `run_status` reports).
#[tokio::test]
async fn get_run_detail_exposes_pending_review_prompt() {
    let dir = seed_with_run();
    let prompt = apb_engine::event::EventPayload::ReviewRequested {
        node: "gate".into(),
        options: vec!["approved".into(), "rejected".into()],
        title: None,
        instruction: String::new(),
        prompt: Some("Check the changelog first.".into()),
    };
    seed_gate_run_yaml(dir.path(), "gate-2", GATE_PROMPT, &[prompt]);
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/runs/gate-2").await;
    assert_eq!(status, StatusCode::OK);
    let pr = &json["progress"]["pending_review"];
    assert_eq!(pr["node"], "gate");
    assert_eq!(pr["prompt"], "Check the changelog first.");
    assert!(
        pr["instruction"]
            .as_str()
            .unwrap()
            .contains("Check the changelog first."),
        "got: {pr}"
    );
}

/// #103.1: a decision for a node that is not a `human_review` node of this
/// run's playbook is 404, not a 200 with a `posted_seq` that reads like
/// success and a record no drive will ever consume.
#[tokio::test]
async fn post_review_on_a_node_that_is_not_a_gate_is_404() {
    let dir = seed_with_run();
    seed_gate_run(dir.path(), "gate-1", &[review_requested("gate")]);
    let app = build_router(AppState::new(dir.path().to_path_buf()));

    let (status, _) = post_json(
        app.clone(),
        "/api/runs/gate-1/review",
        serde_json::json!({ "node": "ghost", "decision": "approved" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown node must be 404");

    // A node that exists but is not a gate is equally unreachable.
    let (status, _) = post_json(
        app,
        "/api/runs/gate-1/review",
        serde_json::json!({ "node": "start", "decision": "approved" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "non-gate node must be 404");

    assert!(
        !dir.path().join(".apb/runs/gate-1/reviews.jsonl").exists(),
        "a rejected decision must not reach the channel"
    );
}

/// #103.1: the gate is real but nothing is waiting on it, so the decision is a
/// state conflict (409), on the same terms `run_playbook_handler` maps one.
#[tokio::test]
async fn post_review_on_a_gate_with_no_pending_request_is_409() {
    let dir = seed_with_run();
    seed_gate_run(dir.path(), "gate-1", &[]);
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = post_json(
        app,
        "/api/runs/gate-1/review",
        serde_json::json!({ "node": "gate", "decision": "approved" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(!dir.path().join(".apb/runs/gate-1/reviews.jsonl").exists());
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
    // Wait for the run and its hooks.json to appear. Bounded: the run is
    // driven on a detached thread whose result is discarded, so a run that
    // fails to start reports nothing at all - without a ceiling this loop
    // would poll a directory that is never going to be written, forever.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
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
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for the webhook run to write its hooks.json under {}",
            dir.path().join(".apb/runs").display()
        );
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
    assert_eq!(json[0]["progress"]["percent"], 100);
}

/// #85 finding 4, server pass-through: `GET /api/runs` carries `driver_dead`
/// for a run whose driver.pid names a provably gone process, and the field is
/// ABSENT (not `false`) on a healthy row, which is what keeps old snapshots
/// byte-identical.
#[tokio::test]
async fn lists_runs_surfaces_a_dead_driver() {
    let dir = seed_with_run();
    let run_id = apb_engine::list_runs(dir.path()).unwrap()[0].run_id.clone();

    // The healthy run seeded above has no driver.pid at all: no drive claim,
    // so the field must be entirely absent from the JSON.
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/runs").await;
    assert_eq!(status, StatusCode::OK);
    let healthy_row = json
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["run_id"] == run_id)
        .expect("seeded run listed");
    assert!(
        healthy_row.get("driver_dead").is_none(),
        "a healthy row must omit driver_dead entirely, got: {healthy_row:?}"
    );

    // A second run whose driver.pid names a process that existed and is now
    // provably gone (spawn, wait, reap, reuse the number).
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .expect("spawn a throwaway child to borrow a pid from");
    let dead_pid = child.id();
    child.wait().expect("reap the throwaway child");

    let dead_dir = dir.path().join(".apb/runs/dead-1");
    fs::create_dir_all(&dead_dir).unwrap();
    fs::write(
        dead_dir.join("events.jsonl"),
        concat!(
            r#"{"seq":0,"ts":1,"type":"run_started","playbook":"dead","version":"1.0.0"}"#,
            "\n",
            r#"{"seq":1,"ts":2,"type":"node_started","node":"start","attempt":1}"#,
            "\n",
        ),
    )
    .unwrap();
    fs::write(dead_dir.join("driver.pid"), format!("{dead_pid}\n")).unwrap();

    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/runs").await;
    assert_eq!(status, StatusCode::OK);
    let dead_row = json
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["run_id"] == "dead-1")
        .expect("dead-1 listed");
    assert_eq!(dead_row["driver_dead"], serde_json::json!(true));
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
    assert_eq!(json["progress"]["percent"], 100);
    assert_eq!(json["model"]["nodes"][0]["type"], "start");
    assert!(json["events"].as_array().unwrap().len() >= 3);
    // Detail progress now comes from the child-credit-aware
    // `progress::from_run_dir` (review R1-I5), not the plain `compute` fold.
    // A full child-credit fixture is out of scope here; assert shape only.
    assert!(json["progress"]["percent"].is_number());
    // review R1-I6: detail carries a `children` array (empty for a run with
    // no `agent_task`/`playbook` sub-runs), mirroring MCP `run_status`.
    assert_eq!(json["children"], serde_json::json!([]));
}

/// A failed run has to say why on the dashboard. The reason is the journal's
/// last `RunError`, the same value MCP `run_status` reports, and before this it
/// was reachable only through `apb doctor --run`.
#[tokio::test]
async fn run_detail_carries_the_failure_reason_of_a_failed_run() {
    let dir = seed_with_run();
    let run_id = apb_engine::list_runs(dir.path()).unwrap()[0].run_id.clone();

    // A succeeded run explains nothing, because nothing went wrong.
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (_, json) = get_json(app, &format!("/api/runs/{run_id}")).await;
    assert_eq!(json["failure_reason"], serde_json::Value::Null);

    // Append the terminal pair an engine failure writes, then read it back.
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    let mut log = apb_engine::event::EventLog::open(&run_dir).unwrap();
    log.append(apb_engine::event::EventPayload::RunError {
        node: Some("note".into()),
        reason: "3 failing tests".into(),
    })
    .unwrap();
    log.append(apb_engine::event::EventPayload::RunFinished {
        outcome: "failed".into(),
    })
    .unwrap();

    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, &format!("/api/runs/{run_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["run_status"], "failed");
    assert_eq!(json["failure_reason"], "node `note`: 3 failing tests");
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

#[tokio::test]
async fn post_playbook_run_continued_from_establishes_lineage() {
    let dir = seed_with_run();
    let first_id = apb_engine::list_runs(dir.path()).unwrap()[0].run_id.clone();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        "/api/playbooks/noagent/run",
        serde_json::json!({
            "params": { "who": "world" },
            "continued_from": first_id,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let second_id = json["run_id"].as_str().unwrap().to_string();

    let runs_dir = dir.path().join(".apb/runs");
    let pred_cfg = apb_engine::run_config::read_run_config(&runs_dir.join(&first_id)).unwrap();
    let succ_cfg = apb_engine::run_config::read_run_config(&runs_dir.join(&second_id)).unwrap();
    assert_eq!(pred_cfg.superseded_by.as_deref(), Some(second_id.as_str()));
    assert_eq!(succ_cfg.continued_from.as_deref(), Some(first_id.as_str()));
}

/// Posts a start for the script playbook and returns the response.
async fn post_script_run(state: AppState) -> axum::response::Response {
    let app = build_router(state);
    let req = Request::builder()
        .method("POST")
        .uri("/api/playbooks/scripted/run")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({})).unwrap(),
        ))
        .unwrap();
    app.oneshot(req).await.unwrap()
}

/// #102.5, revised: with queueing switched off, a second concurrent start
/// against the same project workdir must not fall into the generic 500 bucket
/// - it is a client-actionable "try again shortly", not a server fault. The
/// lock is acquired the same way `run_background` acquires it (a live pid,
/// this test process's own), deterministically, without racing a real second
/// run.
#[tokio::test]
async fn post_playbook_run_workdir_busy_is_429_with_retry_after_when_queueing_is_off() {
    let dir = tempfile::tempdir().unwrap();
    seed_script_playbook(dir.path());
    let _guard = apb_engine::workdir::acquire(dir.path(), false)
        .unwrap()
        .unwrap();
    let state = AppState::new(dir.path().to_path_buf()).with_workdir_queue_wait(None);
    let res = post_script_run(state).await;
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        res.headers()
            .get("retry-after")
            .map(|v| v.to_str().unwrap()),
        Some("5")
    );
}

/// The defect this endpoint exists to not have: an inbound-event bridge posts
/// an event, the workdir happens to be busy, and the refusal destroys the only
/// copy of the event. By default the start is now ADMITTED - a run id comes
/// back, the caller's parameters are on disk, and the run waits for the
/// workdir instead of the event waiting for nobody.
#[tokio::test]
async fn post_playbook_run_on_a_busy_workdir_is_admitted_and_queued() {
    let dir = tempfile::tempdir().unwrap();
    seed_script_playbook(dir.path());
    let _guard = apb_engine::workdir::acquire(dir.path(), false)
        .unwrap()
        .unwrap();
    let res = post_script_run(AppState::new(dir.path().to_path_buf())).await;
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let run_id = body["run_id"]
        .as_str()
        .expect("a queued start still answers with a run id");

    // The event the caller handed over survives the busy workdir: the run
    // directory and the journal exist, and the journal says why nothing has
    // started yet.
    let run_dir = dir.path().join(".apb/runs").join(run_id);
    assert!(
        run_dir.is_dir(),
        "the queued run must be persisted, not held in memory"
    );
    let events = apb_engine::event::read_all(&run_dir).unwrap();
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            apb_engine::event::EventPayload::RunQueued { reason } if reason.contains("workdir")
        )),
        "a queued start must journal why it is waiting, got {events:?}"
    );
    // Queued is not paused: an admitted run reads as running to every observer.
    assert_eq!(
        apb_engine::state::RunState::fold(&events).run_status,
        apb_engine::state::RunStatus::Running
    );
}

#[tokio::test]
async fn post_playbook_run_continued_from_rejects_unknown_predecessor() {
    let dir = seed_with_run();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let req = Request::builder()
        .method("POST")
        .uri("/api/playbooks/noagent/run")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "params": { "who": "world" },
                "continued_from": "ghost-1",
            }))
            .unwrap(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(
        body.contains("run `ghost-1`"),
        "expected clear not-found error, got: {body:?}"
    );
}

#[tokio::test]
async fn post_playbook_run_continued_from_rejects_superseded_predecessor() {
    let dir = seed_with_run();
    let first_id = apb_engine::list_runs(dir.path()).unwrap()[0].run_id.clone();
    let app = build_router(AppState::new(dir.path().to_path_buf()));

    let (status, json) = post_json(
        app.clone(),
        "/api/playbooks/noagent/run",
        serde_json::json!({
            "params": { "who": "world" },
            "continued_from": first_id,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let _second_id = json["run_id"].as_str().unwrap().to_string();

    let req = Request::builder()
        .method("POST")
        .uri("/api/playbooks/noagent/run")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "params": { "who": "world" },
                "continued_from": first_id,
            }))
            .unwrap(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "already-superseded predecessor must be 409, not 500"
    );
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(
        body.contains("already superseded"),
        "expected superseded detail, got: {body:?}"
    );
}

const OTHER: &str = r#"
schema: 1
id: other
name: Other
version: 1.0.0
params:
  - { name: who, type: text }
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hello {{params.who}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

/// Cross-playbook `continued_from` is a client precondition failure
/// (`EngineError::Invalid`), not a server fault. Must surface as 422.
#[tokio::test]
async fn post_playbook_run_continued_from_rejects_cross_playbook() {
    let dir = seed_with_run();
    let first_id = apb_engine::list_runs(dir.path()).unwrap()[0].run_id.clone();

    let vdir = dir.path().join(".apb/playbooks/other/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), OTHER).unwrap();
    fs::write(dir.path().join(".apb/playbooks/other/current"), "1.0.0").unwrap();

    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let req = Request::builder()
        .method("POST")
        .uri("/api/playbooks/other/run")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "params": { "who": "world" },
                "continued_from": first_id,
            }))
            .unwrap(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "cross-playbook continued_from must be 422, not 500"
    );
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(
        body.contains("noagent") && body.contains("other"),
        "expected both playbook ids in error, got: {body:?}"
    );
}

// --- env guards (mirrors connectors_api_test.rs) ----------------------------

struct EnvGuard {
    var: String,
    prior: Option<std::ffi::OsString>,
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.prior {
                Some(v) => std::env::set_var(&self.var, v),
                None => std::env::remove_var(&self.var),
            }
        }
    }
}
fn set_var(var: &str, value: impl AsRef<std::ffi::OsStr>) -> EnvGuard {
    let prior = std::env::var_os(var);
    unsafe {
        std::env::set_var(var, value);
    }
    EnvGuard {
        var: var.to_string(),
        prior,
    }
}
fn unset_var(var: &str) -> EnvGuard {
    let prior = std::env::var_os(var);
    unsafe {
        std::env::remove_var(var);
    }
    EnvGuard {
        var: var.to_string(),
        prior,
    }
}

fn register_workspace(root: &std::path::Path) -> String {
    apb_core::projects::touch(root);
    fs::read_to_string(root.join(".apb/workspace.local"))
        .expect("workspace.local written by registration")
        .trim()
        .to_string()
}

/// #103.2: `GET /api/runs` honors `?workspace=`. The listing used to ignore
/// it and always answer the machine-wide aggregate, so a caller that had just
/// been handed a row's `workspace_id` could not ask for that project's runs
/// alone - while `GET /api/runs/{id}` has always required exactly that param.
#[tokio::test]
async fn lists_runs_filters_by_workspace() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = set_var("APB_CONFIG_DIR", cfg.path());
    let _ci = unset_var("CI");
    let _noreg = unset_var("APB_NO_REGISTRY");

    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    seed_run_in(a.path());
    seed_run_in(b.path());
    let id_a = register_workspace(a.path());
    let id_b = register_workspace(b.path());
    assert_ne!(id_a, id_b);

    // No param: the aggregate, unchanged (this is what the dashboard calls).
    let app = build_router(AppState::new_global());
    let (status, json) = get_json(app, "/api/runs").await;
    assert_eq!(status, StatusCode::OK);
    let aggregate = json.as_array().unwrap().clone();
    assert_eq!(aggregate.len(), 2, "aggregate: {json}");
    let project_a = aggregate
        .iter()
        .find(|r| r["workspace_id"] == serde_json::json!(id_a))
        .expect("project a listed in the aggregate")["project"]
        .clone();
    assert!(project_a.is_string() && project_a != serde_json::json!(""));

    // Scoped: only that project's runs, stamped identically to the aggregate.
    // `project` in particular: it is looked up by workspace id, not by path,
    // because `resolve_root` canonicalizes while the registry keeps the path
    // as registered (every macOS temp root is a `/var` symlink).
    let app = build_router(AppState::new_global());
    let (status, json) = get_json(app, &format!("/api/runs?workspace={id_a}")).await;
    assert_eq!(status, StatusCode::OK);
    let rows = json.as_array().unwrap();
    assert_eq!(rows.len(), 1, "scoped listing: {json}");
    assert_eq!(rows[0]["workspace_id"], serde_json::json!(id_a));
    assert_eq!(rows[0]["project"], project_a, "scoped listing: {json}");
    assert_eq!(rows[0]["playbook"], "noagent");

    // An unknown workspace stays strict, exactly like the detail endpoint.
    let app = build_router(AppState::new_global());
    let (status, _) = get_json(app, "/api/runs?workspace=no-such-workspace").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A run dir with one open attempt and the given attempt/driver pids. The pure
/// fold calls that shape `interrupted`; what it really is depends on whether
/// those processes are alive, which is precisely what the detail endpoint had
/// no way to say.
fn seed_open_attempt_run(root: &std::path::Path, run_id: &str, pid: u32) {
    let run_dir = root.join(".apb/runs").join(run_id);
    fs::create_dir_all(&run_dir).unwrap();
    let mut log = apb_engine::event::EventLog::open(&run_dir).unwrap();
    log.append(apb_engine::event::EventPayload::RunStarted {
        playbook: "noagent".into(),
        version: "1.0.0".into(),
    })
    .unwrap();
    log.append(apb_engine::event::EventPayload::NodeStarted {
        node: "note".into(),
        attempt: 1,
    })
    .unwrap();
    log.append(apb_engine::event::EventPayload::AttemptStarted {
        node: "note".into(),
        attempt: 1,
        agent: "claude".into(),
        soul_delivery: None,
        skills_mode: None,
        pid: Some(pid),
        spawn_ms: None,
    })
    .unwrap();
    fs::write(run_dir.join("driver.pid"), format!("{pid}\n")).unwrap();
}

/// A pid that existed and is now provably gone (spawn, wait, reap).
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .expect("spawn a throwaway child to borrow a pid from");
    let pid = child.id();
    child.wait().expect("reap the throwaway child");
    pid
}

/// #85.4 / #102.4 cause A: the run detail reports through the liveness overlay
/// the listing has always used, and says whether a driver is alive. Before
/// this, a healthy in-flight run read `interrupted` on the detail view while
/// the run list next to it read `running`.
#[tokio::test]
async fn run_detail_reports_a_live_attempt_as_running_with_driver_alive() {
    let dir = seed_with_run();
    // Our own pid: alive by definition, and never a reused number.
    seed_open_attempt_run(dir.path(), "live-1", std::process::id());

    // The pure fold, which is what the endpoint used to report.
    let events = apb_engine::event::read_all(&dir.path().join(".apb/runs/live-1")).unwrap();
    assert_eq!(
        apb_engine::state::RunState::fold(&events).run_status,
        apb_engine::state::RunStatus::Interrupted,
        "fixture must be the pure-fold interrupted shape"
    );

    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/runs/live-1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["run_status"], "running", "detail: {json}");
    assert_eq!(json["nodes"]["note"], "running");
    assert_eq!(json["driver_alive"], serde_json::json!(true));
}

/// The same overlay in the other direction: a driverless run whose attempt pid
/// is provably gone reports the node as `lost` and the driver as not alive, so
/// the detail view can offer a resume instead of showing work in flight.
#[tokio::test]
async fn run_detail_reports_a_dead_attempt_as_lost() {
    let dir = seed_with_run();
    seed_open_attempt_run(dir.path(), "dead-2", dead_pid());
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/runs/dead-2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["nodes"]["note"], "lost", "detail: {json}");
    assert_eq!(json["driver_alive"], serde_json::json!(false));
}

/// A finished run keeps reporting exactly what it always did, and carries an
/// explicit `driver_alive: null` (no drive claim at all is not a dead one).
#[tokio::test]
async fn run_detail_of_a_finished_run_is_unchanged() {
    let dir = seed_with_run();
    let run_id = apb_engine::list_runs(dir.path()).unwrap()[0].run_id.clone();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, &format!("/api/runs/{run_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["run_status"], "succeeded");
    assert_eq!(json["nodes"]["note"], "succeeded");
    assert_eq!(json["driver_alive"], serde_json::Value::Null);
}

/// #103.3 (a): every C0 control byte a `from_utf8_lossy` agent capture can
/// deposit into an output or an event payload must survive the response as
/// valid JSON. serde_json escapes them on the way out, and this pins that
/// invariant against any future hand-rolled embedding of a raw fragment.
#[tokio::test]
async fn run_detail_body_round_trips_control_characters() {
    let dir = seed_with_run();
    let run_id = apb_engine::list_runs(dir.path()).unwrap()[0].run_id.clone();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    let control: String = (0u8..=0x1f).map(|b| b as char).collect();
    let payload = format!("before{control}after");

    let mut log = apb_engine::event::EventLog::open(&run_dir).unwrap();
    log.append(apb_engine::event::EventPayload::NodeFinished {
        node: "note".into(),
        status: "succeeded".into(),
        attempt: 1,
        output: payload.clone(),
        artifacts: Vec::new(),
    })
    .unwrap();
    log.append(apb_engine::event::EventPayload::RunError {
        node: Some("note".into()),
        reason: payload.clone(),
    })
    .unwrap();

    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let res = app
        .oneshot(
            Request::get(format!("/api/runs/{run_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    // No raw C0 byte may appear anywhere in the body.
    assert!(
        !bytes.iter().any(|b| *b < 0x20 && *b != b'\n'),
        "response body carries a raw control byte"
    );
    let json: serde_json::Value =
        serde_json::from_slice(&bytes).expect("detail body must be valid JSON");
    assert_eq!(json["outputs"]["note"], serde_json::json!(payload));
}

/// #103.3 (b): a detail request that lands between the bytes of a line the
/// drive is appending must not answer 500 for the whole run. The tail is
/// dropped, everything already written is reported.
#[tokio::test]
async fn run_detail_tolerates_a_torn_trailing_event_line() {
    let dir = seed_with_run();
    let run_id = apb_engine::list_runs(dir.path()).unwrap()[0].run_id.clone();
    let events_path = dir
        .path()
        .join(".apb/runs")
        .join(&run_id)
        .join("events.jsonl");
    let complete = fs::read_to_string(&events_path).unwrap();
    fs::write(
        &events_path,
        format!("{complete}{{\"seq\":99,\"ts\":9,\"type\":\"node_star"),
    )
    .unwrap();

    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, &format!("/api/runs/{run_id}")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a torn tail must not fail the whole read: {json}"
    );
    assert_eq!(json["run_status"], "succeeded");
    assert_eq!(
        json["events"].as_array().unwrap().len(),
        complete.lines().count(),
        "every complete line is still reported"
    );
}
