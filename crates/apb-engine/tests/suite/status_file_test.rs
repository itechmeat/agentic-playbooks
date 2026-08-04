//! Per-attempt `APB_STATUS_FILE` protocol (subtask S2). Each agent_task attempt
//! is handed a per-attempt JSON status file; the engine reads it FIRST when
//! deciding the attempt's status and outputs and falls back to the textual
//! report when the file is absent or invalid. When the node has a
//! `success_check`, the assembled prompt mentions the file. These end-to-end
//! tests drive real stub agents (via `APB_AGENT_CMD`) that write to
//! `$APB_STATUS_FILE`, exercising the env wiring through the headless adapter.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::event::read_all;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::{RunState, RunStatus};

use crate::common;

// A plain agent_task node (no success_check): the branch is chosen purely by the
// node's final status, so it reveals whether the status file or the textual
// report decided the attempt.
const PLAYBOOK: &str = r#"
schema: 1
id: sf
name: StatusFile
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do" }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: w }
  - { from: w, to: ok, condition: { type: node_status, node: w, equals: success } }
  - { from: w, to: no, fallback: true }
"#;

// A node WITH a marker success_check: used only to prove the prompt mentions the
// status file. The stub echoes the marker so the report is accepted.
const CHECK_PLAYBOOK: &str = r#"
schema: 1
id: sfc
name: StatusFileCheck
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", success_check: { marker: "done" } }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: w }
  - { from: w, to: ok, condition: { type: node_status, node: w, equals: success } }
  - { from: w, to: no, fallback: true }
"#;

// Two linear agent_task nodes: `a` runs first and plants a stale status file at
// `w`'s attempt-1 path (as a prior execution's leftover would appear on a
// resume/continue_from re-run); `w` then writes no status file of its own.
const STALE_PLAYBOOK: &str = r#"
schema: 1
id: sfstale
name: StatusFileStale
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: a, type: agent_task, prompt: "a" }
  - { id: w, type: agent_task, prompt: "w" }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: a }
  - { from: a, to: w }
  - { from: w, to: ok, condition: { type: node_status, node: w, equals: success } }
  - { from: w, to: no, fallback: true }
"#;

fn seed(root: &Path, id: &str, body: &str) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), body).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    common::seed_main(root);
}

// Writes an executable stub agent script and returns its path.
fn stub_agent(root: &Path, script: &str) -> String {
    let path = root.join("stub-agent.sh");
    common::write_sync(&path, script);
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

// 1. The agent's TEXT reply parses as failure, but it writes success to
//    `$APB_STATUS_FILE`. The engine reads the file first, so the node succeeds.
#[test]
fn status_file_success_overrides_agent_text() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "sf", PLAYBOOK);
    // stdout ends with a yaml block reporting failure; the status file says
    // success.
    let script = "#!/bin/sh\nprintf '%s' '{\"status\":\"success\"}' > \"$APB_STATUS_FILE\"\n\
        printf 'work done\\n```yaml\\nstatus: failure\\n```\\n'\n";
    let prog = stub_agent(dir.path(), script);
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "sf", None, RunOptions::default()).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "a success status file must override a failure-parsing text reply"
    );
}

// 2. No status file: the engine falls back to parsing the textual report, whose
//    trailing yaml block reports failure -> node fails.
#[test]
fn absent_status_file_falls_back_to_text_parsing() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "sf", PLAYBOOK);
    // Does NOT touch $APB_STATUS_FILE; ends with a failure report block.
    let script = "#!/bin/sh\nprintf 'did some work\\n```yaml\\nstatus: failure\\n```\\n'\n";
    let prog = stub_agent(dir.path(), script);
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "sf", None, RunOptions::default()).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    assert_eq!(
        res.outcome,
        RunStatus::Failed,
        "with no status file the textual failure report must decide the node"
    );
}

