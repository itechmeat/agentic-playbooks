//! The follow-up check of an ended run: the state fold, `observe_due` over
//! real runs with a script source (no sleeping: `now_ms` is passed in), a
//! connector stand-in, the lease, the origin stamp, and `apb resume --all`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

const DEADLINE: Duration = Duration::from_secs(60);

/// One script node then a finish; the follow-up reads a script that
/// prints `won`.
const PLAYBOOK: &str = r#"
schema: 2
id: fu
name: fu
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
  - { id: work, type: prompt, prompt: "Deliver." }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

fn git(root: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

fn seed(root: &Path) {
    // The config sandbox has no installation key until something makes
    // one; without it `stamp` writes no origin stamp and the run would
    // read as imported.
    apb_core::run_origin::ensure_key().unwrap();
    // A follow-up script read runs in the execution root under the tree
    // guard: the root must be a git repository with a commit.
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "t"]);
    git(root, &["commit", "-q", "--allow-empty", "-m", "seed"]);
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/fu/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(
        vdir.join("scripts/check.sh"),
        "#!/bin/sh\necho '{\"stage\": \"won\"}'\n",
    )
    .unwrap();
    fs::write(root.join(".apb/playbooks/fu/current"), "1.0.0").unwrap();
}

fn seed_with_script(root: &Path, body: &str) {
    seed(root);
    fs::write(root.join(".apb/playbooks/fu/1.0.0/scripts/check.sh"), body).unwrap();
}

fn start(root: &Path) -> mpsc::Receiver<(RunStatus, PathBuf)> {
    let (tx, rx) = mpsc::channel();
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        let r = run(&root, "fu", None, RunOptions::default()).unwrap();
        let _ = tx.send((r.outcome, root.join(".apb/runs").join(&r.run_id)));
    });
    rx
}

/// A finished successful run of `fu`, its journal left as the drive wrote
/// it.
fn finished_run(root: &Path) -> PathBuf {
    let rx = start(root);
    let (status, rd) = rx.recv_timeout(DEADLINE).unwrap();
    assert_eq!(status, RunStatus::Succeeded, "{rd:?}");
    rd
}

fn observations(events: &[Event]) -> Option<(&str, &str, &str)> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::FollowUpObserved {
            check,
            status,
            verdict,
            ..
        } => Some((check.as_str(), status.as_str(), verdict.as_str())),
        _ => None,
    })
}

/// A matching script source journals `observed` and `met`.
#[test]
fn a_matching_script_journals_observed_and_met() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let rd = finished_run(dir.path());
    // Due at finish + 1m; observe 2m later.
    let finished = read_all(&rd)
        .unwrap()
        .into_iter()
        .find(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
        .unwrap();
    let due = u64::try_from(finished.ts).unwrap() + 60_000;
    let event = apb_engine::follow_up::read::observe_due(
        dir.path(),
        rd.file_name().unwrap().to_str().unwrap(),
        due + 60_000,
    )
    .unwrap()
    .expect("the follow-up was read");
    match &event.payload {
        EventPayload::FollowUpObserved {
            check,
            status,
            verdict,
            ..
        } => {
            assert_eq!(check, "deal_won");
            assert_eq!(status, "observed");
            assert_eq!(verdict, "met");
        }
        other => panic!("expected FollowUpObserved, got {other:?}"),
    }
}

