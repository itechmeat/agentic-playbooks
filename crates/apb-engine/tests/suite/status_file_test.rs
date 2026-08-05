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
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::control::{Control, post_control};
use apb_engine::error::EngineError;
use apb_engine::event::{EventPayload, WakeTrigger, read_all};
use apb_engine::scheduler::{RunOptions, RunResult, run};
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

// A node that REQUIRES a verdict (spec 2.2): an attempt whose process ends
// without a valid status file is interrupted, not succeeded. One retry is
// allowed, so a second attempt can finish the work.
const VERDICT_PLAYBOOK: &str = r#"
schema: 1
id: sfv
name: StatusFileVerdict
version: 1.0.0
defaults:
  profile: main
  max_retries: 1
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", require_verdict: true }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: w }
  - { from: w, to: ok, condition: { type: node_status, node: w, equals: success } }
  - { from: w, to: no, fallback: true }
"#;

// A `success_check` node with one retry: used for the Err-branch + recovered
// verdict + rejecting success_check shape (review decision-table row 8).
const CHECK_RETRY_PLAYBOOK: &str = r#"
schema: 1
id: sfcr
name: StatusFileCheckRetry
version: 1.0.0
defaults:
  profile: main
  max_retries: 1
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", success_check: { marker: "ALL DONE" } }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: w }
  - { from: w, to: ok, condition: { type: node_status, node: w, equals: success } }
  - { from: w, to: no, fallback: true }
"#;

// The requirement comes ONLY from `defaults.require_verdict`; the node itself
// says nothing (review M4: the defaults arm needs its own engine-level pin).
const DEFAULTS_VERDICT_PLAYBOOK: &str = r#"
schema: 1
id: sfd
name: StatusFileDefaults
version: 1.0.0
defaults:
  profile: main
  max_retries: 1
  require_verdict: true
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

// Same shape, but the requirement comes from `defaults.require_verdict` and the
// node has a 1 s timeout: used for the "timeout after a written verdict" case.
const TIMEOUT_VERDICT_PLAYBOOK: &str = r#"
schema: 1
id: sft
name: StatusFileTimeout
version: 1.0.0
defaults:
  profile: main
  require_verdict: true
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", timeout_seconds: 1 }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: w }
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

// --- The verdict outlives the process exit (#74 finding 1, spec 2.1) ---

/// Every `attempt_finished` status for a node, in journal order.
fn attempt_statuses(events: &[apb_engine::event::Event], node: &str) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptFinished {
                node: n, status, ..
            } if n == node => Some(status.clone()),
            _ => None,
        })
        .collect()
}

/// Whether an anomaly wake for `node` was journaled whose detail matches.
fn has_anomaly(events: &[apb_engine::event::Event], node: &str, needle: &str) -> bool {
    events.iter().any(|e| {
        matches!(
            &e.payload,
            EventPayload::WakeRaised { trigger: WakeTrigger::Anomaly, node: n, detail }
                if n == node && detail.contains(needle)
        )
    })
}

// 6. A tail crash after the deliverable was already written: the agent writes a
//    success verdict with outputs and THEN exits non-zero. The verdict is the
//    explicit completion signal, so the attempt succeeds with those outputs and
//    the abnormal exit is journaled as an anomaly instead of throwing the
//    deliverable away.
#[test]
fn status_file_success_survives_a_nonzero_exit() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "sf", PLAYBOOK);
    let script = "#!/bin/sh\nprintf '%s' '{\"status\":\"success\",\"outputs\":{\"key\":\"val\"}}' \
        > \"$APB_STATUS_FILE\"\nprintf 'work is done\\n'\necho 'tail crash' 1>&2\nexit 1\n";
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
        "a written success verdict must outlive a non-zero exit"
    );
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    let out = RunState::fold(&events)
        .outputs
        .get("w")
        .cloned()
        .unwrap_or_default();
    assert!(
        out.contains("key") && out.contains("val"),
        "the status-file outputs must become the node output despite the exit, got: {out}"
    );
    assert_eq!(
        attempt_statuses(&events, "w"),
        vec!["succeeded".to_string()],
        "the attempt must be journaled succeeded"
    );
    assert!(
        has_anomaly(&events, "w", "verdict"),
        "the tail exit must be journaled as an anomaly naming the written verdict"
    );
}

