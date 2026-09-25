use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::{RunState, RunStatus};

use crate::common;

// The agent self-reports success (echo ok -> no block -> success), but the
// success_check script decides the final status: exit 1 -> node Failed ->
// failure branch; exit 0 -> success.
const PLAYBOOK: &str = r#"
schema: 1
id: sc
name: SuccessCheck
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", success_check: "scripts/check.sh" }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: w }
  - { from: w, to: ok, condition: { type: node_status, node: w, equals: success } }
  - { from: w, to: no, fallback: true }
"#;

fn seed(root: &Path, check_exit: u8) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/sc/1.0.0");
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/sc/current"), "1.0.0").unwrap();
    common::seed_main(root);
    // The check script with the given exit code (copied to run_dir/scripts).
    fs::write(
        dir.join("scripts/check.sh"),
        format!("#!/bin/sh\nexit {check_exit}\n"),
    )
    .unwrap();
}

fn ok_agent(root: &Path) -> String {
    let path = root.join("ok-agent.sh");
    fs::write(&path, "#!/bin/sh\necho ok\n").unwrap();
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

// The agent self-reports success, but a `success_check: { marker: ... }`
// additionally requires the literal marker in the node output. The agent that
// omits the marker (echo "interim") is rejected as a self-contradictory success
// report; the one that emits it (echo with WAVE-COMPLETE) succeeds. This is the
// engine defense for issue 45 finding 1.
const MARKER_PLAYBOOK: &str = r#"
schema: 1
id: scm
name: SuccessCheckMarker
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", success_check: { marker: "WAVE-COMPLETE" } }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: w }
  - { from: w, to: ok, condition: { type: node_status, node: w, equals: success } }
  - { from: w, to: no, fallback: true }
"#;

fn seed_marker(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/scm/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), MARKER_PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/scm/current"), "1.0.0").unwrap();
    common::seed_main(root);
}

// An agent whose stdout is exactly `text` (its self-report is success: no block).
fn echo_agent(root: &Path, text: &str) -> String {
    let path = root.join("echo-agent.sh");
    fs::write(&path, format!("#!/bin/sh\necho '{text}'\n")).unwrap();
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

// Both branches sequentially: APB_AGENT_CMD is process-global.
#[test]
fn success_check_marker_requires_completion_marker() {
    let _env = common::env_lock();
    // 1. Output lacks the marker -> success report rejected -> node Failed.
    let miss = tempfile::tempdir().unwrap();
    seed_marker(miss.path());
    let prog = echo_agent(
        miss.path(),
        "ST6 dispatched. After it merges, ST8 then ST9 close the wave",
    );
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(miss.path(), "scm", None, RunOptions::default()).unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Failed,
        "a success report without the completion marker must fail the node"
    );

    // 2. Output contains the marker -> success.
    let hit = tempfile::tempdir().unwrap();
    seed_marker(hit.path());
    let prog2 = echo_agent(hit.path(), "all workers merged WAVE-COMPLETE");
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog2);
    }
    let res2 = run(hit.path(), "scm", None, RunOptions::default()).unwrap();

    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    assert_eq!(
        res2.outcome,
        RunStatus::Succeeded,
        "a success report containing the completion marker must succeed"
    );
}

// A marker playbook with a retry budget: a rejected success report must behave
// like any other attempt failure - consume a retry (max_retries honored) and
// fail terminally only after the budget is spent.
const MARKER_RETRY_PLAYBOOK: &str = r#"
schema: 1
id: scmr
name: SuccessCheckMarkerRetry
version: 1.0.0
defaults:
  profile: main
  max_retries: 1
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", success_check: { marker: "WAVE-COMPLETE" } }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: w }
  - { from: w, to: ok, condition: { type: node_status, node: w, equals: success } }
  - { from: w, to: no, fallback: true }
"#;

fn seed_marker_retry(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/scmr/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), MARKER_RETRY_PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/scmr/current"), "1.0.0").unwrap();
    common::seed_main(root);
}

