// Cross-workspace async tests hold a std MutexGuard (env serialization)
// across an await. This is safe: #[tokio::test] uses a current-thread
// runtime, so no other task on this same thread interleaves. We silence
// the lint precisely at the test-module level.
#![allow(clippy::await_holding_lock)]
use super::*;
use rmcp::handler::server::wrapper::Parameters;
use std::fs;
use std::path::Path;

const VALID: &str = include_str!("../../../apb-core/tests/fixtures/valid.yaml");

fn seeded_root() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    seed(dir.path());
    dir
}

fn seed(root: &Path) {
    apb_core::registry::init_project(root).expect("init_project");
    let vdir = root.join(".apb/playbooks/implement-task/1.0.0");
    fs::create_dir_all(&vdir).expect("mkdir version dir");
    fs::write(vdir.join("playbook.yaml"), VALID).expect("write valid.yaml");
    fs::write(root.join(".apb/playbooks/implement-task/current"), "1.0.0")
        .expect("write current pointer");
    fs::create_dir_all(root.join(".apb/profiles/architect")).expect("mkdir profiles");
}

// A pipeline without agent_task - for supervise:"self" tests, where all
// that matters is the token being returned instantly, not the eventual
// outcome of the background run.
const NOAGENT: &str = r#"
schema: 1
id: noagent_sv
name: No Agent Supervised
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hi" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

fn seed_noagent(root: &Path) {
    apb_core::registry::init_project(root).expect("init_project");
    let vdir = root.join(".apb/playbooks/noagent_sv/1.0.0");
    fs::create_dir_all(&vdir).expect("mkdir version dir");
    fs::write(vdir.join("playbook.yaml"), NOAGENT).expect("write noagent playbook.yaml");
    fs::write(root.join(".apb/playbooks/noagent_sv/current"), "1.0.0")
        .expect("write current pointer");
}

// A pipeline with a script node sleeping for 1 second. This gives a
// window in which the background run is provably not yet terminal: this
// way the test actually proves the start is non-blocking (a synchronous
// run would take >= 1s), not just fast. The same technique as in
// apb-engine/tests/background_run_test.rs.
const SLOWSCRIPT: &str = r#"
schema: 1
id: slowscript
name: Slow Script
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/slow.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

fn seed_slowscript(root: &Path) {
    apb_core::registry::init_project(root).expect("init_project");
    let vdir = root.join(".apb/playbooks/slowscript/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).expect("mkdir version+scripts dir");
    fs::write(vdir.join("playbook.yaml"), SLOWSCRIPT).expect("write slowscript playbook.yaml");
    fs::write(vdir.join("scripts/slow.sh"), "#!/bin/sh\nsleep 1\n").expect("write slow.sh");
    fs::write(root.join(".apb/playbooks/slowscript/current"), "1.0.0")
        .expect("write current pointer");
}

/// Extracts the text of the tool result's first text content block
/// (in this server, JSON responses are also encoded as a text block).
fn result_text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| block.as_text())
        .map(|t| t.text.clone())
        .collect::<Vec<_>>()
        .join("")
}

#[test]
fn tool_router_registers_all_read_run_write_and_supervisor_tools() {
    let server = WfMcp::new(PathBuf::from("."));
    let names: Vec<String> = server
        .tool_router
        .list_all()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    let expected = [
        "playbook_list",
        "playbook_catalog",
        "projects_list",
        "playbook_howto",
        "run_progress_report",
        "profile_list",
        "profile_get",
        "profile_write",
        "profile_move",
        "profile_delete",
        "agents_detect",
        "profile_howto",
        "subscriptions_set",
        "playbook_adopt_report",
        "playbook_capture",
        "suggestion_dismiss",
        "playbook_trial",
        "playbook_approve",
        "playbook_prepare_run",
        "playbook_execute_plan",
        "playbook_get",
        "playbook_validate",
        "playbook_run",
        "playbook_create",
        "playbook_update",
        "playbook_delete",
        "runs_list",
        "run_status",
        "run_events",
        "run_report",
        "run_resume",
        "review_decide",
        "supervisor_wait_event",
        "supervisor_run_inspect",
        "supervisor_node_retry",
        "supervisor_run_continue_from",
        "supervisor_run_pause",
        "supervisor_run_abort",
        "supervisor_context_append",
        "supervisor_report",
        "supervisor_patch_playbook",
    ];
    for name in expected {
        assert!(
            names.contains(&name.to_string()),
            "missing tool `{name}` in router"
        );
    }
    assert_eq!(
        names.len(),
        expected.len(),
        "unexpected extra tools registered: {names:?}"
    );
}