// 7. The same shape with a FAILURE verdict: the attempt stays failed, but the
//    agent's own outputs are preserved as the attempt output instead of the raw
//    CLI error text.
#[test]
fn status_file_failure_on_a_nonzero_exit_preserves_agent_outputs() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "sf", PLAYBOOK);
    let script = "#!/bin/sh\nprintf '%s' '{\"status\":\"failure\",\"outputs\":{\"reason\":\"schema drift\"}}' \
        > \"$APB_STATUS_FILE\"\necho 'agent internal error' 1>&2\nexit 2\n";
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
        "a failure verdict keeps the attempt failed"
    );
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    let out = RunState::fold(&events)
        .outputs
        .get("w")
        .cloned()
        .unwrap_or_default();
    assert!(
        out.contains("schema drift"),
        "the agent's own outputs must survive as the failed node's output, got: {out}"
    );
    assert!(
        !out.contains("agent exited with"),
        "the raw CLI error text must not replace the agent's outputs, got: {out}"
    );
    assert_eq!(
        attempt_statuses(&events, "w"),
        vec!["failed".to_string()],
        "a failure verdict must not be upgraded"
    );
}

// 8. A MALFORMED status file on a non-zero exit changes nothing: there is no
//    valid verdict, so the attempt keeps today's failure semantics.
#[test]
fn malformed_status_file_on_a_nonzero_exit_stays_failed() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "sf", PLAYBOOK);
    let script = "#!/bin/sh\nprintf 'not json {' > \"$APB_STATUS_FILE\"\n\
        echo 'boom' 1>&2\nexit 1\n";
    let prog = stub_agent(dir.path(), script);
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "sf", None, RunOptions::default()).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    assert_eq!(res.outcome, RunStatus::Failed);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(attempt_statuses(&events, "w"), vec!["failed".to_string()]);
    assert!(
        !has_anomaly(&events, "w", "verdict"),
        "a malformed file is no verdict, so no verdict anomaly may be journaled"
    );
}

// 9. A timeout after a written success verdict: the deadline kill is transport
//    noise once the verdict exists, so the attempt succeeds with its outputs.
#[test]
fn timeout_after_a_written_success_verdict_succeeds() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "sft", TIMEOUT_VERDICT_PLAYBOOK);
    let script = "#!/bin/sh\nprintf '%s' '{\"status\":\"success\",\"outputs\":{\"key\":\"val\"}}' \
        > \"$APB_STATUS_FILE\"\nsleep 5\n";
    let prog = stub_agent(dir.path(), script);
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let started = std::time::Instant::now();
    let res = run(dir.path(), "sft", None, RunOptions::default()).unwrap();
    let elapsed = started.elapsed();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "a verdict written before the deadline must decide the attempt"
    );
    assert!(
        elapsed.as_millis() < 4000,
        "the agent must still be killed on timeout: took {elapsed:?}"
    );
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    let out = RunState::fold(&events)
        .outputs
        .get("w")
        .cloned()
        .unwrap_or_default();
    assert!(out.contains("val"), "outputs must survive, got: {out}");
}

// --- require_verdict (#71 items 1, 3, 5-context; spec 2.2) ---

// 10. A require_verdict node whose agent prints a mid-work message and exits 0
//     without writing a verdict: the attempt is INTERRUPTED (not succeeded), a
//     retry fires carrying the interruption note in its prompt, and the second
//     attempt's verdict finishes the node.
#[test]
fn require_verdict_turns_a_missing_verdict_into_an_interrupted_retry() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "sfv", VERDICT_PLAYBOOK);
    let dump = dir.path().join("prompts");
    fs::create_dir_all(&dump).unwrap();
    let marker = dir.path().join("first.marker");
    // One prompt dump per attempt, keyed by the attempt's status-file basename
    // (`w-1`, `w-2`), so the note can be asserted per attempt.
    let script = format!(
        "#!/bin/sh\nb=$(basename \"$APB_STATUS_FILE\" .json)\n\
        printf '%s\\n' \"$@\" > \"{d}/$b.txt\"\n\
        if [ -f '{m}' ]; then\n\
        printf '%s' '{{\"status\":\"success\",\"outputs\":{{\"done\":\"yes\"}}}}' > \"$APB_STATUS_FILE\"\n\
        printf 'finished the work\\n'\n\
        else\n\
        touch '{m}'\n\
        printf 'still working on it, will continue after the wait\\n'\n\
        fi\nexit 0\n",
        d = dump.display(),
        m = marker.display()
    );
    let prog = stub_agent(dir.path(), &script);
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "sfv", None, RunOptions::default()).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "the retry after an interrupted attempt must finish the node"
    );
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        attempt_statuses(&events, "w"),
        vec!["interrupted".to_string(), "succeeded".to_string()],
        "the verdict-less attempt must be journaled interrupted, the second succeeded"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::RetryStarted { node, .. } if node == "w")),
        "an interrupted attempt must consume a retry"
    );
    let state = RunState::fold(&events);
    let out = state.outputs.get("w").cloned().unwrap_or_default();
    assert!(
        out.contains("done") && out.contains("yes"),
        "the second attempt's verdict outputs must become the node output, got: {out}"
    );
    let first = fs::read_to_string(dump.join("w-1.txt")).unwrap();
    let second = fs::read_to_string(dump.join("w-2.txt")).unwrap();
    assert!(
        second.contains("cut off mid-work"),
        "the retry prompt must carry the interruption note, got: {second}"
    );
    assert!(
        !first.contains("cut off mid-work"),
        "the first attempt has nothing to recover, so it must carry no interruption note"
    );
    assert!(
        first.contains("APB_STATUS_FILE"),
        "a require_verdict node must always be told the status-file contract"
    );
    // The partial mid-work text is preserved on the interrupted attempt event so
    // the work is observable rather than silently dropped.
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::AttemptFinished { node, status, partial_output: Some(p), .. }
                if node == "w" && status == "interrupted" && p.contains("still working on it")
        )),
        "the interrupted attempt must preserve its partial output"
    );
}

