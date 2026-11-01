//! A run's secret values never leave apb on a run surface that is not the
//! executor (design: secrets of a run). One run declares `message` secret;
//! its script nodes echo the value into a node output, into stderr and into
//! the error that fails the run, and a sub-playbook node hands it to a child
//! run as the child's instruction, which the child echoes again. Every MCP
//! run tool, every HTTP run route, the HTML report and the notice body are
//! then checked for the value, for the parent and the child. The raw journal
//! keeps it: redaction happens on read.

use std::fs;
use std::path::Path;
use std::time::Duration;

use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::secret_runs::{SECRET, seed_secret_runs};

fn holds(text: &str) -> bool {
    text.contains(SECRET)
}

async fn get_text(app: axum::Router, uri: &str) -> (StatusCode, String) {
    let res = app
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[test]
fn the_fixture_really_echoes_the_value_into_the_raw_journal() {
    let dir = tempfile::tempdir().unwrap();
    let (parent, child) = seed_secret_runs(dir.path());
    let runs = dir.path().join(".apb/runs");
    // Redact on read: what the executor produced stays as it was, so the
    // checks below prove a surface hid it, not that it was never there.
    let journal = fs::read_to_string(runs.join(&parent).join("events.jsonl")).unwrap();
    assert!(holds(&journal), "the parent journal must keep the echo");
    let journal = fs::read_to_string(runs.join(&child).join("events.jsonl")).unwrap();
    assert!(holds(&journal), "the child journal must keep the echo");
    let view = apb_engine::run_view::RunView::load(&runs.join(&parent), &parent).unwrap();
    assert_eq!(view.run_status, apb_engine::state::RunStatus::Failed);
    assert!(
        view.state.outputs.get("boom").is_some_and(|o| holds(o)),
        "the failed node must carry the value in its raw output"
    );
}

#[test]
fn no_mcp_run_tool_answers_with_the_value() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (parent, child) = seed_secret_runs(root);
    use apb_mcp::tools;
    let list = tools::runs_list(root).unwrap().to_string();
    assert!(!holds(&list), "runs_list leaked the value");
    for run in [&parent, &child] {
        let answers = [
            ("run_status", tools::run_status(root, run).unwrap()),
            ("run_events", tools::run_events(root, run, None).unwrap()),
            ("run_report", tools::run_report(root, run).unwrap()),
            (
                "run_retro_context",
                tools::run_retro_context(root, run, None).unwrap(),
            ),
            (
                "supervisor_run_inspect",
                tools::sv_run_inspect_with(root, run, false).unwrap(),
            ),
            (
                "supervisor_run_inspect full",
                tools::sv_run_inspect_with(root, run, true).unwrap(),
            ),
        ];
        for (tool, answer) in answers {
            assert!(!holds(&answer.to_string()), "{tool} leaked the value");
        }
        let res = apb_engine::run_wait::wait_run(root, run, Duration::from_secs(5)).unwrap();
        let waited = tools::run_wait_result(root, run, &res).unwrap();
        assert!(!holds(&waited.to_string()), "run_wait leaked the value");
        // A wake whose detail quotes the value, longer than the clip, so a
        // clip before the redaction would leave part of it behind.
        let wake = apb_engine::run_wait::SupervisorWait::Wake(apb_engine::inspect::WakeEvent {
            seq: 1,
            trigger: apb_engine::event::WakeTrigger::NodeFailed,
            node: "boom".into(),
            detail: format!("{}{SECRET}", "x".repeat(8000)),
            triage: None,
        });
        let woke = tools::supervisor_wait_result(root, run, None, &wake).unwrap();
        assert!(
            !holds(&woke.to_string()),
            "supervisor_wait_event leaked the value"
        );
    }
}

