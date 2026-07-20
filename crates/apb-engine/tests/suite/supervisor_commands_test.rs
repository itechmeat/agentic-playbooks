use crate::common;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::control::Control;
use apb_engine::error::EngineError;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{
    RunMode, RunOptions, post_supervisor_command, resume, run, run_background,
};
use apb_engine::state::{RunState, RunStatus};

// The same trick as in supervised_drive_test.rs: tests in this file mutate the
// process-wide env var APB_AGENT_CMD, so such scenarios are serialized
// via a shared mutex over the whole set_var..run..remove_var span.

const POLL_DEADLINE: Duration = Duration::from_secs(5);
const POLL_STEP: Duration = Duration::from_millis(20);

/// Polls `f` until it returns Some(..) or the deadline elapses; otherwise panics
/// with a clear message instead of hanging.
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

fn wait_for_wake(run_dir: &Path) -> Event {
    poll_until("a WakeRaised event in events.jsonl", || {
        read_all(run_dir)
            .ok()?
            .into_iter()
            .find(|e| matches!(e.payload, EventPayload::WakeRaised { .. }))
    })
}

// Ensures the drive loop has finished at least one node before a proactively
// posted Pause/ContextAppend is caught by the top-of-loop scan - otherwise a
// Pause could race ahead of even the `start` node, leaving `RunState` with no
// `last_node` at all, and `resume(..., None)` would then fail with "nothing
// to resume from" rather than exercising the cursor-persistence fix this
// suite is testing.
fn wait_for_any_node_finished(run_dir: &Path) {
    poll_until("at least one NodeFinished event", || {
        read_all(run_dir).ok().and_then(|events| {
            events
                .iter()
                .any(|e| matches!(e.payload, EventPayload::NodeFinished { .. }))
                .then_some(())
        })
    })
}

/// Polls events.jsonl until the folded RunState shows the desired
/// terminal/paused status, and returns the event log at that point.
fn wait_for_run_status(run_dir: &Path, want: RunStatus) -> Vec<Event> {
    poll_until(&format!("run status {want:?}"), || {
        let events = read_all(run_dir).ok()?;
        let state = RunState::fold(&events);
        if state.run_status == want {
            Some(events)
        } else {
            None
        }
    })
}

fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut p = fs::metadata(path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(path, p).unwrap();
}

// Stub: fails on the first invocation, leaves a marker file, succeeds on all following ones.
// The same trick as in supervised_drive_test.rs::flaky_agent.
fn flaky_agent(dir: &Path) -> String {
    let marker = dir.join("cmd_flaky.marker");
    let path = dir.join("cmd_flaky.sh");
    let body = format!(
        "#!/bin/sh\nif [ -f '{m}' ]; then echo ok; exit 0; else touch '{m}'; echo firstfail 1>&2; exit 1; fi\n",
        m = marker.display()
    );
    fs::write(&path, body).unwrap();
    set_executable(&path);
    path.to_string_lossy().to_string()
}

fn seed(root: &Path, id: &str, yaml: &str) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    common::seed_main(root);
}

// A multi-step playbook without agent_task (two script nodes, each 0.3s):
// used to check a proactive Pause at a node boundary, WITHOUT wake -
// script nodes don't fail, so the Supervised branch with await_control is not
// engaged at all here, and the top-of-loop scan is the only path for applying commands.
fn seed_slow_multistep(root: &Path, id: &str) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    let yaml = format!(
        r#"
schema: 1
id: {id}
name: Slow Multistep
version: 1.0.0
nodes:
  - {{ id: start, type: start }}
  - {{ id: nap1, type: script, script: "scripts/nap.sh", runner: sh }}
  - {{ id: nap2, type: script, script: "scripts/nap.sh", runner: sh }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: nap1 }}
  - {{ from: nap1, to: nap2 }}
  - {{ from: nap2, to: done }}
"#,
        id = id
    );
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(vdir.join("scripts/nap.sh"), "#!/bin/sh\nsleep 0.3\n").unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    common::seed_main(root);
}

// The only unconditional edge `work -> done` - as in supervised_drive_test.rs,
// a node failure raises wake and waits for a command; no fallback edge is needed.
const WF_SUPERVISED: &str = r#"
schema: 1
id: supflow
name: Supervised
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

const WF_LINEAR: &str = r#"
schema: 1
id: lin
name: Linear
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "x" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: done }
"#;

// Scenario 1: post_supervisor_command rejects a traversal run_id and a
// nonexistent (but character-"safe") run_id as NotFound.
#[test]
fn post_supervisor_command_rejects_traversal_and_missing_run() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();

    let err = post_supervisor_command(dir.path(), "../../etc", Control::Pause).unwrap_err();
    match err {
        EngineError::NotFound(_) => {}
        other => panic!("expected NotFound for traversal run_id, got {other:?}"),
    }

    let err = post_supervisor_command(dir.path(), "does-not-exist", Control::Pause).unwrap_err();
    match err {
        EngineError::NotFound(_) => {}
        other => panic!("expected NotFound for missing run_id, got {other:?}"),
    }
}

