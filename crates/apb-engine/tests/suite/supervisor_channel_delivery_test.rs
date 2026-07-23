//! Issue #45 findings 2 and 8: supervisor channel delivery.
//!
//! - Finding 2: applied `context_append` notes land in subsequent agent attempt
//!   prompts (even when the template never references `{{run.context}}`).
//! - Finding 8: a child-run wake is mirrored onto the parent run so
//!   `wait_wake` / `supervisor_wait_event` on the parent observes it.

use crate::common;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::control::Control;
use apb_engine::event::{EventLog, EventPayload, WakeTrigger, raise_wake, read_all};
use apb_engine::inspect::wait_wake;
use apb_engine::run_config::{RunConfig, write_run_config};
use apb_engine::scheduler::{RunMode, RunOptions, post_supervisor_command, run_background};
use apb_engine::state::{RunState, RunStatus};

const POLL_DEADLINE: Duration = Duration::from_secs(8);
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

fn set_executable(path: &Path) {
    let mut p = fs::metadata(path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(path, p).unwrap();
}

fn wait_for_run_status(run_dir: &Path, want: RunStatus) {
    poll_until(&format!("run status {want:?}"), || {
        let events = read_all(run_dir).ok()?;
        let state = RunState::fold(&events);
        (state.run_status == want).then_some(())
    });
}

/// Agent stub that dumps every argv element (including the prompt) then succeeds.
fn dump_agent(dir: &Path, dump: &Path) -> String {
    let path = dir.join("cmd_dump.sh");
    let body = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{d}'\necho ok\n",
        d = dump.display()
    );
    common::write_sync(&path, &body);
    set_executable(&path);
    path.to_string_lossy().to_string()
}

/// Flaky agent that fails once, dumps argv on every attempt, then succeeds.
fn flaky_dump_agent(dir: &Path, dump: &Path) -> String {
    let marker = dir.join("cmd_flaky.marker");
    let path = dir.join("cmd_flaky_dump.sh");
    let body = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{d}'\nif [ -f '{m}' ]; then echo ok; exit 0; else touch '{m}'; echo firstfail 1>&2; exit 1; fi\n",
        d = dump.display(),
        m = marker.display()
    );
    common::write_sync(&path, &body);
    set_executable(&path);
    path.to_string_lossy().to_string()
}

fn seed_playbook(root: &Path, id: &str, yaml: &str) {
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

// Finding 2: a context_append applied after the first failed attempt is
// injected into the retry attempt's prompt under a "Supervisor notes:" block,
// even though the node template is just "do" (no `{{run.context}}`).
#[test]
fn context_note_appears_in_retry_agent_prompt() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let dump = dir.path().join("agent_dump.txt");

    let yaml = r#"
schema: 2
id: notedel
name: Note Delivery
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do the work" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;
    seed_playbook(dir.path(), "notedel", yaml);

    let prog = flaky_dump_agent(dir.path(), &dump);
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    let opts = RunOptions {
        mode: RunMode::Supervised,
        ..Default::default()
    };
    let run_id = run_background(dir.path(), "notedel", None, opts).unwrap();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    poll_until("a WakeRaised event", || {
        read_all(&run_dir)
            .ok()?
            .into_iter()
            .find(|e| matches!(e.payload, EventPayload::WakeRaised { .. }))
            .map(|_| ())
    });

    let note = "review produced four blockers: a, b, c, d";
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

    wait_for_run_status(&run_dir, RunStatus::Succeeded);
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    let dumped = fs::read_to_string(&dump).expect("agent dump");
    assert!(
        dumped.contains("Supervisor notes:"),
        "retry prompt must carry the delimited supervisor notes section, got:\n{dumped}"
    );
    assert!(
        dumped.contains(note),
        "retry prompt must include the applied note text, got:\n{dumped}"
    );
    // Manifest stays free of free-form note bodies.
    let manifest = fs::read_to_string(run_dir.join("manifest.yaml")).unwrap_or_default();
    assert!(
        !manifest.contains(note),
        "notes must not be persisted into the immutable manifest"
    );
}