// 3. The status file carries a non-empty `outputs` object; its compact JSON
//    becomes the node's downstream output.
#[test]
fn status_file_outputs_flow_downstream() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "sf", PLAYBOOK);
    let script = "#!/bin/sh\nprintf '%s' '{\"status\":\"success\",\"outputs\":{\"key\":\"val\"}}' \
        > \"$APB_STATUS_FILE\"\nprintf 'ignore me\\n'\n";
    let prog = stub_agent(dir.path(), script);
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "sf", None, RunOptions::default()).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    let state = RunState::fold(&events);
    let out = state.outputs.get("w").cloned().unwrap_or_default();
    assert!(
        out.contains("key") && out.contains("val"),
        "the status-file outputs object must become the node output, got: {out}"
    );
    assert!(
        !out.contains("ignore me"),
        "the textual reply must not survive when the status file carries outputs, got: {out}"
    );
}

// 4. Stale status-file removal (issue #70 item 3). Node `a` plants a stale
//    `w-1.json` (as a prior execution would leave on a resume/continue_from
//    re-run), then node `w` writes NO status file. The engine must drop the
//    stale file before spawning `w`, so `read_status_file` sees nothing and
//    `w`'s textual reply decides the node - the stale outputs must NOT leak into
//    the node output.
#[test]
fn stale_status_file_is_removed_before_spawn() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "sfstale", STALE_PLAYBOOK);
    // `a` plants a stale success+outputs file at `w`'s attempt-1 path; `w` writes
    // no status file and emits only a textual reply. Branch on the node id that
    // the engine encodes into the $APB_STATUS_FILE basename.
    let script = "#!/bin/sh\nd=$(dirname \"$APB_STATUS_FILE\")\n\
        case \"$APB_STATUS_FILE\" in\n\
        *a-1.json) printf '%s' '{\"status\":\"success\",\"outputs\":{\"stale\":\"leaked\"}}' \
        > \"$d/w-1.json\"; printf 'a done\\n' ;;\n\
        *) printf 'the real w output\\n' ;;\n\
        esac\n";
    let prog = stub_agent(dir.path(), script);
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "sfstale", None, RunOptions::default()).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "w's textual reply must decide the node, not the stale status file"
    );
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    let state = RunState::fold(&events);
    let out = state.outputs.get("w").cloned().unwrap_or_default();
    assert!(
        out.contains("the real w output"),
        "w's output must be its own textual reply, got: {out}"
    );
    assert!(
        !out.contains("leaked"),
        "the stale status-file outputs must not be adopted as w's output, got: {out}"
    );
}

// 5. The assembled prompt mentions `APB_STATUS_FILE` only when the node has a
//    success_check. Proven end to end: the stub dumps its argv (the prompt is
//    delivered via argv) to a file and the test asserts on that dump.
#[test]
fn prompt_mentions_status_file_only_with_success_check() {
    let _env = common::env_lock();

    // With a success_check: the note is present.
    let with = tempfile::tempdir().unwrap();
    seed(with.path(), "sfc", CHECK_PLAYBOOK);
    let with_dump = with.path().join("argv-dump.txt");
    let with_script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\necho done\n",
        with_dump.display()
    );
    let with_prog = stub_agent(with.path(), &with_script);
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &with_prog);
    }
    run(with.path(), "sfc", None, RunOptions::default()).unwrap();
    let with_prompt = fs::read_to_string(&with_dump).unwrap();

    // Without a success_check: the note is absent.
    let without = tempfile::tempdir().unwrap();
    seed(without.path(), "sf", PLAYBOOK);
    let without_dump = without.path().join("argv-dump.txt");
    let without_script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\necho done\n",
        without_dump.display()
    );
    let without_prog = stub_agent(without.path(), &without_script);
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &without_prog);
    }
    run(without.path(), "sf", None, RunOptions::default()).unwrap();
    let without_prompt = fs::read_to_string(&without_dump).unwrap();

    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }

    assert!(
        with_prompt.contains("APB_STATUS_FILE"),
        "a node with a success_check must have its prompt mention APB_STATUS_FILE"
    );
    assert!(
        !without_prompt.contains("APB_STATUS_FILE"),
        "a node without a success_check must not mention APB_STATUS_FILE in its prompt"
    );
}