/// Tools carry safety annotations: read-only on reads, destructive on
/// mutations. Improves approval UX in MCP clients (Claude Desktop, etc.).
#[test]
fn tools_carry_safety_annotations() {
    let server = WfMcp::new(PathBuf::from("."));
    let tools = server.tool_router.list_all();
    let find = |n: &str| {
        tools
            .iter()
            .find(|t| t.name.as_ref() == n)
            .unwrap_or_else(|| panic!("missing tool `{n}`"))
            .clone()
    };
    let read_only = |n: &str| find(n).annotations.as_ref().and_then(|a| a.read_only_hint);
    let destructive = |n: &str| {
        find(n)
            .annotations
            .as_ref()
            .and_then(|a| a.destructive_hint)
    };

    for n in [
        "playbook_list",
        "playbook_get",
        "playbook_validate",
        "runs_list",
        "run_status",
        "run_events",
        "run_report",
        "supervisor_wait_event",
        "supervisor_run_inspect",
    ] {
        assert_eq!(read_only(n), Some(true), "tool `{n}` must be read-only");
        assert_ne!(
            destructive(n),
            Some(true),
            "read-only tool `{n}` must not be destructive"
        );
    }
    for n in [
        "playbook_run",
        "playbook_create",
        "playbook_update",
        "playbook_delete",
        "run_resume",
        "review_decide",
        "supervisor_node_retry",
        "supervisor_run_continue_from",
        "supervisor_run_pause",
        "supervisor_run_abort",
        "supervisor_context_append",
        "supervisor_report",
        "supervisor_patch_playbook",
    ] {
        assert_eq!(destructive(n), Some(true), "tool `{n}` must be destructive");
        assert_ne!(
            read_only(n),
            Some(true),
            "destructive tool `{n}` must not be read-only"
        );
    }
}

#[tokio::test]
async fn playbook_list_returns_success_json_content() {
    let dir = seeded_root();
    let server = WfMcp::new(dir.path().to_path_buf());
    let result = server
        .playbook_list(Parameters(WorkspaceArg::default()))
        .await;
    assert_eq!(result.is_error, Some(false));
    assert_eq!(result.content.len(), 1);
}

#[tokio::test]
async fn playbook_get_missing_id_surfaces_as_tool_error() {
    let dir = seeded_root();
    let server = WfMcp::new(dir.path().to_path_buf());
    let result = server
        .playbook_get(Parameters(PlaybookGetArgs {
            id: "does-not-exist".to_string(),
            version: None,
            workspace: None,
        }))
        .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.content.len(), 1);
}

#[tokio::test]
async fn run_status_missing_run_surfaces_as_tool_error() {
    let dir = seeded_root();
    let server = WfMcp::new(dir.path().to_path_buf());
    let result = server
        .run_status(Parameters(RunRefArgs {
            run_id: "missing-run".to_string(),
            workspace: None,
        }))
        .await;
    assert_eq!(result.is_error, Some(true));
}

#[tokio::test]
async fn supervise_self_returns_token() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_noagent(dir.path());
    let server = WfMcp::new(dir.path().to_path_buf());

    let result = server
        .playbook_run(Parameters(PlaybookRunArgs {
            id: "noagent_sv".to_string(),
            version: None,
            params: BTreeMap::new(),
            instruction: None,
            supervise: Some("self".to_string()),
            background: None,
            // The pipeline is seeded directly (untrusted), and the gate now
            // also applies to supervise:self - confirm explicitly.
            acknowledge_untrusted: Some(true),
            scope: None,
        }))
        .await;

    assert_eq!(
        result.is_error,
        Some(false),
        "unexpected error: {}",
        result_text(&result)
    );
    let text = result_text(&result);
    assert!(
        text.contains("supervisor_token"),
        "expected supervisor_token in response, got: {text}"
    );
}