// 11. Without require_verdict nothing changes: an exit 0 with no verdict and no
//     report block stays a success carrying the agent's text (the documented
//     default every existing playbook relies on).
#[test]
fn without_require_verdict_a_missing_verdict_stays_succeeded() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "sf", PLAYBOOK);
    let script = "#!/bin/sh\nprintf 'a plain reply with no verdict\\n'\n";
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
        "the default text-report contract must be preserved byte for byte"
    );
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        attempt_statuses(&events, "w"),
        vec!["succeeded".to_string()]
    );
    let out = RunState::fold(&events)
        .outputs
        .get("w")
        .cloned()
        .unwrap_or_default();
    assert!(out.contains("a plain reply with no verdict"), "got: {out}");
}

// --- Fix round 1: review decision-table rows 8, 11, 19 and the defaults arm ---

const POLL_DEADLINE: Duration = Duration::from_secs(10);
const POLL_STEP: Duration = Duration::from_millis(20);

fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        if start.elapsed() > POLL_DEADLINE {
            panic!("timed out after {POLL_DEADLINE:?} waiting for: {what}");
        }
        std::thread::sleep(POLL_STEP);
    }
}

fn find_run_id(root: &Path, prefix: &str) -> String {
    poll_until(&format!("a run dir with prefix `{prefix}`"), || {
        let runs_dir = root.join(".apb/runs");
        if !runs_dir.is_dir() {
            return None;
        }
        fs::read_dir(&runs_dir)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .find(|n| n.starts_with(prefix))
    })
}

// 12. Row 8: the process exits non-zero AFTER writing a success verdict, and the
//     node's success_check rejects the recovered output. The verdict does not
//     bypass the gate: the attempt is failed with the recovered output preserved
//     as `rejected_output`, a retry is consumed, and the tail-exit anomaly is
//     still journaled.
#[test]
fn a_rejected_recovered_verdict_consumes_a_retry_and_exposes_rejected_output() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "sfcr", CHECK_RETRY_PLAYBOOK);
    // The recovered output is the verdict's outputs object, which does NOT carry
    // the completion marker the node demands.
    let script = "#!/bin/sh\nprintf '%s' '{\"status\":\"success\",\"outputs\":{\"state\":\"half way\"}}' \
        > \"$APB_STATUS_FILE\"\necho 'died in the tail' 1>&2\nexit 1\n";
    let prog = stub_agent(dir.path(), script);
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "sfcr", None, RunOptions::default()).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    assert_eq!(
        res.outcome,
        RunStatus::Failed,
        "a recovered verdict the success_check rejects must not succeed the node"
    );
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        attempt_statuses(&events, "w"),
        vec!["failed".to_string(), "failed".to_string()],
        "both attempts must be journaled failed by the gate"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::RetryStarted { node, .. } if node == "w")),
        "a gate rejection on the Err branch must still consume a retry"
    );
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::AttemptFinished { node, rejected_output: Some(r), .. }
                if node == "w" && r.contains("half way")
        )),
        "the recovered verdict output must be preserved as rejected_output"
    );
    assert!(
        has_anomaly(&events, "w", "verdict"),
        "the tail exit must still be journaled as an anomaly even when the gate rejects"
    );
    let state = RunState::fold(&events);
    assert!(
        state
            .rejected_outputs
            .get("w")
            .is_some_and(|r| r.contains("half way")),
        "the rejected output must reach downstream templates"
    );
}