// A rejected success report consumes a retry (max_retries=1 -> a RetryStarted
// event before the node ends Failed) AND its discarded agent text is exposed to
// downstream templates as `nodes.<id>.rejected_output` (via the RunState fold).
#[test]
fn success_check_rejection_consumes_retry_and_exposes_rejected_output() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed_marker_retry(dir.path());
    // Every attempt reports success but omits the marker -> rejected each time.
    let agent_text = "ST6 dispatched. interim progress only, wave not closed";
    let prog = echo_agent(dir.path(), agent_text);
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "scmr", None, RunOptions::default()).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    assert_eq!(
        res.outcome,
        RunStatus::Failed,
        "a success report rejected on every attempt must end the node Failed"
    );

    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::RetryStarted { .. })),
        "a rejected success report must consume a retry (max_retries=1) - expected a retry_started event"
    );
    let s = RunState::fold(&events);
    assert_eq!(
        s.rejected_outputs.get("w").map(|t| t.trim()),
        Some(agent_text),
        "the discarded agent report text must be exposed as nodes.w.rejected_output"
    );
}

// Both branches sequentially: APB_AGENT_CMD is process-global.
#[test]
fn success_check_overrides_agent_self_assessment() {
    let _env = common::env_lock();
    // 1. The check fails (exit 1) -> node Failed despite echo ok.
    let fail = tempfile::tempdir().unwrap();
    seed(fail.path(), 1);
    let prog = ok_agent(fail.path());
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(fail.path(), "sc", None, RunOptions::default()).unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Failed,
        "failing success_check must fail the node"
    );

    // 2. The check passes (exit 0) -> success.
    let pass = tempfile::tempdir().unwrap();
    seed(pass.path(), 0);
    let prog2 = ok_agent(pass.path());
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog2);
    }
    let res2 = run(pass.path(), "sc", None, RunOptions::default()).unwrap();

    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    assert_eq!(
        res2.outcome,
        RunStatus::Succeeded,
        "passing success_check must allow success"
    );
}

/// Issue #107: between the agent process exiting and the drive journaling its
/// `attempt_finished`, the drive is still working on the attempt (reading the
/// status file, running the `success_check`, which can take seconds). A reader
/// must see that run as running, not interrupted: the attempt's pid is gone,
/// but the drive recorded that itself and is finishing the attempt. The check
/// blocks on a file the test controls, so the window is held open by
/// construction, not by timing.
#[test]
fn a_run_reads_running_while_the_drive_post_processes_an_exited_attempt() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), 0);
    let entered = dir.path().join("check-entered");
    let release = dir.path().join("check-release");
    // Bounded: a test that never releases fails instead of hanging.
    common::write_sync(
        &dir.path().join(".apb/playbooks/sc/1.0.0/scripts/check.sh"),
        &format!(
            "#!/bin/sh\n: > '{e}'\ni=0\nwhile [ ! -f '{r}' ] && [ $i -lt 1200 ]; do sleep 0.05; i=$((i+1)); done\nexit 0\n",
            e = entered.display(),
            r = release.display()
        ),
    );
    let prog = ok_agent(dir.path());
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let root = dir.path().to_path_buf();
    let drive = std::thread::spawn(move || run(&root, "sc", None, RunOptions::default()));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !entered.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the check never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let runs = dir.path().join(".apb/runs");
    let run_id = fs::read_dir(&runs)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .next()
        .expect("a run dir");
    let view = apb_engine::run_view::RunView::load(&runs.join(&run_id), &run_id).unwrap();
    let nodes = view.nodes();
    fs::write(&release, "").unwrap();
    let res = drive.join().unwrap().unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }

    assert_eq!(
        view.run_status,
        RunStatus::Running,
        "a run whose drive is finishing an exited attempt is running"
    );
    assert_eq!(
        nodes.get("w").map(String::as_str),
        Some("running"),
        "{nodes:?}"
    );
    assert_eq!(res.outcome, RunStatus::Succeeded);
}