/// Non-blocking start (`background: true`): a run with a script node
/// sleeping for 1s returns the run_id noticeably faster than a second AND
/// is not yet terminal right after returning. A synchronous run would
/// take >= 1s, so both facts together prove a genuinely non-blocking
/// start, not just a fast run.
#[tokio::test]
async fn background_run_returns_run_id_without_blocking() {
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().expect("tempdir");
    seed_slowscript(dir.path());
    let server = WfMcp::new(dir.path().to_path_buf());

    let started = Instant::now();
    let result = server
        .playbook_run(Parameters(PlaybookRunArgs {
            id: "slowscript".to_string(),
            version: None,
            params: BTreeMap::new(),
            instruction: None,
            supervise: None,
            background: Some(true),
            // The pipeline is seeded directly (untrusted); the test confirms
            // explicitly so it verifies the non-blocking start specifically,
            // not the policy gate.
            acknowledge_untrusted: Some(true),
            scope: None,
        }))
        .await;
    let elapsed = started.elapsed();
    assert_eq!(
        result.is_error,
        Some(false),
        "unexpected error: {}",
        result_text(&result)
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "background start blocked for {elapsed:?}"
    );

    let text = result_text(&result);
    let v: serde_json::Value = serde_json::from_str(&text).expect("json body");
    let run_id = v["run_id"].as_str().expect("run_id present").to_string();

    // Right after returning, the script is still sleeping (~1s), so the
    // run must not be terminal. A status-read error at this point also
    // means "not finished yet" - we only fail if the status is already
    // terminal.
    if let Ok(status) = tools::run_status(dir.path(), &run_id) {
        let st = status["run_status"].as_str().unwrap_or("").to_string();
        assert!(
            st != "succeeded" && st != "failed",
            "run must still be in flight right after a non-blocking start, got: {st}"
        );
    }

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = tools::run_status(dir.path(), &run_id).expect("run_status");
        match status["run_status"].as_str().unwrap_or("") {
            "succeeded" | "failed" => break,
            other => {
                assert!(
                    Instant::now() < deadline,
                    "run did not finish, last status: {other}"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

#[test]
fn patch_playbook_tool_maps_to_patch_capability() {
    assert_eq!(
        capability_for_tool("supervisor_patch_playbook"),
        "patch_playbook"
    );
}

#[test]
fn patch_playbook_rejected_without_capability() {
    let dir = tempfile::tempdir().unwrap();
    let server = WfMcp::new(dir.path().to_path_buf());
    // A session with only observe - patch_playbook is not granted.
    let token = server.mint_token("run-x".to_string(), vec!["observe".to_string()]);
    let err = server
        .resolve_session(&token, "supervisor_patch_playbook")
        .unwrap_err();
    assert!(err.to_string().contains("patch_playbook"));
}

#[test]
fn patch_playbook_allowed_with_capability() {
    let dir = tempfile::tempdir().unwrap();
    let server = WfMcp::new(dir.path().to_path_buf());
    let token = server.mint_token("run-x".to_string(), vec!["patch_playbook".to_string()]);
    assert_eq!(
        server
            .resolve_session(&token, "supervisor_patch_playbook")
            .unwrap(),
        "run-x"
    );
}

#[tokio::test]
async fn supervisor_tool_rejects_unknown_token() {
    let dir = seeded_root();
    let server = WfMcp::new(dir.path().to_path_buf());

    let result = server
        .supervisor_run_inspect(Parameters(SupervisorRunRefArgs {
            token: "bogus".to_string(),
        }))
        .await;

    assert_eq!(result.is_error, Some(true));
}

#[tokio::test]
async fn capability_gate_blocks_retry_when_observe_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_noagent(dir.path());
    let server = WfMcp::new(dir.path().to_path_buf());

    // A real run, so that supervisor_run_inspect passes the gate and
    // reaches the engine (rather than just not hitting a token/capability
    // error).
    let started = tools::playbook_run_supervised(
        dir.path(),
        "noagent_sv",
        None,
        BTreeMap::new(),
        None,
        None,
        None,
    )
    .expect("playbook_run_supervised");
    let run_id = started["run_id"].as_str().expect("run_id").to_string();

    let token = server.mint_token(run_id, vec!["observe".to_string()]);

    let retry_result = server
        .supervisor_node_retry(Parameters(SupervisorRetryArgs {
            token: token.clone(),
            node: "note".to_string(),
            prompt_override: None,
        }))
        .await;
    assert_eq!(
        retry_result.is_error,
        Some(true),
        "retry capability should be denied for observe-only session"
    );
    assert!(
        result_text(&retry_result).contains("capability"),
        "expected capability-denied message, got: {}",
        result_text(&retry_result)
    );

    let inspect_result = server
        .supervisor_run_inspect(Parameters(SupervisorRunRefArgs { token }))
        .await;
    assert_eq!(
        inspect_result.is_error,
        Some(false),
        "observe capability should allow run_inspect: {}",
        result_text(&inspect_result)
    );
}

// Disk fallback of token resolution (Task 3, Phase 4c): the token is
// persisted to disk with one call to write_supervisor_session (as
// mint_token does), and is resolved by a completely fresh WfMcp with an
// empty in-memory session table - this is exactly how a separate
// `apb mcp` process (for a background agent, Task 4) validates a token
// issued by the process that started the run.
#[tokio::test]
async fn resolve_session_falls_back_to_disk_when_in_memory_table_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    seed_noagent(dir.path());

    let started = tools::playbook_run_supervised(
        dir.path(),
        "noagent_sv",
        None,
        BTreeMap::new(),
        None,
        None,
        None,
    )
    .expect("playbook_run_supervised");
    let run_id = started["run_id"].as_str().expect("run_id").to_string();

    apb_engine::write_supervisor_session(
        dir.path(),
        &run_id,
        "sv-disk-1",
        &["observe".to_string(), "retry".to_string()],
    )
    .expect("write_supervisor_session");

    let server = WfMcp::new(dir.path().to_path_buf());

    let inspect_result = server
        .supervisor_run_inspect(Parameters(SupervisorRunRefArgs {
            token: "sv-disk-1".to_string(),
        }))
        .await;
    assert_eq!(
        inspect_result.is_error,
        Some(false),
        "disk-persisted token should resolve past the gate: {}",
        result_text(&inspect_result)
    );

    // Capabilities from disk must also be enforced: an unknown token
    // without a disk persist remains refused.
    let unknown_result = server
        .supervisor_run_inspect(Parameters(SupervisorRunRefArgs {
            token: "sv-nowhere".to_string(),
        }))
        .await;
    assert_eq!(unknown_result.is_error, Some(true));
}

// Disk fallback + capability gate: the token from disk has only observe,
// but a retry-class tool must be refused. Checks that the capability gate
// is enforced on the disk-fallback path of resolve_session too, not only
// on the in-memory path.
#[tokio::test]
async fn disk_resolved_observe_only_token_is_denied_retry_tool() {
    let dir = tempfile::tempdir().unwrap();
    seed_noagent(dir.path());

    let started = tools::playbook_run_supervised(
        dir.path(),
        "noagent_sv",
        None,
        BTreeMap::new(),
        None,
        None,
        None,
    )
    .expect("playbook_run_supervised");
    let run_id = started["run_id"].as_str().expect("run_id").to_string();

    apb_engine::write_supervisor_session(
        dir.path(),
        &run_id,
        "sv-disk-observe-only",
        &["observe".to_string()],
    )
    .expect("write_supervisor_session");

    let server = WfMcp::new(dir.path().to_path_buf());

    let retry_result = server
        .supervisor_node_retry(Parameters(SupervisorRetryArgs {
            token: "sv-disk-observe-only".to_string(),
            node: "note".to_string(),
            prompt_override: None,
        }))
        .await;
    assert_eq!(
        retry_result.is_error,
        Some(true),
        "disk-resolved observe-only token should be denied retry capability"
    );
    assert!(
        result_text(&retry_result).contains("capability"),
        "expected capability-denied message, got: {}",
        result_text(&retry_result)
    );

    let inspect_result = server
        .supervisor_run_inspect(Parameters(SupervisorRunRefArgs {
            token: "sv-disk-observe-only".to_string(),
        }))
        .await;
    assert_eq!(
        inspect_result.is_error,
        Some(false),
        "disk-resolved observe-only token should pass observe capability gate: {}",
        result_text(&inspect_result)
    );
}

// Cross-workspace reads (spec 7, Task 10). Env-dependent tests are
// serialized with a lock, so as not to race APB_CONFIG_DIR in parallel.
static CROSS_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn seed_noagent_run(root: &Path) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/noagent/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    let yaml = NOAGENT
        .replace("id: noagent_sv", "id: noagent")
        .replace("No Agent Supervised", "No Agent");
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/noagent/current"), "1.0.0").unwrap();
}

#[tokio::test]
async fn run_status_resolves_foreign_workspace() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = tempfile::tempdir().unwrap();
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
        std::env::remove_var("APB_NO_REGISTRY");
        std::env::remove_var("CI");
    }

    seed_noagent_run(a.path());
    seed_noagent_run(b.path());
    apb_core::projects::touch(a.path());
    apb_core::projects::touch(b.path());
    let b_id = apb_core::workspace::ensure_id(b.path()).unwrap();

    let run_id =
        apb_engine::run_background(b.path(), "noagent", None, apb_engine::RunOptions::default())
            .unwrap();
    let run_dir = b.path().join(".apb/runs").join(&run_id);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if let Ok(events) = apb_engine::event::read_all(&run_dir)
            && events.iter().any(|e| {
                matches!(
                    e.payload,
                    apb_engine::event::EventPayload::RunFinished { .. }
                )
            })
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let server = WfMcp::new(a.path().to_path_buf());
    let res = server
        .run_status(Parameters(RunRefArgs {
            run_id: run_id.clone(),
            workspace: Some(b_id),
        }))
        .await;
    assert_eq!(
        res.is_error,
        Some(false),
        "unexpected error: {}",
        result_text(&res)
    );
    assert!(result_text(&res).contains(&run_id));

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