// Scenario 2: after wake, the supervisor sends ContextAppend, then Retry.
// await_control must apply ContextAppend in place (not terminal,
// the wait continues) and return Retry to the calling code. The agent stub
// fails on the first invocation and succeeds on the second, so the run reaches
// Succeeded. Check: SupervisorAction{context_append} is logged BEFORE
// SupervisorAction{node_retry}, and the resulting context.md contains the note text.
#[test]
fn context_append_then_retry_recovers_after_wake() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "ctxsup", WF_SUPERVISED);

    let prog = flaky_agent(dir.path());
    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    let opts = RunOptions {
        mode: RunMode::Supervised,
        ..Default::default()
    };
    let run_id = run_background(dir.path(), "ctxsup", None, opts).unwrap();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    wait_for_wake(&run_dir);

    let note = "lint fails because of X";
    post_supervisor_command(
        dir.path(),
        &run_id,
        Control::ContextAppend { note: note.into() },
    )
    .unwrap();
    post_supervisor_command(
        dir.path(),
        &run_id,
        Control::Retry {
            node: "work".into(),
            prompt_override: None,
        },
    )
    .unwrap();

    let events = wait_for_run_status(&run_dir, RunStatus::Succeeded);
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    let ca_idx = events
        .iter()
        .position(|e| matches!(&e.payload, EventPayload::SupervisorAction { action, .. } if action == "context_append"))
        .expect("expected a SupervisorAction{action: context_append} event in the log");
    let retry_idx = events
        .iter()
        .position(|e| matches!(&e.payload, EventPayload::SupervisorAction { action, .. } if action == "node_retry"))
        .expect("expected a SupervisorAction{action: node_retry} event in the log");
    assert!(
        ca_idx < retry_idx,
        "context_append must be logged before node_retry, got ca={ca_idx} retry={retry_idx}"
    );

    let context_md = fs::read_to_string(run_dir.join("context.md")).unwrap();
    assert!(
        context_md.contains(note),
        "expected context.md to contain the supervisor note, got:\n{context_md}"
    );
}

// Scenario 3: a proactive Pause at a node boundary in Supervised, without a single wake -
// a multi-step playbook of script nodes, none of which fail. Right
// after start we send ContextAppend, then Pause; the top-of-loop scan must
// apply both at the nearest iteration boundary (no await_control is
// involved here, since there was no wake). The run ends up Paused rather than running
// to completion (nap2/done are not fully executed, no RunFinished appears).
#[test]
fn proactive_context_append_then_pause_in_supervised_without_wake() {
    let dir = tempfile::tempdir().unwrap();
    seed_slow_multistep(dir.path(), "propause");

    let opts = RunOptions {
        mode: RunMode::Supervised,
        ..Default::default()
    };
    let run_id = run_background(dir.path(), "propause", None, opts).unwrap();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    let note = "proactive note before pause";
    post_supervisor_command(
        dir.path(),
        &run_id,
        Control::ContextAppend { note: note.into() },
    )
    .unwrap();
    post_supervisor_command(dir.path(), &run_id, Control::Pause).unwrap();

    let events = wait_for_run_status(&run_dir, RunStatus::Paused);

    assert!(
        events.iter().any(|e| matches!(&e.payload, EventPayload::SupervisorAction { action, detail, .. } if action == "context_append" && detail == note)),
        "expected a proactively-applied SupervisorAction{{action: context_append}} carrying the note"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::RunFinished { .. })),
        "run must have paused, not run to completion"
    );

    let context_md = fs::read_to_string(run_dir.join("context.md")).unwrap();
    assert!(
        context_md.contains(note),
        "expected context.md to contain the proactively applied note, got:\n{context_md}"
    );
}

// Scenario 4a: Autonomous is unchanged - top-of-loop is shared between both modes,
// so an Abort set in advance (pre-seeded, before resume) still drives the
// autonomous run to RunStatus::Aborted. Unlike the like-named test in
// supervised_drive_test.rs, here the command is sent via post_supervisor_command
// (rather than post_control directly), to exercise the helper itself on a real run.
#[test]
fn autonomous_preseeded_abort_via_post_supervisor_command() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "lin", WF_LINEAR);

    let first = run(dir.path(), "lin", None, RunOptions::default()).unwrap();
    assert_eq!(first.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".apb/runs").join(&first.run_id);

    post_supervisor_command(
        dir.path(),
        &first.run_id,
        Control::Abort {
            reason: "preseeded via post_supervisor_command".into(),
        },
    )
    .unwrap();

    let res = resume(dir.path(), &first.run_id, Some("a")).unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Aborted,
        "pre-seeded Abort must end autonomous drive as Aborted"
    );

    let events = read_all(&run_dir).unwrap();
    assert!(
        events.iter().any(|e| matches!(&e.payload, EventPayload::RunAborted { reason } if reason.contains("preseeded"))),
        "expected a RunAborted event carrying the posted reason"
    );
}

