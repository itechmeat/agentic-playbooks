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
/// Starts the playbook detached and pauses it (drain) until `wait_ms` from
/// now; returns once the run reads paused.
fn start_and_pause(root: &Path, wait_ms: u64) -> (String, RunGuard, u64) {
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

    let until = u64::try_from(apb_core::clock::now_ms()).unwrap() + wait_ms;
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
    let (run_id, _guard, until) = start_and_pause(root, 1_500);
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
    // A long `until`, so the lock is surely taken before it arrives.
    let (run_id, _guard, until) = start_and_pause(root, 5_000);
    let run_dir = root.join(".apb/runs").join(&run_id);
    let lock = root.join(".apb/workdir.lock");
    poll("the paused driver to release the lock", || {
        (!lock.is_file()).then_some(())
    });
    fs::write(&lock, std::process::id().to_string()).unwrap();
    assert!(
        apb_core::clock::now_ms() < u128::from(until),
        "the lock was taken only after `until`; the test proves nothing"
    );
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

// --- a follow-up waker ---

/// A playbook whose follow-up script prints the expected value; the run
/// succeeds and the follow-up is due 1 m later (shortened by
/// `APB_FOLLOW_UP_AFTER_MS`).
const FOLLOW_UP_PLAYBOOK: &str = r#"
schema: 2
id: wake
name: Wake
version: 1.0.0
follow_up:
  after: 1m
  check:
    id: deal_won
    description: the deal moved
    script: scripts/check.sh
    observe:
      - { id: stage, type: field, path: stage, expect: { equals: won } }
nodes:
  - { id: start, type: start }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: done }
"#;

fn seed_follow_up(root: &Path) {
    seed(root);
    // A follow-up script read runs in the execution root under the tree
    // guard: the root must be a git repository with a commit.
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
        vec!["commit", "-q", "--allow-empty", "-m", "seed"],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(&args)
                .output()
                .unwrap()
                .status
                .success(),
            "git {args:?} failed"
        );
    }
    let vdir = root.join(".apb/playbooks/wake/1.0.0");
    fs::write(vdir.join("playbook.yaml"), FOLLOW_UP_PLAYBOOK).unwrap();
    fs::write(
        vdir.join("scripts/check.sh"),
        "echo '{\"stage\": \"won\"}'\n",
    )
    .unwrap();
}

/// A succeeded run with a pending follow-up leaves `wakers/follow-up-<seq>`
/// held; with a short after and a short poll the waker journals
/// `follow_up_observed` and exits.
#[test]
fn a_follow_up_waker_is_armed_and_reads_in_time() {
    let tmp = tempfile::tempdir().unwrap();
    seed_follow_up(tmp.path());
    let out = crate::common::apb_std()
        .args(["run", "wake"])
        .env("APB_WAKER_POLL_MS", "50")
        .env("APB_FOLLOW_UP_AFTER_MS", "300")
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{stdout} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run_id = tmp
        .path()
        .join(".apb/runs")
        .read_dir()
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .file_name()
        .to_string_lossy()
        .into_owned();
    let dir = tmp.path().join(".apb/runs").join(&run_id);
    let events = read_all(&dir).unwrap();
    let finished_seq = events
        .iter()
        .find(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
        .unwrap()
        .seq;
    // The waker file the drive left behind.
    poll("the waker lease", || {
        dir.join(format!("wakers/follow-up-{finished_seq}"))
            .is_file()
            .then_some(())
    });
    // The waker reads the follow-up once its (shortened) time comes and
    // exits, leaving the observation in the journal.
    let started = std::time::Instant::now();
    poll("the follow-up observation", || {
        let events = read_all(&dir).ok()?;
        if events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::FollowUpObserved { status, .. } if status == "observed"))
        {
            return Some(());
        }
        if started.elapsed() > Duration::from_secs(20) {
            panic!("no observation: {events:?}");
        }
        None
    });
    let events = read_all(&dir).unwrap();
    assert!(
        events.iter().any(|e| matches!(&e.payload,
        EventPayload::FollowUpObserved { verdict, .. } if verdict == "met")),
        "{events:?}"
    );
}

// ---- The run deadline while a run is parked (routine operations) ----

