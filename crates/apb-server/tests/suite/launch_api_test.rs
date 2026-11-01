//! The launch policy on the HTTP API. A dashboard without keys is reachable
//! by anything on the machine, so its starts and resumes are decided like
//! the CLI without a terminal (`ask` only with the refusal's nonce, `deny`
//! refused) and its editor never lowers a launch floor. With keys the click
//! is a person's launch, and the editor lowers a floor only to the lowering
//! the person confirmed. The run detail shows the authorization a run keeps.

use std::fs;
use std::path::Path;

use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::common::config_sandbox;

fn yaml(id: &str, decl: &str) -> String {
    format!(
        "schema: 2\nid: {id}\nname: Demo\nversion: 1.0.0\n{decl}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n"
    )
}

/// A project with playbook `id` declaring `decl` (an `agent_launch:` line or
/// nothing), written straight into its store as a checkout would.
fn project(id: &str, decl: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    apb_core::registry::init_project(root).unwrap();
    apb_core::run_origin::ensure_key().unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml(id, decl)).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    dir
}

/// One request, as `key` when given; the answer parsed as JSON, or as a
/// JSON string when it is plain text.
async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    key: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(k) = key {
        req = req.header("authorization", format!("Bearer {k}"));
    }
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(serde_json::to_vec(&b).unwrap())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

fn launch_of(root: &Path, run_id: &str) -> apb_core::launch::LaunchRecord {
    apb_engine::manifest::read(&root.join(".apb/runs").join(run_id))
        .unwrap()
        .expect("a manifest")
        .launch
        .expect("a launch record")
}

fn runs(root: &Path) -> usize {
    fs::read_dir(root.join(".apb/runs"))
        .map(|d| d.count())
        .unwrap_or(0)
}

