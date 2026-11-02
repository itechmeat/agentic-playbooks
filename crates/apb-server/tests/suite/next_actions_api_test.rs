//! Next actions on the HTTP API: the run detail carries them, an answer
//! names its request and a stale or second one is a conflict, and a person
//! resolves an unknown send through its own route.

use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::fs;
use std::path::Path;
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

/// A run parked on `ask`'s question, journaled, so its action names the
/// request seq.
fn seed_question(root: &Path) -> (String, u64) {
    let run_id = "run-q-1".to_string();
    let run_dir = root.join(".apb/runs").join(&run_id);
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("playbook.yaml"), INTERACTIVE_WF).unwrap();
    let mut log = apb_engine::event::EventLog::create(&run_dir).unwrap();
    log.append(apb_engine::event::EventPayload::RunStarted {
        playbook: "iq".into(),
        version: "1.0.0".into(),
        definition: None,
    })
    .unwrap();
    apb_engine::question::post_question(&run_dir, "ask", 1, "which way", vec!["left".into()])
        .unwrap();
    let seq = log
        .append(apb_engine::event::EventPayload::QuestionAsked {
            node: "ask".into(),
            question: "which way".into(),
            options: vec!["left".into()],
        })
        .unwrap()
        .seq;
    (run_id, seq)
}

/// A finished run whose one send nobody can tell went out.
fn seed_unknown_send(root: &Path) -> String {
    let run_id = "run-send-1".to_string();
    let dir = root.join(".apb/runs").join(&run_id);
    fs::create_dir_all(&dir).unwrap();
    let lines = [
        r#"{"seq":0,"ts":1,"type":"run_started","playbook":"p","version":"1.0.0"}"#,
        r#"{"seq":1,"ts":2,"type":"send_intent_recorded","intent":"si-a","nonce":"n","node":"post","connector":"telegram","function":"send_message","account":"main","mode":"none","label":"hello","pid":0,"deadline_ms":0}"#,
        r#"{"seq":2,"ts":3,"type":"send_intent_settled","intent":"si-a","outcome":"unknown","source":"response","detail":"timeout"}"#,
        r#"{"seq":3,"ts":4,"type":"run_finished","outcome":"succeeded"}"#,
    ];
    fs::write(dir.join("events.jsonl"), lines.join("\n") + "\n").unwrap();
    run_id
}

async fn call(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    call_as(app, method, uri, None, body).await
}

