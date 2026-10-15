//! Issue #200: a pause with `until` resumes on its own, through the real
//! `apb` binary. The driver that applies the pause exits and leaves the
//! hidden `apb __pause-waker` behind; at `until` the waker resumes the run in
//! a detached driver whose `run_resumed` names `engine`. Only this crate can
//! reach the shipped binary the waker re-execs.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::pause::{PauseBy, PauseOutcome, PauseRequest, request_pause};

use crate::common::RunGuard;

const DEADLINE: Duration = Duration::from_secs(60);

fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(start.elapsed() < DEADLINE, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

const PLAYBOOK: &str = r#"
schema: 1
id: wake
name: Wake
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: s, type: script, script: "scripts/s.sh", runner: sh }
  - { id: t, type: script, script: "scripts/t.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: s }
  - { from: s, to: t }
  - { from: t, to: done }
"#;

fn seed(root: &Path) {
    crate::common::apb_std()
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    let vdir = root.join(".apb/playbooks/wake/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    let d = root.display();
    fs::write(
        vdir.join("scripts/s.sh"),
        format!("touch '{d}/s_running'\nwhile [ -f '{d}/slow' ]; do sleep 0.05; done\necho s\n"),
    )
    .unwrap();
    fs::write(vdir.join("scripts/t.sh"), "echo t\n").unwrap();
    fs::write(root.join(".apb/playbooks/wake/current"), "1.0.0").unwrap();
}

fn finishes(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::NodeFinished { node: n, .. } if n == node))
        .count()
}

/// Starts the run, pauses it with an `until` 1.5 s ahead and waits until
/// the drained driver journals the pause. Returns the run id, its stop guard
/// and `until`.
fn start_and_pause(root: &Path) -> (String, RunGuard, u64) {
    seed(root);
    fs::write(root.join("slow"), "").unwrap();
    let out = crate::common::apb_std()
        .args(["run", "wake", "--detach"])
        .env("APB_WAKER_POLL_MS", "50")
        .current_dir(root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{stdout} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run_id = stdout
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .expect("run id")
        .trim()
        .to_string();
    let guard = RunGuard::new(root, &run_id);
    let run_dir = root.join(".apb/runs").join(&run_id);
    poll("the script to start", || {
        root.join("s_running").is_file().then_some(())
    });

    let until = u64::try_from(apb_core::clock::now_ms()).unwrap() + 1_500;
    let req = PauseRequest {
        reason: Some("lunch".into()),
        by: Some(PauseBy::Operator),
        until: Some(until),
        ..PauseRequest::default()
    };
    let posted = request_pause(root, &run_id, &req).unwrap();
    assert!(matches!(posted, PauseOutcome::Posted { .. }), "{posted:?}");
    // Drain: the running script finishes, then the driver pauses and exits.
    fs::remove_file(root.join("slow")).unwrap();
    poll("the run to pause", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| matches!(e.payload, EventPayload::RunPaused { until: Some(_), .. }))
            .then_some(())
    });
    (run_id, guard, until)
}

#[test]
fn a_pause_with_until_is_resumed_by_its_waker() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (run_id, _guard, until) = start_and_pause(root);
    let run_dir = root.join(".apb/runs").join(&run_id);
    let events = poll("the waker to resume the run to its finish", || {
        let ev = read_all(&run_dir).ok()?;
        ev.iter()
            .any(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
            .then_some(ev)
    });
    let resumed = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunResumed { by, .. } => Some(*by),
        _ => None,
    });
    assert_eq!(resumed, Some(Some(PauseBy::Engine)), "resumed by the waker");
    let resumed_at = events
        .iter()
        .find(|e| matches!(e.payload, EventPayload::RunResumed { .. }))
        .unwrap()
        .ts;
    assert!(resumed_at >= u128::from(until), "not before `until`");
    assert!(matches!(
        events.last().map(|e| &e.payload),
        Some(EventPayload::RunFinished { outcome }) if outcome == "succeeded"
    ));
    assert_eq!(finishes(&events, "s"), 1);
    assert_eq!(finishes(&events, "t"), 1);
}

/// A waker whose `until` arrives while another live run holds the busy lock
/// of the tree keeps the run paused and waits; once the lock frees it
/// resumes the run, which then finishes.
#[test]
fn a_waker_waits_out_a_busy_workdir() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (run_id, _guard, until) = start_and_pause(root);
    let run_dir = root.join(".apb/runs").join(&run_id);
    let lock = root.join(".apb/workdir.lock");
    poll("the paused driver to release the lock", || {
        (!lock.is_file()).then_some(())
    });
    fs::write(&lock, std::process::id().to_string()).unwrap();
    poll("`until` to pass", || {
        (apb_core::clock::now_ms() > u128::from(until) + 1_000).then_some(())
    });
    let events = read_all(&run_dir).unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::RunResumed { .. })),
        "no resume while the lock is held"
    );
    fs::remove_file(&lock).unwrap();
    let events = poll("the waker to resume the run to its finish", || {
        let ev = read_all(&run_dir).ok()?;
        ev.iter()
            .any(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
            .then_some(ev)
    });
    assert!(matches!(
        events.last().map(|e| &e.payload),
        Some(EventPayload::RunFinished { outcome }) if outcome == "succeeded"
    ));
    assert_eq!(finishes(&events, "s"), 1);
    assert_eq!(finishes(&events, "t"), 1);
}