// Finding 2: a note applied after node A finishes is present when later agent
// node B starts (proactive top-of-loop apply between nodes).
#[test]
fn context_note_after_node_a_appears_in_later_agent_node_b() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let dump = dir.path().join("agent_dump_b.txt");

    init_project(dir.path()).unwrap();
    let id = "notelater";
    let vdir = dir.path().join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    let yaml = format!(
        r#"
schema: 2
id: {id}
name: Note Later
version: 1.0.0
defaults:
  profile: main
nodes:
  - {{ id: start, type: start }}
  - {{ id: a, type: script, script: "scripts/nap.sh", runner: sh }}
  - {{ id: b, type: agent_task, prompt: "later work" }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: a }}
  - {{ from: a, to: b }}
  - {{ from: b, to: done }}
"#
    );
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(vdir.join("scripts/nap.sh"), "#!/bin/sh\nsleep 0.4\n").unwrap();
    fs::write(
        dir.path().join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    common::seed_main(dir.path());

    let prog = dump_agent(dir.path(), &dump);
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    let opts = RunOptions {
        mode: RunMode::Autonomous,
        ..Default::default()
    };
    let run_id = run_background(dir.path(), id, None, opts).unwrap();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    // Post the note while A is still napping so the top-of-loop scan applies it
    // before B's agent attempt starts.
    let note = "fix-round: re-check the review blockers";
    post_supervisor_command(
        dir.path(),
        &run_id,
        Control::ContextAppend { note: note.into() },
    )
    .unwrap();

    wait_for_run_status(&run_dir, RunStatus::Succeeded);
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    let dumped = fs::read_to_string(&dump).expect("agent dump for node b");
    assert!(
        dumped.contains("Supervisor notes:") && dumped.contains(note),
        "later agent node B must see notes applied after A, got:\n{dumped}"
    );
}

// Finding 8: a wake raised on a nested child run is mirrored into the parent
// event log and surfaces through wait_wake on the parent run id.
#[test]
fn child_run_wake_surfaces_on_parent_wait_wake() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let parent_id = "parent-run";
    let child_id = "child-run";
    let parent_dir = root.join(".apb/runs").join(parent_id);
    let child_dir = root.join(".apb/runs").join(child_id);
    fs::create_dir_all(&parent_dir).unwrap();
    fs::create_dir_all(&child_dir).unwrap();

    let mut parent_log = EventLog::create(&parent_dir).unwrap();
    parent_log
        .append(EventPayload::RunStarted {
            playbook: "parent".into(),
            version: "1.0.0".into(),
        })
        .unwrap();
    parent_log
        .append(EventPayload::ChildRunStarted {
            node_id: "brainstorm".into(),
            run_id: child_id.into(),
        })
        .unwrap();
    // Parent keeps its EventLog open (as drive does during nested child work).
    // After the child mirrors a wake, resync before any further parent append.

    write_run_config(
        &child_dir,
        &RunConfig {
            parent_run: Some(parent_id.into()),
            ..Default::default()
        },
    )
    .unwrap();
    let mut child_log = EventLog::create(&child_dir).unwrap();
    child_log
        .append(EventPayload::RunStarted {
            playbook: "child".into(),
            version: "1.0.0".into(),
        })
        .unwrap();

    raise_wake(
        &child_dir,
        &mut child_log,
        WakeTrigger::Anomaly,
        "ask",
        "interactive question",
    )
    .unwrap();

    parent_log.resync_seq().unwrap();

    let wake = wait_wake(root, parent_id, None, Duration::from_secs(1))
        .unwrap()
        .expect("parent wait_wake must observe the mirrored child wake");
    assert_eq!(wake.node, "brainstorm");
    assert!(
        matches!(wake.trigger, WakeTrigger::Anomaly),
        "trigger should match the child wake"
    );
    assert!(
        wake.detail.contains("child_run=child-run")
            && wake.detail.contains("child_node=ask")
            && wake.detail.contains("interactive question"),
        "detail must identify the child run and node, got: {}",
        wake.detail
    );

    // after_seq past the mirrored wake returns None until a new wake appears.
    let none = wait_wake(root, parent_id, Some(wake.seq), Duration::from_millis(200)).unwrap();
    assert!(none.is_none());
}