#[tokio::test]
async fn unreachable_workspace_returns_structured_error() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = tempfile::tempdir().unwrap();
    let a = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
        std::env::remove_var("APB_NO_REGISTRY");
        std::env::remove_var("CI");
    }

    seed_noagent_run(a.path());
    let gone = tempfile::tempdir().unwrap();
    seed_noagent_run(gone.path());
    apb_core::projects::touch(gone.path());
    let gone_id = apb_core::workspace::ensure_id(gone.path()).unwrap();
    drop(gone);

    let server = WfMcp::new(a.path().to_path_buf());
    let res = server
        .run_status(Parameters(RunRefArgs {
            run_id: "whatever".into(),
            workspace: Some(gone_id),
        }))
        .await;
    assert!(
        result_text(&res).contains("workspace_unreachable"),
        "got: {}",
        result_text(&res)
    );

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

// Two-phase cross-workspace launch (spec 7, Task 11).
async fn prepared_token(server: &WfMcp, b_id: &str) -> serde_json::Value {
    let res = server
        .playbook_prepare_run(Parameters(PlaybookPrepareRunArgs {
            id: "noagent".into(),
            version: None,
            workspace: b_id.to_string(),
            params: BTreeMap::new(),
        }))
        .await;
    serde_json::from_str(&result_text(&res)).unwrap()
}