/// A non-matching source: `observed` and `not_met`; a failing one:
/// `unknown`.
#[test]
fn not_met_and_unknown() {
    let dir = tempfile::tempdir().unwrap();
    seed_with_script(dir.path(), "#!/bin/sh\necho '{\"stage\": \"lost\"}'\n");
    let rd = finished_run(dir.path());
    let finished = read_all(&rd).unwrap();
    let due = u64::try_from(
        finished
            .iter()
            .find(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
            .unwrap()
            .ts,
    )
    .unwrap()
        + 120_000;
    let event = apb_engine::follow_up::read::observe_due(
        dir.path(),
        rd.file_name().unwrap().to_str().unwrap(),
        due,
    )
    .unwrap()
    .unwrap();
    match &event.payload {
        EventPayload::FollowUpObserved {
            status, verdict, ..
        } => {
            assert_eq!((status.as_str(), verdict.as_str()), ("observed", "not_met"));
        }
        other => panic!("{other:?}"),
    }

    let dir = tempfile::tempdir().unwrap();
    seed_with_script(dir.path(), "#!/bin/sh\nexit 3\n");
    let rd = finished_run(dir.path());
    let finished = read_all(&rd).unwrap();
    let due = u64::try_from(
        finished
            .iter()
            .find(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
            .unwrap()
            .ts,
    )
    .unwrap()
        + 120_000;
    let event = apb_engine::follow_up::read::observe_due(
        dir.path(),
        rd.file_name().unwrap().to_str().unwrap(),
        due,
    )
    .unwrap()
    .unwrap();
    match &event.payload {
        EventPayload::FollowUpObserved {
            status,
            verdict,
            reason,
            ..
        } => {
            // A failing script could not be read honestly: `unknown`, with
            // the reason, and nothing counted as observed.
            assert_eq!((status.as_str(), verdict.as_str()), ("unknown", "unknown"));
            assert!(reason.as_deref().unwrap_or_default().contains("exited"));
        }
        other => panic!("{other:?}"),
    }
}

/// Past grace: `missed` with the script not run (a marker file it would
/// write is absent).
#[test]
fn past_grace_is_missed_and_the_source_is_not_read() {
    let dir = tempfile::tempdir().unwrap();
    seed_with_script(
        dir.path(),
        "#!/bin/sh\ntouch marker-was-read\necho '{\"stage\": \"won\"}'\n",
    );
    let rd = finished_run(dir.path());
    let _ = fs::remove_file(dir.path().join("marker-was-read"));
    let finished = read_all(&rd).unwrap();
    let due = u64::try_from(
        finished
            .iter()
            .find(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
            .unwrap()
            .ts,
    )
    .unwrap()
        + 60_000;
    // Default grace is 24 h: 3 days past due is missed.
    let event = apb_engine::follow_up::read::observe_due(
        dir.path(),
        rd.file_name().unwrap().to_str().unwrap(),
        due + 3 * 24 * 3600 * 1000,
    )
    .unwrap()
    .unwrap();
    match &event.payload {
        EventPayload::FollowUpObserved {
            status,
            verdict,
            reason,
            ..
        } => {
            assert_eq!((status.as_str(), verdict.as_str()), ("missed", "unknown"));
            assert!(reason.as_deref().unwrap_or_default().contains("grace"));
        }
        other => panic!("{other:?}"),
    }
    assert!(
        !dir.path().join("marker-was-read").exists(),
        "the source was read past its grace"
    );
}

/// Two calls: one event. A held lease journals nothing.
#[test]
fn two_calls_journal_one_event_and_a_held_lease_journals_none() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let rd = finished_run(dir.path());
    let run_id = rd.file_name().unwrap().to_str().unwrap().to_string();
    let finished = read_all(&rd).unwrap();
    let due = u64::try_from(
        finished
            .iter()
            .find(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
            .unwrap()
            .ts,
    )
    .unwrap()
        + 120_000;
    let first = apb_engine::follow_up::read::observe_due(dir.path(), &run_id, due)
        .unwrap()
        .expect("the first call read it");
    let second = apb_engine::follow_up::read::observe_due(dir.path(), &run_id, due).unwrap();
    assert!(second.is_none(), "the second call wrote again");
    let events = read_all(&rd).unwrap();
    let (check, status, verdict) = observations(&events).expect("one observation");
    assert_eq!((check, status, verdict), ("deal_won", "observed", "met"));
    let _ = first;

    // A held lease: the next call journals nothing even before the read.
    // The lease is the one of the `run_finished` the follow-up is for.
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let rd = finished_run(dir.path());
    let run_id = rd.file_name().unwrap().to_str().unwrap().to_string();
    let (finished_seq, due) = finish_of(&rd);
    let lease = apb_engine::waker::Lease::try_take(
        &rd,
        apb_engine::waker::WakerFor::FollowUp(finished_seq),
    )
    .unwrap()
    .expect("the lease is free");
    let none = apb_engine::follow_up::read::observe_due(dir.path(), &run_id, due).unwrap();
    assert!(none.is_none(), "a held lease read the follow-up");
    // Released, the same call reads it: the refusal above was the lease's.
    drop(lease);
    let read = apb_engine::follow_up::read::observe_due(dir.path(), &run_id, due).unwrap();
    assert!(read.is_some(), "a free lease did not read the follow-up");
}

/// The `run_finished` seq of the run at `rd` and a time two minutes after
/// it (past the 1m `after`, inside the default grace).
fn finish_of(rd: &Path) -> (u64, u64) {
    let events = read_all(rd).unwrap();
    let finished = events
        .iter()
        .find(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
        .unwrap();
    (finished.seq, u64::try_from(finished.ts).unwrap() + 120_000)
}

/// A waker holds its lease while it waits, so a second arm or `apb resume
/// --all` sees it held and starts no other waker; a resume ends it.
#[test]
fn a_waiting_waker_holds_its_lease_until_a_resume_ends_it() {
    let _env = crate::common::env_lock();
    let _poll = crate::common::EnvVar::set(apb_engine::pause::waker::POLL_ENV, "50");
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let rd = finished_run(dir.path());
    let run_id = rd.file_name().unwrap().to_str().unwrap().to_string();
    let (finished_seq, _) = finish_of(&rd);
    let wait = apb_engine::waker::WakerFor::FollowUp(finished_seq);
    let root = dir.path().to_path_buf();
    let id = run_id.clone();
    let waker = std::thread::spawn(move || {
        apb_engine::follow_up::waker::run_follow_up_waker(&root, &id, finished_seq);
    });
    let deadline = std::time::Instant::now() + DEADLINE;
    while !apb_engine::waker::Lease::held(&rd, wait) {
        assert!(
            std::time::Instant::now() < deadline,
            "the waiting waker never held its lease"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // A resume after the end: the follow-up is no longer pending, so the
    // waker ends without journaling and lets the lease go.
    let mut log = apb_engine::event::EventLog::open(&rd).unwrap();
    log.append(EventPayload::resumed("done", None, false))
        .unwrap();
    drop(log);
    waker.join().unwrap();
    assert!(!apb_engine::waker::Lease::held(&rd, wait));
    assert!(observations(&read_all(&rd).unwrap()).is_none());
}

/// A run directory without an origin stamp is never read: `observe_due`
/// and `apb resume --all` journal nothing (the marker the script writes is
/// absent).
#[test]
fn an_imported_run_is_never_read() {
    let dir = tempfile::tempdir().unwrap();
    seed_with_script(
        dir.path(),
        "#!/bin/sh\ntouch marker-imported\necho '{\"stage\": \"won\"}'\n",
    );
    let rd = finished_run(dir.path());
    let run_id = rd.file_name().unwrap().to_str().unwrap().to_string();
    // The stamp the gate's origin check reads.
    let _ = fs::remove_file(rd.join("origin.stamp"));
    let finished = read_all(&rd).unwrap();
    let due = u64::try_from(
        finished
            .iter()
            .find(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
            .unwrap()
            .ts,
    )
    .unwrap()
        + 120_000;
    let none = apb_engine::follow_up::read::observe_due(dir.path(), &run_id, due).unwrap();
    assert!(none.is_none(), "an imported run was read: {none:?}");
    assert!(!dir.path().join("marker-imported").exists());
    // resume --all also leaves it alone.
    let _ = apb_engine::pause::resume_all(dir.path());
    assert!(!dir.path().join("marker-imported").exists());
    let events = read_all(&rd).unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::FollowUpObserved { .. }))
    );
}

/// A removed worktree: a script source reads `unknown`.
#[test]
fn a_removed_worktree_reads_unknown() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    // The run resolves `work` as its working tree (a directory that
    // exists), which the test removes afterwards.
    fs::create_dir_all(dir.path().join("work")).unwrap();
    let (tx, rx) = mpsc::channel();
    let root = dir.path().to_path_buf();
    std::thread::spawn(move || {
        let r = run(
            &root,
            "fu",
            None,
            RunOptions {
                worktree: Some("work".into()),
                ..RunOptions::default()
            },
        )
        .unwrap();
        let _ = tx.send((r.outcome, root.join(".apb/runs").join(&r.run_id)));
    });
    let (status, rd) = rx.recv_timeout(DEADLINE).unwrap();
    assert_eq!(status, RunStatus::Succeeded);
    let run_id = rd.file_name().unwrap().to_str().unwrap().to_string();
    assert!(
        read_all(&rd)
            .unwrap()
            .iter()
            .any(|e| matches!(e.payload, EventPayload::WorktreeResolved { .. })),
        "the run resolved its worktree"
    );
    let _ = fs::remove_dir_all(dir.path().join("work"));
    let finished = read_all(&rd).unwrap();
    let due = u64::try_from(
        finished
            .iter()
            .find(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
            .unwrap()
            .ts,
    )
    .unwrap()
        + 120_000;
    let event = apb_engine::follow_up::read::observe_due(dir.path(), &run_id, due)
        .unwrap()
        .unwrap();
    match &event.payload {
        EventPayload::FollowUpObserved {
            status,
            verdict,
            reason,
            ..
        } => {
            assert_eq!(
                (status.as_str(), verdict.as_str()),
                ("unknown", "unknown"),
                "a gone worktree is unknown, never met or missed"
            );
            assert!(
                reason.as_deref().unwrap_or_default().contains("gone"),
                "{reason:?}"
            );
        }
        other => panic!("{other:?}"),
    }
}

/// A configured secret in an observed value is absent from the journal
/// bytes while the verdict is computed from the raw value; unresolved
/// secrets give `unknown` with no observations.
#[test]
fn secrets_are_scrubbed_or_withheld() {
    // A secret param whose value the script prints: the journal does not
    // hold it.
    let dir = tempfile::tempdir().unwrap();
    let secret = "hunter2-secret-value";
    let yaml = PLAYBOOK
        .replace(
            "follow_up:",
            "params:\n  - { name: token, type: text, secret: true }\nfollow_up:",
        )
        .replace(
            "- { id: stage, type: field, path: stage, expect: { equals: won } }",
            "- { id: stage, type: list, path: stage, expect: { contains: won } }",
        );
    apb_core::run_origin::ensure_key().unwrap();
    // A follow-up script read runs in the execution root under the tree
    // guard: the root must be a git repository with a commit.
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["config", "user.email", "t@t"]);
    git(dir.path(), &["config", "user.name", "t"]);
    git(dir.path(), &["commit", "-q", "--allow-empty", "-m", "seed"]);
    init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".apb/playbooks/fu/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        vdir.join("scripts/check.sh"),
        format!("#!/bin/sh\necho '{{\"stage\": [\"won\", \"{secret}\"]}}'\n"),
    )
    .unwrap();
    fs::write(dir.path().join(".apb/playbooks/fu/current"), "1.0.0").unwrap();
    // The start carries the secret param's value: it lands in run.yaml,
    // and the run's redactor resolves it from there.
    let (tx, rx) = mpsc::channel();
    let root = dir.path().to_path_buf();
    std::thread::spawn(move || {
        let mut opts = RunOptions::default();
        opts.params.insert("token".into(), secret.to_string());
        let r = run(&root, "fu", None, opts).unwrap();
        let _ = tx.send((r.outcome, root.join(".apb/runs").join(&r.run_id)));
    });
    let _ = rx.recv_timeout(DEADLINE).unwrap();
    let dirs: Vec<PathBuf> = fs::read_dir(dir.path().join(".apb/runs"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    let rd = &dirs[0];
    let finished = read_all(rd.as_path()).unwrap();
    let due = u64::try_from(
        finished
            .iter()
            .find(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
            .unwrap()
            .ts,
    )
    .unwrap()
        + 120_000;
    let run_id = rd.file_name().unwrap().to_str().unwrap().to_string();
    let event = apb_engine::follow_up::read::observe_due(dir.path(), &run_id, due)
        .unwrap()
        .unwrap();
    match &event.payload {
        EventPayload::FollowUpObserved { verdict, .. } => {
            assert_eq!(verdict, "met", "the verdict is computed from the raw value");
        }
        other => panic!("{other:?}"),
    }
    let bytes = fs::read_to_string(rd.join("events.jsonl")).unwrap();
    assert!(!bytes.contains(secret), "the secret reached the journal");

    // Unresolved secrets: the event is `unknown` with no observations.
    let dir = tempfile::tempdir().unwrap();
    seed_with_script(dir.path(), "#!/bin/sh\necho '{\"stage\": \"won\"}'\n");
    let rd = finished_run(dir.path());
    let run_id = rd.file_name().unwrap().to_str().unwrap().to_string();
    // Point the run's redactor at a config that cannot resolve: remove the
    // run's own run.yaml (the redactor withholds).
    let _ = fs::remove_file(rd.join("run.yaml"));
    let finished = read_all(rd.as_path()).unwrap();
    let due = u64::try_from(
        finished
            .iter()
            .find(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
            .unwrap()
            .ts,
    )
    .unwrap()
        + 120_000;
    let event = apb_engine::follow_up::read::observe_due(dir.path(), &run_id, due)
        .unwrap()
        .unwrap();
    match &event.payload {
        EventPayload::FollowUpObserved {
            status,
            verdict,
            observations,
            ..
        } => {
            assert_eq!((status.as_str(), verdict.as_str()), ("unknown", "unknown"));
            assert!(observations.is_empty(), "{observations:?}");
        }
        other => panic!("{other:?}"),
    }
}