#[tokio::test]
async fn no_http_run_route_answers_with_the_value() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let (parent, child) = tokio::task::spawn_blocking({
        let root = root.clone();
        move || seed_secret_runs(&root)
    })
    .await
    .unwrap();
    let app = build_router(AppState::new(root.clone()));
    let (status, body) = get_text(app.clone(), "/api/runs").await;
    assert_eq!(status, StatusCode::OK);
    assert!(!holds(&body), "GET /api/runs leaked the value");
    for run in [&parent, &child] {
        for route in [
            format!("/api/runs/{run}"),
            format!("/api/runs/{run}/report"),
            format!("/api/runs/{run}/report.html"),
        ] {
            let (status, body) = get_text(app.clone(), &route).await;
            assert_eq!(status, StatusCode::OK, "{route}");
            assert!(!holds(&body), "{route} leaked the value");
        }
    }
    let (_, body) = get_text(app, &format!("/api/runs/{parent}")).await;
    assert!(
        body.contains("[redacted]"),
        "the echoed output must show as redacted, not vanish"
    );
    let detail: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(detail["params"]["message"], apb_core::run_params::REDACTED);
    assert_eq!(detail["redacted_params"], serde_json::json!(["message"]));
}

#[test]
fn the_html_report_and_the_notice_body_carry_no_value() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (parent, child) = seed_secret_runs(root);
    for run in [&parent, &child] {
        let run_dir = root.join(".apb/runs").join(run);
        let html = apb_engine::report_html::render(root, &run_dir).unwrap();
        assert!(!holds(&html), "the HTML report leaked the value");
        let events = apb_engine::run_view::read_events(&run_dir).unwrap();
        for e in &events {
            let kind = match &e.payload {
                apb_engine::event::EventPayload::RunFinished { .. } => {
                    apb_core::notify::Transition::Finished
                }
                _ => continue,
            };
            let t = apb_engine::notify::transitions::TransitionId { kind, seq: e.seq };
            let notice =
                apb_engine::notify::notice::Notice::build(root, &run_dir, run, &events, &t);
            assert!(
                !holds(&notice.subject) && !holds(&notice.text),
                "the notice leaked the value"
            );
        }
    }
}

/// A finished run written by hand: `run_started` at `started` ms and
/// `run_finished` `ran` ms later, so it has a duration.
fn seed_finished(root: &Path, run_id: &str, started: u128) {
    let run_dir = root.join(".apb/runs").join(run_id);
    fs::create_dir_all(&run_dir).unwrap();
    let lines = [
        serde_json::json!({"seq": 0, "ts": started, "type": "run_started", "playbook": "secretpb", "version": "1.0.0"}),
        serde_json::json!({"seq": 1, "ts": started + 1500, "type": "run_finished", "outcome": "succeeded"}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(run_dir.join("events.jsonl"), text).unwrap();
}

#[tokio::test]
async fn redaction_keeps_the_run_list_newest_first_with_its_durations() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    apb_core::registry::init_project(&root).unwrap();
    seed_finished(&root, "secretpb-older", 1_000_000);
    seed_finished(&root, "secretpb-newer", 2_000_000);
    let engine = apb_engine::list_runs(&root).unwrap();
    assert!(
        engine.iter().all(|r| r.duration_ms.is_some()),
        "the fixture runs must have durations"
    );
    let by_id = |id: &str| engine.iter().find(|r| r.run_id == id).unwrap();

    let mcp = apb_mcp::tools::runs_list(&root).unwrap();
    for entry in mcp.as_array().unwrap() {
        let id = entry["run_id"].as_str().unwrap();
        let want = by_id(id);
        assert_eq!(entry["duration_ms"], serde_json::json!(want.duration_ms));
        assert_eq!(
            entry.get("paused_ms").cloned().unwrap_or_default(),
            serde_json::json!(want.paused_ms)
        );
    }

    let app = build_router(AppState::new(root.clone()));
    let (status, body) = get_text(app, "/api/runs").await;
    assert_eq!(status, StatusCode::OK);
    let list: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    let ids: Vec<&str> = list.iter().map(|e| e["run_id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["secretpb-newer", "secretpb-older"]);
    for entry in &list {
        let want = by_id(entry["run_id"].as_str().unwrap());
        assert_eq!(entry["duration_ms"], serde_json::json!(want.duration_ms));
        assert_eq!(
            entry.get("paused_ms").cloned().unwrap_or_default(),
            serde_json::json!(want.paused_ms)
        );
    }
}