fn setup_two(cfg: &Path) -> (tempfile::TempDir, tempfile::TempDir, String) {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg);
        std::env::remove_var("APB_NO_REGISTRY");
        std::env::remove_var("CI");
    }
    seed_noagent_run(a.path());
    seed_noagent_run(b.path());
    // Pipeline B must be trusted+active, so that preflight lets it through.
    tools::approve_local(b.path(), "noagent", "1.0.0");
    apb_core::projects::touch(b.path());
    let b_id = apb_core::workspace::ensure_id(b.path()).unwrap();
    (a, b, b_id)
}

fn run_finished(root: &Path, run_id: &str) -> bool {
    let run_dir = root.join(".apb/runs").join(run_id);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if let Ok(events) = apb_engine::event::read_all(&run_dir)
            && events.iter().any(|e| {
                matches!(
                    e.payload,
                    apb_engine::event::EventPayload::RunFinished { .. }
                )
            })
        {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

#[tokio::test]
async fn prepare_then_execute_runs_in_target_workspace() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = tempfile::tempdir().unwrap();
    let (a, b, b_id) = setup_two(cfg.path());

    let server = WfMcp::new(a.path().to_path_buf());
    let plan = prepared_token(&server, &b_id).await;
    let token = plan["plan_token"].as_str().unwrap().to_string();

    let res = server
        .playbook_execute_plan(Parameters(PlaybookExecutePlanArgs {
            plan_token: token,
            acknowledge_untrusted: None,
        }))
        .await;
    let out: serde_json::Value = serde_json::from_str(&result_text(&res)).unwrap();
    let run_id = out["run_ref"]["run_id"]
        .as_str()
        .expect("run_id present")
        .to_string();
    assert_eq!(out["run_ref"]["workspace_id"], b_id);
    assert!(
        run_finished(b.path(), &run_id),
        "run should finish in target workspace B"
    );

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

#[tokio::test]
async fn token_is_single_use() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = tempfile::tempdir().unwrap();
    let (a, _b, b_id) = setup_two(cfg.path());
    let server = WfMcp::new(a.path().to_path_buf());
    let plan = prepared_token(&server, &b_id).await;
    let token = plan["plan_token"].as_str().unwrap().to_string();

    let first = server
        .playbook_execute_plan(Parameters(PlaybookExecutePlanArgs {
            plan_token: token.clone(),
            acknowledge_untrusted: None,
        }))
        .await;
    assert!(result_text(&first).contains("run_ref"));
    let second = server
        .playbook_execute_plan(Parameters(PlaybookExecutePlanArgs {
            plan_token: token,
            acknowledge_untrusted: None,
        }))
        .await;
    assert!(
        result_text(&second).contains("plan_replayed"),
        "got: {}",
        result_text(&second)
    );

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

#[tokio::test]
async fn digest_drift_invalidates_plan() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = tempfile::tempdir().unwrap();
    let (a, b, b_id) = setup_two(cfg.path());
    let server = WfMcp::new(a.path().to_path_buf());
    let plan = prepared_token(&server, &b_id).await;
    let token = plan["plan_token"].as_str().unwrap().to_string();

    // Drift: edit the definition in B after prepare.
    let vpath = b.path().join(".apb/playbooks/noagent/1.0.0/playbook.yaml");
    let cur = std::fs::read_to_string(&vpath).unwrap();
    std::fs::write(&vpath, format!("{cur}# drift\n")).unwrap();
    // Re-approve the new digest, so preflight does not fail on trust and
    // we verify plan_stale specifically.
    tools::approve_local(b.path(), "noagent", "1.0.0");

    let res = server
        .playbook_execute_plan(Parameters(PlaybookExecutePlanArgs {
            plan_token: token,
            acknowledge_untrusted: None,
        }))
        .await;
    assert!(
        result_text(&res).contains("plan_stale"),
        "got: {}",
        result_text(&res)
    );

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

#[tokio::test]
async fn tampered_token_is_rejected() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = tempfile::tempdir().unwrap();
    let (a, _b, b_id) = setup_two(cfg.path());
    let server = WfMcp::new(a.path().to_path_buf());
    let plan = prepared_token(&server, &b_id).await;
    let token = plan["plan_token"].as_str().unwrap().to_string();
    // Flip the last character of the signature.
    let mut chars: Vec<char> = token.chars().collect();
    let last = chars.len() - 1;
    chars[last] = if chars[last] == '0' { '1' } else { '0' };
    let forged: String = chars.into_iter().collect();

    let res = server
        .playbook_execute_plan(Parameters(PlaybookExecutePlanArgs {
            plan_token: forged,
            acknowledge_untrusted: None,
        }))
        .await;
    assert!(
        result_text(&res).contains("invalid_plan_token"),
        "got: {}",
        result_text(&res)
    );

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

// Review regression (Critical #1): a local playbook cannot be run past
// the gate by giving its own workspace_id in the two-phase contract.
#[tokio::test]
async fn self_workspace_prepare_is_rejected() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = tempfile::tempdir().unwrap();
    let (a, _b, _b_id) = setup_two(cfg.path());
    let server = WfMcp::new(a.path().to_path_buf());
    let a_id = apb_core::workspace::ensure_id(a.path()).unwrap();

    let res = server
        .playbook_prepare_run(Parameters(PlaybookPrepareRunArgs {
            id: "noagent".into(),
            version: None,
            workspace: a_id,
            params: BTreeMap::new(),
        }))
        .await;
    assert!(
        result_text(&res).contains("use_playbook_run_for_current_workspace"),
        "got: {}",
        result_text(&res)
    );

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

// Review regression (Critical #1): an untrusted playbook of another
// workspace does not run without acknowledge.
#[tokio::test]
async fn untrusted_foreign_plan_requires_acknowledge() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = tempfile::tempdir().unwrap();
    // Like setup_two, but we do NOT approve playbook B.
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
        std::env::remove_var("APB_NO_REGISTRY");
        std::env::remove_var("CI");
    }
    seed_noagent_run(a.path());
    seed_noagent_run(b.path());
    apb_core::projects::touch(b.path());
    let b_id = apb_core::workspace::ensure_id(b.path()).unwrap();

    let server = WfMcp::new(a.path().to_path_buf());
    let plan = prepared_token(&server, &b_id).await;
    assert_eq!(plan["plan"]["trusted"], false, "B playbook is untrusted");
    let token = plan["plan_token"].as_str().unwrap().to_string();

    let refused = server
        .playbook_execute_plan(Parameters(PlaybookExecutePlanArgs {
            plan_token: token.clone(),
            acknowledge_untrusted: None,
        }))
        .await;
    assert!(
        result_text(&refused).contains("untrusted_requires_acknowledge"),
        "got: {}",
        result_text(&refused)
    );

    let ok = server
        .playbook_execute_plan(Parameters(PlaybookExecutePlanArgs {
            plan_token: token,
            acknowledge_untrusted: Some(true),
        }))
        .await;
    assert!(
        result_text(&ok).contains("run_ref"),
        "got: {}",
        result_text(&ok)
    );

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

// Review regression (Important #2): a global playbook is started via
// playbook_run with scope=global and runs in the current project.
#[tokio::test]
async fn global_scope_playbook_runs_in_current_project() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
        std::env::remove_var("APB_NO_REGISTRY");
        std::env::remove_var("CI");
    }
    apb_core::registry::init_project(proj.path()).unwrap();
    // A global playbook lives at <config_dir>/playbooks/ (without .apb).
    let gdir = cfg.path().join("playbooks/noagent/1.0.0");
    std::fs::create_dir_all(&gdir).unwrap();
    let gyaml = NOAGENT
        .replace("id: noagent_sv", "id: noagent")
        .replace("No Agent Supervised", "No Agent");
    std::fs::write(gdir.join("playbook.yaml"), &gyaml).unwrap();
    std::fs::write(cfg.path().join("playbooks/noagent/current"), "1.0.0").unwrap();
    // Approve the digest, so the gate lets it through without acknowledge.
    let mut trust = apb_core::trust::TrustStore::load();
    trust
        .approve(
            &apb_core::scope::digest_str(&gyaml),
            "noagent",
            apb_core::trust::OriginKind::LocallyApproved,
        )
        .unwrap();

    let server = WfMcp::new(proj.path().to_path_buf());
    let res = server
        .playbook_run(Parameters(PlaybookRunArgs {
            id: "noagent".into(),
            version: None,
            params: BTreeMap::new(),
            instruction: None,
            supervise: None,
            background: None,
            acknowledge_untrusted: None,
            scope: Some("global".into()),
        }))
        .await;
    let out: serde_json::Value = serde_json::from_str(&result_text(&res)).unwrap();
    let run_id = out["run_id"].as_str().expect("run_id present").to_string();
    assert_eq!(out["scope"], "global");
    // The run landed in the project's .apb/runs, not in the global store.
    assert!(
        run_finished(proj.path(), &run_id),
        "global playbook should run in the current project"
    );

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}