#[tokio::test]
async fn a_keyless_dashboard_starts_ask_only_with_its_nonce_and_never_deny() {
    let _cfg = config_sandbox().await;
    let dir = project("demo", "agent_launch: ask\n");
    let root = dir.path();
    let app = build_router(AppState::new(root.to_path_buf()));

    let (status, refusal) = call(
        &app,
        "POST",
        "/api/playbooks/demo/run",
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refusal}");
    assert_eq!(
        refusal["policy"], "launch_requires_confirmation",
        "{refusal}"
    );
    let nonce = refusal["launch_nonce"]
        .as_str()
        .expect("a nonce")
        .to_string();
    assert!(refusal["start_actions"].is_array(), "{refusal}");
    assert_eq!(runs(root), 0, "a refusal writes no run");

    // A wrong nonce is refused again with the same fresh nonce.
    let (status, again) = call(
        &app,
        "POST",
        "/api/playbooks/demo/run",
        None,
        Some(json!({
            "confirm_launch": "launch-wrong",
            "launch_version": refusal["launch_version"],
            "launch_choice": refusal["launch_choice"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{again}");
    assert_eq!(again["launch_nonce"], nonce.as_str());

    let (status, started) = call(
        &app,
        "POST",
        "/api/playbooks/demo/run",
        None,
        Some(json!({
            "confirm_launch": nonce,
            "launch_version": refusal["launch_version"],
            "launch_choice": refusal["launch_choice"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let run_id = started["run_id"].as_str().unwrap().to_string();
    let _reaper = crate::driver_reaper::DriverReaper::new(root, &run_id);
    let record = launch_of(root, &run_id);
    assert_eq!(record.initiator, "dashboard_keyless");
    assert_eq!(record.confirmation.as_deref(), Some("dashboard_nonce"));
    assert_eq!(record.kind, "start");

    // The run detail reports the authorization, never a nonce.
    let (status, detail) = call(&app, "GET", &format!("/api/runs/{run_id}"), None, None).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(
        detail["launch"]["initiator"], "dashboard_keyless",
        "{detail}"
    );
    assert_eq!(detail["launch"]["policy"], "ask", "{detail}");
    assert!(
        !detail.to_string().contains(&nonce),
        "no nonce in the detail"
    );

    // `deny` is refused without a nonce, whatever the request carries.
    let deny = project("locked", "agent_launch: deny\n");
    let app = build_router(AppState::new(deny.path().to_path_buf()));
    let (status, refusal) = call(
        &app,
        "POST",
        "/api/playbooks/locked/run",
        None,
        Some(json!({ "confirm_launch": nonce })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refusal}");
    assert_eq!(refusal["policy"], "launch_requires_person", "{refusal}");
    assert_eq!(refusal["launch_by_person"], true);
    assert!(refusal.get("launch_nonce").is_none(), "{refusal}");
    assert_eq!(runs(deny.path()), 0);
}

#[tokio::test]
async fn a_dashboard_with_keys_starts_deny_as_a_person() {
    let _cfg = config_sandbox().await;
    let dir = project("locked", "agent_launch: deny\n");
    let root = dir.path();
    let (key, state) = crate::common::keyed(root.to_path_buf());
    let app = build_router(state);
    let (status, started) = call(
        &app,
        "POST",
        "/api/playbooks/locked/run",
        Some(&key),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let run_id = started["run_id"].as_str().unwrap().to_string();
    let _reaper = crate::driver_reaper::DriverReaper::new(root, &run_id);
    let record = launch_of(root, &run_id);
    assert_eq!(record.initiator, "dashboard");
    assert_eq!(record.confirmation.as_deref(), Some("dashboard"));
    assert_eq!(record.policy, apb_core::launch::LaunchPolicy::Deny);
}

/// A start with `continued_from` is recorded as such, not as a plain start.
#[tokio::test]
async fn a_continued_start_is_recorded_as_continue_from() {
    let _cfg = config_sandbox().await;
    let dir = project("demo", "");
    let root = dir.path();
    let pred = apb_engine::run(root, "demo", None, apb_engine::RunOptions::default()).unwrap();
    let app = build_router(AppState::new(root.to_path_buf()));
    let (status, started) = call(
        &app,
        "POST",
        "/api/playbooks/demo/run",
        None,
        Some(json!({ "continued_from": pred.run_id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let run_id = started["run_id"].as_str().unwrap().to_string();
    let _reaper = crate::driver_reaper::DriverReaper::new(root, &run_id);
    assert_eq!(launch_of(root, &run_id).kind, "continue_from");
}

/// The editor of a dashboard without keys never lowers a floor (409, the
/// floor stays and the fit report says so); with keys a lowering needs the
/// person's confirmation, and the confirmed one moves the floor.
#[tokio::test]
async fn the_editor_lowers_a_floor_only_with_keys_and_a_confirmation() {
    let _cfg = config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    apb_core::registry::init_project(root).unwrap();

    let keyless = build_router(AppState::new(root.to_path_buf()));
    let (status, body) = call(
        &keyless,
        "POST",
        "/api/playbooks",
        None,
        Some(json!({ "id": "demo", "yaml": yaml("demo", "agent_launch: deny\n") })),
    )
    .await;
    assert!(status.is_success(), "{status} {body}");
    let (status, fit) = call(&keyless, "GET", "/api/playbooks/demo/fit", None, None).await;
    assert_eq!(status, StatusCode::OK, "{fit}");
    assert_eq!(fit["launch"]["policy"], "deny", "{fit}");
    assert_eq!(fit["launch"]["floor"], "deny", "{fit}");
    assert!(!fit["launch"]["sources"].as_array().unwrap().is_empty());

    // An agent cannot save the looser version.
    let e = apb_mcp::tools::playbook_update(root, "demo", &yaml("demo", "")).unwrap_err();
    assert!(e.to_string().contains("launch_policy_loosened"), "{e}");

    // Neither can a dashboard without keys, even with a confirmation.
    let (status, body) = call(
        &keyless,
        "PUT",
        "/api/playbooks/demo",
        None,
        Some(json!({ "yaml": yaml("demo", "agent_launch: allow\n"), "confirm_lower_floor": "allow" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"], "launch_floor_lowering_needs_terminal",
        "{body}"
    );
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("apb launch floor demo --lower-to allow"),
        "{body}"
    );
    let (_, fit) = call(&keyless, "GET", "/api/playbooks/demo/fit", None, None).await;
    assert_eq!(fit["launch"]["floor"], "deny", "{fit}");

    // With keys: refused until the person confirms the exact lowering.
    let (key, state) = crate::common::keyed(root.to_path_buf());
    let keyed = build_router(state);
    let looser = yaml("demo", "agent_launch: allow\n");
    let (status, body) = call(
        &keyed,
        "PUT",
        "/api/playbooks/demo",
        Some(&key),
        Some(json!({ "yaml": looser })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "launch_floor_lowering_unconfirmed", "{body}");
    assert_eq!(body["declared"], "allow", "{body}");
    assert_eq!(body["floor"], "deny", "{body}");
    let (status, body) = call(
        &keyed,
        "PUT",
        "/api/playbooks/demo",
        Some(&key),
        Some(json!({ "yaml": looser, "confirm_lower_floor": "ask" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "another level is no confirmation: {body}"
    );
    let (status, body) = call(
        &keyed,
        "PUT",
        "/api/playbooks/demo",
        Some(&key),
        Some(json!({ "yaml": looser, "confirm_lower_floor": "allow" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, fit) = call(&keyed, "GET", "/api/playbooks/demo/fit", Some(&key), None).await;
    assert_eq!(fit["launch"]["policy"], "allow", "{fit}");
    assert_eq!(fit["launch"]["floor"], "allow", "{fit}");
    // An agent's save no longer meets the old floor.
    apb_mcp::tools::playbook_update(root, "demo", &yaml("demo", "")).unwrap();
}

/// A paused run with no launch record (as an older apb started it), whose
/// snapshot declares `decl`: stamped as created here, so the resume gate
/// accepts it, with a live process (this test) holding the workdir, so a
/// resume that passes the launch check stops at the busy workdir and spawns
/// no driver.
fn never_authorized_run(root: &Path, run_id: &str, decl: &str) {
    use apb_engine::event::{EventLog, EventPayload};
    let snapshot = format!(
        "schema: 2\nid: scripted\nname: Scripted\nversion: 1.0.0\n{decl}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, type: script, script: \"scripts/work.sh\", runner: sh }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: done }}\n"
    );
    let run_dir = root.join(".apb/runs").join(run_id);
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("playbook.yaml"), snapshot).unwrap();
    let mut log = EventLog::open(&run_dir).unwrap();
    for p in [
        EventPayload::RunStarted {
            playbook: "scripted".into(),
            version: "1.0.0".into(),
            definition: None,
        },
        EventPayload::NodeStarted {
            node: "start".into(),
            attempt: 1,
        },
        EventPayload::NodeFinished {
            node: "start".into(),
            status: "succeeded".into(),
            attempt: 1,
            output: String::new(),
            artifacts: Vec::new(),
        },
        EventPayload::RunPaused {
            reason: "away".into(),
            by: Some(apb_engine::pause::PauseBy::Operator),
            mode: None,
            until: None,
            keep_lock: false,
        },
    ] {
        log.append(p).unwrap();
    }
    apb_core::run_origin::stamp(&run_dir, run_id).unwrap();
    fs::write(
        root.join(".apb/workdir.lock"),
        std::process::id().to_string(),
    )
    .unwrap();
    assert!(apb_engine::gate::launch::run_authorization(&run_dir).is_none());
}

/// A never-authorized run of an `ask` or `deny` playbook: a keyless
/// dashboard resumes `ask` only with the refusal's nonce and never `deny`;
/// a dashboard with keys resumes `deny` as a person.
#[tokio::test]
async fn a_never_authorized_run_resumes_by_the_dashboards_rules() {
    let _cfg = config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    apb_core::registry::init_project(root).unwrap();
    apb_core::run_origin::ensure_key().unwrap();
    never_authorized_run(root, "ask-1", "agent_launch: ask\n");
    never_authorized_run(root, "deny-1", "agent_launch: deny\n");
    let keyless = build_router(AppState::new(root.to_path_buf()));

    let (status, refusal) = call(
        &keyless,
        "POST",
        "/api/runs/ask-1/resume",
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refusal}");
    assert_eq!(
        refusal["policy"], "launch_requires_confirmation",
        "{refusal}"
    );
    assert_eq!(refusal["run_id"], "ask-1");
    assert_eq!(refusal["launch_kind"], "resume");
    let nonce = refusal["launch_nonce"].as_str().unwrap().to_string();
    let (status, out) = call(
        &keyless,
        "POST",
        "/api/runs/ask-1/resume",
        None,
        Some(json!({ "confirm_launch": nonce })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{out}");
    assert_eq!(out["error"], "workdir_busy", "past the launch check: {out}");

    let (status, refusal) = call(
        &keyless,
        "POST",
        "/api/runs/deny-1/resume",
        None,
        Some(json!({ "confirm_launch": nonce })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refusal}");
    assert_eq!(refusal["policy"], "launch_requires_person", "{refusal}");
    assert!(refusal.get("launch_nonce").is_none(), "{refusal}");

    let (key, state) = crate::common::keyed(root.to_path_buf());
    let keyed = build_router(state);
    let (status, out) = call(
        &keyed,
        "POST",
        "/api/runs/deny-1/resume",
        Some(&key),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{out}");
    assert_eq!(out["error"], "workdir_busy", "past the launch check: {out}");
    for run in ["ask-1", "deny-1"] {
        let events = apb_engine::event::read_all(&root.join(".apb/runs").join(run)).unwrap();
        assert!(
            !events.iter().any(|e| matches!(
                e.payload,
                apb_engine::event::EventPayload::RunResumed { .. }
            )),
            "a refused resume journals nothing"
        );
    }
}