/// The pids of the `apb __pause-waker` processes watching `run_id`'s
/// deadline.
fn deadline_wakers(run_id: &str) -> Vec<u32> {
    let out = std::process::Command::new("pgrep")
        .args(["-f", &format!("__pause-waker .*{run_id}.*--deadline")])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

/// Starts `wake` detached with `--deadline <deadline>` and pauses it as
/// the engine does (no `until`, a budget pause); returns once the drained
/// driver journaled the pause.
fn start_and_park(root: &Path, deadline: &str) -> (String, RunGuard) {
    fs::write(root.join("slow"), "").unwrap();
    let out = crate::common::apb_std()
        .args(["run", "wake", "--detach", "--deadline", deadline])
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
    let req = PauseRequest {
        reason: Some("budget (test)".into()),
        by: Some(PauseBy::Engine),
        ..PauseRequest::default()
    };
    request_pause(root, &run_id, &req).unwrap();
    fs::remove_file(root.join("slow")).unwrap();
    poll("the run to park", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| matches!(e.payload, EventPayload::RunPaused { .. }))
            .then_some(())
    });
    let _ = fs::remove_file(root.join("s_running"));
    (run_id, guard)
}

fn deadline_aborted(root: &Path, run_id: &str) -> bool {
    read_all(&root.join(".apb/runs").join(run_id))
        .unwrap()
        .iter()
        .any(|e| {
            matches!(&e.payload,
            EventPayload::RunAborted { reason, .. } if reason.starts_with("deadline_exceeded:"))
        })
}

/// A run the engine parked with no `until`: the driver leaves the deadline
/// waker behind, and the waker ends the run at its deadline.
#[test]
fn deadline_waker_ends_a_parked_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let (run_id, _guard) = start_and_park(root, "4s");
    poll("the run to end at its deadline", || {
        deadline_aborted(root, &run_id).then_some(())
    });
    assert!(
        !read_all(&root.join(".apb/runs").join(&run_id))
            .unwrap()
            .iter()
            .any(|e| matches!(e.payload, EventPayload::RunResumed { .. })),
        "the waker resumed the run"
    );
}

/// After a reboot no waker is left: `apb resume --all` ends a run past its
/// deadline (`deadline_stopped`) and gives a run with time left its
/// deadline waker back.
#[test]
fn resume_all_ends_an_expired_indefinite_pause() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let (short, _g1) = start_and_park(root, "4s");
    let (long, _g2) = start_and_park(root, "1h");
    for id in [&short, &long] {
        let pids = poll("the deadline waker", || {
            let p = deadline_wakers(id);
            (!p.is_empty()).then_some(p)
        });
        for pid in pids {
            crate::common::sig::kill_pid(pid);
        }
        poll("the waker to die", || {
            deadline_wakers(id).is_empty().then_some(())
        });
    }
    std::thread::sleep(Duration::from_secs(5));
    assert!(
        !deadline_aborted(root, &short),
        "a killed waker ended the run"
    );
    let out = crate::common::apb_std()
        .args(["resume", "--all"])
        .current_dir(root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout
            .lines()
            .any(|l| l.contains(short.as_str()) && l.contains("deadline_stopped")),
        "{stdout}"
    );
    assert!(deadline_aborted(root, &short));
    poll("the long run's waker back", || {
        (!deadline_wakers(&long).is_empty()).then_some(())
    });
    for pid in deadline_wakers(&long) {
        crate::common::sig::kill_pid(pid);
    }
}

/// A person's plain resume of a parked run past its deadline is refused
/// `deadline_passed` and leaves the run to a deadline waker it arms, which
/// ends the run (here its first waker was killed).
#[test]
fn a_refused_resume_leaves_the_run_to_its_deadline_waker() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let (run_id, _guard) = start_and_park(root, "3s");
    let pids = poll("the deadline waker", || {
        let p = deadline_wakers(&run_id);
        (!p.is_empty()).then_some(p)
    });
    for pid in pids {
        crate::common::sig::kill_pid(pid);
    }
    poll("the waker to die", || {
        deadline_wakers(&run_id).is_empty().then_some(())
    });
    std::thread::sleep(Duration::from_secs(4));
    let out = crate::common::apb_std()
        .args(["resume", &run_id])
        .env("APB_WAKER_POLL_MS", "50")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("deadline_passed"), "{stderr}");
    poll("the armed waker to end the run", || {
        deadline_aborted(root, &run_id).then_some(())
    });
}