// Scenario 4b: the same top-of-loop scan also works in Autonomous mode -
// a proactive ContextAppend + Pause at a node boundary of a multi-step autonomous
// run (without a single wake, since Autonomous never has one)
// drives it to RunStatus::Paused, and the note lands in context.md.
#[test]
fn autonomous_proactive_context_append_then_pause_at_node_boundary() {
    let dir = tempfile::tempdir().unwrap();
    seed_slow_multistep(dir.path(), "autopause");

    // RunOptions::default() -> RunMode::Autonomous.
    let run_id = run_background(dir.path(), "autopause", None, RunOptions::default()).unwrap();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    let note = "autonomous proactive note";
    post_supervisor_command(
        dir.path(),
        &run_id,
        Control::ContextAppend { note: note.into() },
    )
    .unwrap();
    post_supervisor_command(dir.path(), &run_id, Control::Pause).unwrap();

    let events = wait_for_run_status(&run_dir, RunStatus::Paused);
    assert!(
        events.iter().any(|e| matches!(&e.payload, EventPayload::SupervisorAction { action, detail, .. } if action == "context_append" && detail == note)),
        "expected the proactive context_append to be logged even in Autonomous mode"
    );

    let context_md = fs::read_to_string(run_dir.join("context.md")).unwrap();
    assert!(
        context_md.contains(note),
        "expected context.md to contain the note, got:\n{context_md}"
    );
}

// Scenario 5 (Task 4 completion-plan defect 1, ContextAppend half): `drive`
// used to reset its control cursor to `None` on every invocation and re-read
// control.jsonl from the very start - so an already-applied ContextAppend was
// re-applied by a resumed drive, a duplicate SupervisorAction{context_append}.
// Reuses the proactive-Pause mechanism from Scenario 3/4b (no wake needed):
// drive N applies the note then pauses at the nearest node boundary; drive
// N+1 (resume) must complete the run WITHOUT replaying that note.
#[test]
fn resume_does_not_replay_an_already_applied_context_append() {
    let dir = tempfile::tempdir().unwrap();
    seed_slow_multistep(dir.path(), "onceapp");

    let run_id = run_background(dir.path(), "onceapp", None, RunOptions::default()).unwrap();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    wait_for_any_node_finished(&run_dir);

    let note = "applied exactly once across drive N and N+1";
    post_supervisor_command(
        dir.path(),
        &run_id,
        Control::ContextAppend { note: note.into() },
    )
    .unwrap();
    post_supervisor_command(dir.path(), &run_id, Control::Pause).unwrap();

    // Drive N: applies the note, then pauses at the nearest node boundary.
    wait_for_run_status(&run_dir, RunStatus::Paused);

    // Drive N+1: resume must finish the remaining nodes without re-applying
    // the ContextAppend drive N already consumed.
    let res = resume(dir.path(), &run_id, None).unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "resume must complete the run rather than re-pause on the already-consumed Pause"
    );

    let events = read_all(&run_dir).unwrap();
    let applied = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::SupervisorAction { action, detail, .. } if action == "context_append" && detail == note))
        .count();
    assert_eq!(
        applied, 1,
        "expected exactly one context_append SupervisorAction across both drives, got {applied}"
    );
}

// Scenario 6 (Task 4 completion-plan defect 1, Pause half): the same
// reset-cursor bug also re-fires a Pause a prior drive already consumed -
// without a persisted cursor, the resumed drive N+1 sees the very same Pause
// entry again (its seq unchanged) and re-pauses immediately instead of
// finishing the run. Exactly one RunPaused event must exist once resume
// completes.
#[test]
fn resume_does_not_replay_a_consumed_pause() {
    let dir = tempfile::tempdir().unwrap();
    seed_slow_multistep(dir.path(), "oncepause");

    let run_id = run_background(dir.path(), "oncepause", None, RunOptions::default()).unwrap();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    wait_for_any_node_finished(&run_dir);

    post_supervisor_command(dir.path(), &run_id, Control::Pause).unwrap();
    wait_for_run_status(&run_dir, RunStatus::Paused);

    let res = resume(dir.path(), &run_id, None).unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "a Pause consumed by drive N must not re-pause drive N+1"
    );

    let events = read_all(&run_dir).unwrap();
    let paused = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::RunPaused { .. }))
        .count();
    assert_eq!(
        paused, 1,
        "expected exactly one RunPaused event (from drive N only), got {paused}"
    );
}