/// [`call`] with the bearer `key` when given.
async fn call_as(
    app: axum::Router,
    method: &str,
    uri: &str,
    key: Option<&str>,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(k) = key {
        req = req.header("authorization", format!("Bearer {k}"));
    }
    let req = req
        .body(match body {
            Some(b) => Body::from(serde_json::to_vec(&b).unwrap()),
            None => Body::empty(),
        })
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

#[tokio::test]
async fn the_run_detail_carries_the_question_action_with_its_request() {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let (run_id, seq) = seed_question(dir.path());
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, detail) = call(app, "GET", &format!("/api/runs/{run_id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    let actions = &detail["next_actions"];
    assert_eq!(actions["stop"], "needs_input", "{actions}");
    let primary = &actions["actions"][actions["primary"].as_u64().unwrap() as usize];
    assert_eq!(primary["code"], "answer_question");
    assert_eq!(primary["run_id"], run_id.as_str());
    assert_eq!(primary["target"]["request"]["seq"], seq);
}

/// An answer that names its request goes in once; a second one for the
/// same request is a conflict (409), not a server fault, and so is a
/// request the node never asked.
#[tokio::test]
async fn an_answer_to_an_answered_request_is_a_conflict() {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let (run_id, seq) = seed_question(dir.path());
    let uri = format!("/api/runs/{run_id}/answer");
    let body = serde_json::json!({ "node": "ask", "answer": "left", "request": seq });
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (first, _) = call(app.clone(), "POST", &uri, Some(body.clone())).await;
    assert_eq!(first, StatusCode::OK);
    let (second, text) = call(app.clone(), "POST", &uri, Some(body)).await;
    assert_eq!(second, StatusCode::CONFLICT, "{text}");
    assert!(text.as_str().unwrap().contains("already"), "{text}");
    let unknown = serde_json::json!({ "node": "ask", "answer": "left", "request": seq + 50 });
    let (status, _) = call(app, "POST", &uri, Some(unknown)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A review decision that names a request the node never opened is not
/// found; a decision for the open request goes in, a second one is a
/// conflict.
#[tokio::test]
async fn a_review_decision_names_its_request() {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let run_id = "run-r-1";
    let run_dir = dir.path().join(".apb/runs").join(run_id);
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(
        run_dir.join("playbook.yaml"),
        r#"
schema: 2
id: rv
name: RV
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: gate, type: human_review }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: gate }
  - { from: gate, to: done }
"#,
    )
    .unwrap();
    let mut log = apb_engine::event::EventLog::create(&run_dir).unwrap();
    log.append(apb_engine::event::EventPayload::RunStarted {
        playbook: "rv".into(),
        version: "1.0.0".into(),
        definition: None,
    })
    .unwrap();
    let seq = log
        .append(apb_engine::event::EventPayload::ReviewRequested {
            node: "gate".into(),
            options: vec!["approved".into(), "rejected".into()],
            title: None,
            instruction: String::new(),
            prompt: None,
            recommendation: None,
            plan_digest: None,
        })
        .unwrap()
        .seq;
    let uri = format!("/api/runs/{run_id}/review");
    let decide = |request: u64| serde_json::json!({ "node": "gate", "decision": "approved", "note": "", "request": request });
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (missing, _) = call(app.clone(), "POST", &uri, Some(decide(seq + 9))).await;
    assert_eq!(missing, StatusCode::NOT_FOUND);
    let (ok, _) = call(app.clone(), "POST", &uri, Some(decide(seq))).await;
    assert_eq!(ok, StatusCode::OK);
    let (again, _) = call(app, "POST", &uri, Some(decide(seq))).await;
    assert_eq!(again, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_person_resolves_an_unknown_send_once_from_the_dashboard() {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let run_id = seed_unknown_send(dir.path());
    let (key, state) = crate::common::keyed(dir.path().to_path_buf());
    let app = build_router(state);
    let key = Some(key.as_str());
    let call = |app, method, uri: &str, body| {
        let uri = uri.to_string();
        async move { call_as(app, method, &uri, key, body).await }
    };

    let (_, detail) = call(app.clone(), "GET", &format!("/api/runs/{run_id}"), None).await;
    let codes: Vec<&str> = detail["next_actions"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["resolve_intent", "read_report"], "{detail}");

    let resolve = |intent: &str| format!("/api/runs/{run_id}/intents/{intent}/resolve");
    let body = serde_json::json!({ "sent": false, "note": "checked the channel" });
    let (unknown, _) = call(app.clone(), "POST", &resolve("si-zzz"), Some(body.clone())).await;
    assert_eq!(unknown, StatusCode::NOT_FOUND);
    let (no_run, _) = call(
        app.clone(),
        "POST",
        "/api/runs/run-ghost/intents/si-a/resolve",
        Some(body.clone()),
    )
    .await;
    assert_eq!(no_run, StatusCode::NOT_FOUND);
    let (ok, answer) = call(app.clone(), "POST", &resolve("si-a"), Some(body.clone())).await;
    assert_eq!(ok, StatusCode::OK, "{answer}");
    assert_eq!(answer["settled"], "not_sent");
    let (again, _) = call(app.clone(), "POST", &resolve("si-a"), Some(body)).await;
    assert_eq!(again, StatusCode::CONFLICT);

    // Resolved: the action is gone, the report stays.
    let (_, detail) = call(app, "GET", &format!("/api/runs/{run_id}"), None).await;
    assert_eq!(detail["next_actions"]["actions"][0]["code"], "read_report");
    assert_eq!(
        detail["next_actions"]["actions"].as_array().unwrap().len(),
        1
    );
}

/// A dashboard without keys records no decision on a send: anything on the
/// machine reaches it. It answers 403 with the CLI line, whatever the ids
/// look like (a traversal-shaped one included), and the journal is
/// untouched.
#[tokio::test]
async fn a_keyless_dashboard_refuses_a_send_decision_and_points_at_the_cli() {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let run_id = seed_unknown_send(dir.path());
    let journal = dir
        .path()
        .join(".apb/runs")
        .join(&run_id)
        .join("events.jsonl");
    let before = fs::read(&journal).unwrap();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let body = serde_json::json!({ "sent": true });
    let (status, answer) = call(
        app.clone(),
        "POST",
        &format!("/api/runs/{run_id}/intents/si-a/resolve"),
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
    let text = answer.as_str().unwrap_or_default();
    assert!(
        text.contains(&format!(
            "apb intent resolve '--sent|--not-sent' {run_id} si-a"
        )),
        "{text}"
    );
    for uri in [
        format!("/api/runs/{run_id}/intents/..%2F..%2Fx/resolve"),
        "/api/runs/..%2F..%2Fetc/intents/si-a/resolve".to_string(),
        format!("/api/runs/{run_id}/intents/%2E%2E/resolve"),
    ] {
        let (status, answer) = call(app.clone(), "POST", &uri, Some(body.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}: {answer}");
        let text = answer.as_str().unwrap_or_default();
        assert!(text.contains("apb intent resolve"), "{text}");
    }
    assert_eq!(fs::read(&journal).unwrap(), before, "nothing was recorded");

    // With keys, a traversal-shaped id finds no run or intent.
    let (key, state) = crate::common::keyed(dir.path().to_path_buf());
    let app = build_router(state);
    for uri in [
        format!("/api/runs/{run_id}/intents/..%2F..%2Fx/resolve"),
        "/api/runs/..%2F..%2Fetc/intents/si-a/resolve".to_string(),
    ] {
        let (status, answer) =
            call_as(app.clone(), "POST", &uri, Some(&key), Some(body.clone())).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {answer}");
    }
    assert_eq!(fs::read(&journal).unwrap(), before, "nothing was recorded");
}

/// A run whose `work` retry waits an hour, driven by this process (a live
/// driver).
fn seed_deferred(root: &Path) -> String {
    let run_id = "run-defer-1".to_string();
    let dir = root.join(".apb/runs").join(&run_id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), INTERACTIVE_WF).unwrap();
    let now = apb_core::clock::now_ms_u64();
    let lines = [
        serde_json::json!({"seq":0,"ts":now,"type":"run_started","playbook":"iq","version":"1.0.0"}),
        serde_json::json!({"seq":1,"ts":now,"type":"node_started","node":"ask","attempt":1}),
        serde_json::json!({"seq":2,"ts":now,"type":"retry_deferred","node":"ask","after_attempt":1,"retry":1,"step":0,"binding":"b","delay_ms":3_600_000}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(dir.join("events.jsonl"), text).unwrap();
    fs::write(
        apb_engine::driver::driver_pid_path(&dir),
        std::process::id().to_string(),
    )
    .unwrap();
    run_id
}

/// `retry_now` on a run a live driver holds: the run detail shows the
/// waiting retry and its action, the resume route journals the expedite
/// and starts no driver. Refusals come in MCP `run_resume`'s order (see
/// `server::tests::run_resume_retry_now_expedites_a_live_run` in apb-mcp)
/// with its codes: the arguments (400), then the run gate, then a node with
/// nothing waiting (409).
#[tokio::test]
async fn retry_now_expedites_a_live_run_and_refuses_a_node_with_nothing_waiting() {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let run_id = seed_deferred(dir.path());
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, detail) = call(app.clone(), "GET", &format!("/api/runs/{run_id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["nodes"]["ask"], "retry_waiting", "{detail}");
    assert_eq!(detail["retry_waits"][0]["node"], "ask");
    assert_eq!(detail["retry_waits"][0]["due"]["kind"], "at");
    let actions = &detail["next_actions"];
    assert_eq!(actions["stop"], "progresses_by_itself");
    assert_eq!(actions["actions"][0]["code"], "retry_now");
    assert_eq!(
        actions["actions"][0]["command"],
        format!("apb resume --retry-now --node=ask {run_id}")
    );

    let uri = format!("/api/runs/{run_id}/resume");
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    // On a run directory this machine did not create: the arguments are
    // refused first, then the run gate refuses before anything about the
    // run's retries is told, and nothing is journaled.
    let cases = [
        (
            serde_json::json!({ "retry_now": true, "node": "ask", "from_node": "ask" }),
            StatusCode::BAD_REQUEST,
            "retry_now_with_from_node",
        ),
        (
            serde_json::json!({ "node": "ask" }),
            StatusCode::BAD_REQUEST,
            "node_without_retry_now",
        ),
        (
            serde_json::json!({ "retry_now": true, "node": "nope" }),
            StatusCode::CONFLICT,
            "run_not_created_locally",
        ),
        (
            serde_json::json!({ "retry_now": true, "node": "ask" }),
            StatusCode::CONFLICT,
            "run_not_created_locally",
        ),
    ];
    for (body, want_status, code) in cases {
        let (status, out) = call(app.clone(), "POST", &uri, Some(body.clone())).await;
        assert_eq!(status, want_status, "{body}: {out}");
        let got = out["error"].as_str().or(out["policy"].as_str());
        assert_eq!(got, Some(code), "{body}: {out}");
    }
    let events = apb_engine::event::read_all(&run_dir).unwrap();
    assert_eq!(events.len(), 3, "nothing journaled");
    apb_core::run_origin::ensure_key().unwrap();
    apb_core::run_origin::stamp(&run_dir, &run_id).unwrap();

    let (status, out) = call(
        app.clone(),
        "POST",
        &uri,
        Some(serde_json::json!({ "retry_now": true, "node": "nope" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{out}");
    assert_eq!(out["error"], "no_deferred_retry");

    let (status, out) = call(
        app.clone(),
        "POST",
        &uri,
        Some(serde_json::json!({ "retry_now": true, "node": "ask", "allow_environment_drift": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["retry_now"], serde_json::json!(["ask"]));
    assert_eq!(out["live"], true);
    // The expedite is the whole action: a resume argument it cannot apply
    // is named, never dropped in silence.
    assert_eq!(
        out["not_applied"],
        serde_json::json!(["allow_environment_drift"])
    );
    let events = apb_engine::event::read_all(&dir.path().join(".apb/runs").join(&run_id)).unwrap();
    let last = serde_json::to_value(&events.last().unwrap().payload).unwrap();
    // A dashboard without keys takes any request that reaches its loopback
    // port, so it never records the expedite as a person's.
    assert_eq!(
        last,
        serde_json::json!({"type": "retry_expedited", "node": "ask", "by": "host"})
    );
}

/// A dashboard with keys: the request was signed in, so the retry-now is
/// journaled as the person's (`operator`).
#[tokio::test]
async fn a_signed_in_retry_now_is_journaled_as_the_operators() {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let run_id = seed_deferred(dir.path());
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    apb_core::run_origin::ensure_key().unwrap();
    apb_core::run_origin::stamp(&run_dir, &run_id).unwrap();
    let (key, state) = crate::common::keyed(dir.path().to_path_buf());
    let app = build_router(state);
    let (status, out) = call_as(
        app,
        "POST",
        &format!("/api/runs/{run_id}/resume"),
        Some(&key),
        Some(serde_json::json!({ "retry_now": true, "node": "ask" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["live"], true, "{out}");
    let events = apb_engine::event::read_all(&run_dir).unwrap();
    let last = serde_json::to_value(&events.last().unwrap().payload).unwrap();
    assert_eq!(
        last,
        serde_json::json!({"type": "retry_expedited", "node": "ask", "by": "operator"})
    );
}