// 13. Row 11: a require_verdict node whose process exits NON-ZERO without a
//     verdict. The exit is classified interrupted (not merely failed), the
//     adapter detail is preserved as the partial output, and the retry is
//     consumed exactly as on the exit-0 shape.
#[test]
fn require_verdict_on_a_nonzero_exit_is_interrupted_and_consumes_a_retry() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "sfv", VERDICT_PLAYBOOK);
    let script = "#!/bin/sh\necho 'got half way and blew up' 1>&2\nexit 1\n";
    let prog = stub_agent(dir.path(), script);
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "sfv", None, RunOptions::default()).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    assert_eq!(res.outcome, RunStatus::Failed);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        attempt_statuses(&events, "w"),
        vec!["interrupted".to_string(), "interrupted".to_string()],
        "a verdict-less non-zero exit must be interrupted, not plain failed"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::RetryStarted { node, .. } if node == "w")),
        "the interrupted attempt must consume a retry on the Err branch too"
    );
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::AttemptFinished { node, status, partial_output: Some(p), .. }
                if node == "w" && status == "interrupted" && p.contains("got half way and blew up")
        )),
        "the adapter's failure detail must be preserved as the partial output"
    );
}

// 14. The requirement can come from `defaults.require_verdict` alone: a node
//     that says nothing still has its verdict-less exit classified interrupted,
//     and its prompt still carries the status-file contract.
#[test]
fn defaults_require_verdict_interrupts_a_verdictless_attempt() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "sfd", DEFAULTS_VERDICT_PLAYBOOK);
    let dump = dir.path().join("argv-dump.txt");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\nprintf 'mid-work, nothing recorded\\n'\n",
        dump.display()
    );
    let prog = stub_agent(dir.path(), &script);
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "sfd", None, RunOptions::default()).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    assert_eq!(
        res.outcome,
        RunStatus::Failed,
        "defaults.require_verdict must apply to a node that declares nothing"
    );
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        attempt_statuses(&events, "w"),
        vec!["interrupted".to_string(), "interrupted".to_string()],
        "the defaults arm must reach the same interruption path as the node arm"
    );
    let prompt = fs::read_to_string(&dump).unwrap();
    assert!(
        prompt.contains("APB_STATUS_FILE") && prompt.contains("REQUIRES the verdict"),
        "a node required to record a verdict via defaults must be told the contract"
    );
}

// 15. Row 19 (owner decision, spec 2.2 addendum): a supervisor-issued interrupt
//     is a CONTROL decision, not transport noise. An agent that records a
//     success verdict and then wedges must still see its attempt journaled
//     failed when the supervisor interrupts it - otherwise
//     `supervisor_interrupt_attempt` silently loses its contract. The verdict is
//     not thrown away silently: it rides the journaled anomaly so the supervisor
//     can accept the work explicitly.
#[test]
fn a_supervisor_interrupt_beats_a_written_success_verdict() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "sf", PLAYBOOK);
    // Record the verdict, announce it via a marker, then wedge for far longer
    // than this test is willing to wait.
    let marker = dir.path().join("verdict-written.marker");
    let script = format!(
        "#!/bin/sh\nprintf '%s' '{{\"status\":\"success\",\"outputs\":{{\"key\":\"val\"}}}}' \
        > \"$APB_STATUS_FILE\"\ntouch '{m}'\nsleep 30\n",
        m = marker.display()
    );
    let prog = stub_agent(dir.path(), &script);
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    let root = dir.path().to_path_buf();
    let (tx, rx) = mpsc::channel::<Result<RunResult, EngineError>>();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "sf", None, RunOptions::default()));
    });
    let run_id = find_run_id(dir.path(), "sf-");
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    poll_until("the stub agent to record its verdict", || {
        marker.is_file().then_some(())
    });
    post_control(
        &run_dir,
        Control::Interrupt {
            reason: "attempt is wedged after writing its verdict".into(),
        },
    )
    .unwrap();

    let res = rx
        .recv_timeout(POLL_DEADLINE)
        .expect("the drive must return well under the agent's 30s wedge")
        .unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    assert_eq!(
        res.outcome,
        RunStatus::Failed,
        "a supervisor interrupt must not be overridden by a written verdict"
    );
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        attempt_statuses(&events, "w"),
        vec!["failed".to_string()],
        "the interrupted attempt must stay failed, as before the verdict-over-exit change"
    );
    assert!(
        has_anomaly(&events, "w", "supervisor interrupt"),
        "the anomaly must name the interrupt that overruled the verdict"
    );
    assert!(
        has_anomaly(&events, "w", "val"),
        "the anomaly must carry the verdict text so the supervisor can see the work existed"
    );
}
