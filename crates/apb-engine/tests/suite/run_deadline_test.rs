//! A run's deadline (routine operations, D2): the run ends aborted
//! `deadline_exceeded` in every state it can be in when the deadline
//! passes, the clock leaves out a person's pause, a resume is refused or
//! granted time, and the guard and the patch rule keep the limit in place.
//! Deadlines here are a few seconds, set through `RunOptions.deadline_ms`
//! and `ResumeRequest.deadline_ms` (the validator's minute floor is for
//! authors).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::error::EngineError;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::pause::kind::PauseBy;
use apb_engine::scheduler::{ResumeRequest, RunOptions, RunResult, resume_req, run};
use apb_engine::state::{RunState, RunStatus};

use crate::common;

const WAIT: Duration = Duration::from_secs(30);

/// Holds the shared env lock and the variables a test sets.
struct Env {
    _lock: std::sync::MutexGuard<'static, ()>,
    keys: Vec<&'static str>,
}

impl Env {
    fn set(vars: &[(&'static str, &str)]) -> Self {
        let lock = common::env_lock();
        for (k, v) in vars {
            unsafe { std::env::set_var(k, v) };
        }
        Self {
            _lock: lock,
            keys: vars.iter().map(|(k, _)| *k).collect(),
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        for k in &self.keys {
            unsafe { std::env::remove_var(k) };
        }
    }
}

fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(start.elapsed() < WAIT, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn seed(root: &Path, id: &str, yaml: &str, scripts: &[(&str, String)]) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    for (name, body) in scripts {
        fs::write(vdir.join("scripts").join(name), body).unwrap();
    }
    common::seed_main(root);
}

/// A script that announces itself as `<name>_running`, records a SIGTERM
/// as `term_seen` and sleeps while the file `slow` exists.
fn slow_script(root: &Path, name: &str) -> String {
    let d = root.display();
    format!(
        "trap 'touch \"{d}/term_seen\"; exit 1' TERM\ntouch \"{d}/{name}_running\"\nwhile [ -f \"{d}/slow\" ]; do sleep 0.05; done\necho ok\n"
    )
}

fn one_script(id: &str) -> String {
    format!(
        "schema: 2\nid: {id}\nname: D\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: s, type: script, script: \"scripts/s.sh\", runner: sh }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: s }}\n  - {{ from: s, to: done }}\n"
    )
}

fn run_dir(root: &Path, run_id: &str) -> PathBuf {
    root.join(".apb/runs").join(run_id)
}

fn events(root: &Path, run_id: &str) -> Vec<Event> {
    read_all(&run_dir(root, run_id)).unwrap()
}

fn find_run(root: &Path, prefix: &str) -> String {
    poll(&format!("a run `{prefix}`"), || {
        fs::read_dir(root.join(".apb/runs"))
            .ok()?
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .find(|n| n.starts_with(prefix))
    })
}

fn spawn(
    root: &Path,
    id: &str,
    opts: RunOptions,
) -> mpsc::Receiver<Result<RunResult, EngineError>> {
    let (tx, rx) = mpsc::channel();
    let (root, id) = (root.to_path_buf(), id.to_string());
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, &id, None, opts));
    });
    rx
}

fn aborted_reason(events: &[Event]) -> Option<(String, Option<PauseBy>)> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::RunAborted { reason, by } => Some((reason.clone(), *by)),
        _ => None,
    })
}

fn assert_deadline_aborted(events: &[Event]) {
    let (reason, by) = aborted_reason(events).expect("a run_aborted");
    assert!(reason.starts_with("deadline_exceeded:"), "{reason}");
    assert_eq!(by, Some(PauseBy::Engine));
}

fn opts(deadline_ms: u64) -> RunOptions {
    RunOptions {
        deadline_ms: Some(deadline_ms),
        ..RunOptions::default()
    }
}

/// A run still working at its deadline ends aborted `deadline_exceeded`,
/// by the engine, with the graceful kill (the script saw SIGTERM).
#[cfg(unix)]
#[test]
fn a_run_ends_at_its_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        "dl",
        &one_script("dl"),
        &[("s.sh", slow_script(root, "s"))],
    );
    let _env = Env::set(&[("APB_PAUSE_GRACE_MS", "3000")]);
    fs::write(root.join("slow"), "").unwrap();
    let started = Instant::now();
    let rx = spawn(root, "dl", opts(1500));
    let res = rx.recv_timeout(WAIT).unwrap().unwrap();
    assert_eq!(res.outcome, RunStatus::Aborted);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(root.join("term_seen").is_file(), "no graceful kill");
    let ev = events(root, &res.run_id);
    assert_deadline_aborted(&ev);
    // The run argument is recorded on the provenance and guarded.
    let recorded = ev.iter().find_map(|e| match &e.payload {
        EventPayload::RunProvenance { deadline, .. } => *deadline,
        _ => None,
    });
    assert_eq!(recorded.map(|d| d.total_ms), Some(1500));
    assert!(ev.iter().any(|e| matches!(&e.payload,
        EventPayload::RunGuard { requires } if requires.iter().any(|r| r == "deadline"))));
}

/// A person's pause stops the clock: a run paused by a person past its
/// deadline's wall time is not ended, and a plain resume of a run whose
/// deadline passed is refused with nothing journaled.
#[cfg(unix)]
#[test]
fn an_operator_pause_does_not_count() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        "op",
        &one_script("op"),
        &[("s.sh", slow_script(root, "s"))],
    );
    let _env = Env::set(&[("APB_PAUSE_GRACE_MS", "300")]);
    fs::write(root.join("slow"), "").unwrap();
    let rx = spawn(root, "op", opts(2500));
    let run_id = find_run(root, "op-");
    poll("the script", || {
        root.join("s_running").is_file().then_some(())
    });
    apb_engine::pause::request::request_pause(
        root,
        &run_id,
        &apb_engine::pause::PauseRequest {
            reason: Some("later".into()),
            by: Some(PauseBy::Operator),
            mode: apb_engine::pause::PauseMode::Now,
            ..apb_engine::pause::PauseRequest::default()
        },
    )
    .unwrap();
    let res = rx.recv_timeout(WAIT).unwrap().unwrap();
    assert_eq!(res.outcome, RunStatus::Paused);
    std::thread::sleep(Duration::from_millis(3000));
    let ev = events(root, &run_id);
    assert_eq!(
        RunState::fold(&ev).run_status,
        RunStatus::Paused,
        "a person's pause was ended"
    );
    assert!(apb_engine::run_deadline::remaining_ms(&ev, apb_core::clock::now_ms()).unwrap() > 0);
}

/// A plain resume past the deadline is refused `deadline_passed` with
/// nothing journaled; a resume with `deadline_ms` grants time from now.
#[cfg(unix)]
#[test]
fn a_plain_resume_past_the_deadline_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        "rs",
        &one_script("rs"),
        &[("s.sh", slow_script(root, "s"))],
    );
    let _env = Env::set(&[("APB_PAUSE_GRACE_MS", "300")]);
    fs::write(root.join("slow"), "").unwrap();
    let res = spawn(root, "rs", opts(1000))
        .recv_timeout(WAIT)
        .unwrap()
        .unwrap();
    assert_eq!(res.outcome, RunStatus::Aborted);
    let before = fs::read(run_dir(root, &res.run_id).join("events.jsonl")).unwrap();
    let e = resume_req(root, &res.run_id, &ResumeRequest::operator()).unwrap_err();
    assert!(e.to_string().contains("deadline_passed"), "{e}");
    assert_eq!(
        fs::read(run_dir(root, &res.run_id).join("events.jsonl")).unwrap(),
        before,
        "a refused resume journaled something"
    );
    let e =
        apb_engine::scheduler::resume_detached_req(root, &res.run_id, &ResumeRequest::operator())
            .unwrap_err();
    assert!(e.to_string().contains("deadline_passed"), "{e}");
    assert!(!run_dir(root, &res.run_id).join("driver.pid").exists());
}

/// A resume with more time: the total is the elapsed time plus the grant,
/// journaled on `run_resumed`, and the run finishes.
#[cfg(unix)]
#[test]
fn a_resume_with_deadline_grants_time() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        "gr",
        &one_script("gr"),
        &[("s.sh", slow_script(root, "s"))],
    );
    let _env = Env::set(&[("APB_PAUSE_GRACE_MS", "300")]);
    fs::write(root.join("slow"), "").unwrap();
    let res = spawn(root, "gr", opts(1000))
        .recv_timeout(WAIT)
        .unwrap()
        .unwrap();
    assert_eq!(res.outcome, RunStatus::Aborted);
    let elapsed = apb_engine::pause::deadline_elapsed_ms(
        &events(root, &res.run_id),
        apb_core::clock::now_ms(),
    );
    fs::remove_file(root.join("slow")).unwrap();
    let resumed = resume_req(
        root,
        &res.run_id,
        &ResumeRequest {
            from_node: Some("s"),
            deadline_ms: Some(60_000),
            ..ResumeRequest::operator()
        },
    )
    .unwrap();
    assert_eq!(
        resumed.outcome,
        RunStatus::Succeeded,
        "{:#?}",
        events(root, &res.run_id)
            .iter()
            .map(|e| format!("{} {:?}", e.ts, e.payload))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let ev = events(root, &res.run_id);
    let granted = ev.iter().find_map(|e| match &e.payload {
        EventPayload::RunResumed { deadline, .. } => *deadline,
        _ => None,
    });
    let granted = granted.expect("the grant on run_resumed");
    assert_eq!(
        granted.source,
        apb_engine::run_deadline::DeadlineSource::Resume
    );
    let total = u128::from(granted.total_ms);
    assert!(
        total >= elapsed + 60_000 && total < elapsed + 61_000,
        "{total} vs {elapsed}"
    );
}

/// A run started without a deadline and resumed with one gets the guard
/// before its `run_resumed`; a supervisor patch that changes `deadline`
/// is refused by the forward-patch rule.
#[cfg(unix)]
#[test]
fn a_resume_grant_adds_the_guard() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        "gd",
        &one_script("gd"),
        &[("s.sh", slow_script(root, "s"))],
    );
    let _env = Env::set(&[("APB_PAUSE_GRACE_MS", "300")]);
    fs::write(root.join("slow"), "").unwrap();
    let rx = spawn(root, "gd", RunOptions::default());
    let run_id = find_run(root, "gd-");
    poll("the script", || {
        root.join("s_running").is_file().then_some(())
    });
    apb_engine::stop::stop_run(root, &run_id).unwrap();
    rx.recv_timeout(WAIT).unwrap().unwrap();
    assert!(
        !events(root, &run_id)
            .iter()
            .any(|e| matches!(e.payload, EventPayload::RunGuard { .. }))
    );
    fs::remove_file(root.join("slow")).unwrap();
    resume_req(
        root,
        &run_id,
        &ResumeRequest {
            from_node: Some("s"),
            deadline_ms: Some(60_000),
            ..ResumeRequest::operator()
        },
    )
    .unwrap();
    let ev = events(root, &run_id);
    let guard = ev.iter().position(|e| {
        matches!(&e.payload,
        EventPayload::RunGuard { requires } if requires.iter().any(|r| r == "deadline"))
    });
    let resumed = ev
        .iter()
        .position(|e| matches!(e.payload, EventPayload::RunResumed { .. }));
    assert!(guard.is_some() && guard < resumed, "{guard:?} {resumed:?}");
}

// ---- Parked runs: the deadline waker's body, run in a thread here (a test
// binary is not `apb`, so no waker process starts) ----

/// A journal of a driverless run with a deadline of `total_ms` and the
/// given events after its start.
fn seed_parked(root: &Path, run_id: &str, total_ms: u64, extra: &[EventPayload]) -> PathBuf {
    init_project(root).unwrap();
    let dir = run_dir(root, run_id);
    fs::create_dir_all(&dir).unwrap();
    let mut log = apb_engine::event::EventLog::create(&dir).unwrap();
    log.append(EventPayload::RunStarted {
        playbook: "parked".into(),
        version: "1.0.0".into(),
        definition: None,
        external_data: false,
    })
    .unwrap();
    let provenance: EventPayload = serde_json::from_value(serde_json::json!({
        "type": "run_provenance",
        "deadline": { "total_ms": total_ms, "source": "run" }
    }))
    .unwrap();
    log.append(provenance).unwrap();
    for p in extra {
        log.append(p.clone()).unwrap();
    }
    dir
}

fn paused_by(by: PauseBy, until: Option<u64>) -> EventPayload {
    EventPayload::RunPaused {
        reason: "budget (test)".into(),
        by: Some(by),
        mode: None,
        until,
        keep_lock: false,
    }
}

/// Runs the deadline waker's body in a thread; returns when it ended.
fn waker(root: &Path, run_id: &str) -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::channel();
    let (root, run_id) = (root.to_path_buf(), run_id.to_string());
    std::thread::spawn(move || {
        apb_engine::pause::waker::run_deadline_waker(&root, &run_id);
        let _ = tx.send(());
    });
    rx
}

/// An engine pause without `until` (a budget pause): the deadline waker
/// ends the run `deadline_exceeded`, and no resume happens.
#[test]
fn a_parked_run_ends_at_its_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = Env::set(&[("APB_WAKER_POLL_MS", "50")]);
    seed_parked(root, "pk-1", 1000, &[paused_by(PauseBy::Engine, None)]);
    waker(root, "pk-1").recv_timeout(WAIT).unwrap();
    let ev = events(root, "pk-1");
    assert_deadline_aborted(&ev);
    assert!(
        !ev.iter()
            .any(|e| matches!(e.payload, EventPayload::RunResumed { .. }))
    );
}

/// A child the engine paused (a quota pause) and handed back to its parent
/// has no driver of its own and no deadline: the parent's deadline ends it
/// too, it is not left paused with an abort nobody applies.
#[test]
fn a_parked_child_ends_with_its_parent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = Env::set(&[("APB_WAKER_POLL_MS", "50")]);
    let child = root.join(".apb/runs/ch-1");
    fs::create_dir_all(&child).unwrap();
    let mut log = apb_engine::event::EventLog::create(&child).unwrap();
    log.append(EventPayload::RunStarted {
        playbook: "child".into(),
        version: "1.0.0".into(),
        definition: None,
        external_data: false,
    })
    .unwrap();
    log.append(EventPayload::RunPaused {
        reason: "quota (test)".into(),
        by: Some(PauseBy::Engine),
        mode: None,
        until: Some(u64::MAX / 2),
        keep_lock: false,
    })
    .unwrap();
    seed_parked(
        root,
        "pp-1",
        1000,
        &[
            EventPayload::ChildRunStarted {
                node_id: "c".into(),
                run_id: "ch-1".into(),
            },
            paused_by(PauseBy::Engine, None),
        ],
    );
    waker(root, "pp-1").recv_timeout(WAIT).unwrap();
    assert_deadline_aborted(&events(root, "pp-1"));
    assert_eq!(
        RunState::fold(&events(root, "ch-1")).run_status,
        RunStatus::Aborted,
        "the parked child outlived its parent's deadline"
    );
}

/// A real child (its run config names the parent) quota-pauses and hands
/// its node back; the parent drives on into a gate and waits there, live.
/// The child's liveness follows that live parent, yet nothing drives the
/// child: the parent's deadline ends it too, instead of posting an abort
/// nobody applies.
#[cfg(unix)]
#[test]
fn a_parked_child_of_a_live_parent_ends_at_the_parents_deadline() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let child_yaml = "schema: 1\nid: qchild\nname: Q\nversion: 1.0.0\ndefaults:\n  profile: main\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: \"do\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n";
    seed(root, "qchild", child_yaml, &[]);
    let parent_yaml = "schema: 2\nid: lp\nname: LP\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: sub, type: playbook, playbook: qchild }\n  - { id: gate, type: human_review, title: Wait }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: sub }\n  - { from: sub, to: gate }\n  - { from: gate, to: done }\n";
    seed(root, "lp", parent_yaml, &[]);
    let agent = root.join("agent.sh");
    common::write_sync(
        &agent,
        "#!/bin/sh\necho '429 FreeUsageLimitError: Rate limit exceeded. Retry-After: 3600' 1>&2\nexit 1\n",
    );
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    let _env = Env::set(&[
        ("APB_AGENT_CMD", agent.to_str().unwrap()),
        ("APB_INFRA_BACKOFF_MS", "20,20"),
        ("APB_QUOTA_BACKOFF_MS", "600000,1200000"),
    ]);
    let rx = spawn(root, "lp", opts(2500));
    let parent = find_run(root, "lp-");
    let child = find_run(root, "qchild-");
    // The parent is live in its gate while the child sits parked.
    poll("the parent's gate", || {
        events(root, &parent)
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::ReviewRequested { .. }))
            .then_some(())
    });
    assert_eq!(
        RunState::fold(&events(root, &child)).run_status,
        RunStatus::Paused
    );
    assert_eq!(
        apb_engine::run_config::read_run_config(&run_dir(root, &child))
            .unwrap()
            .parent_run
            .as_deref(),
        Some(parent.as_str())
    );
    assert_eq!(
        apb_engine::liveness::driver_alive(&run_dir(root, &child), &child),
        Some(true),
        "the child's liveness follows its live parent"
    );
    let res = rx.recv_timeout(WAIT).unwrap().unwrap();
    assert_eq!(res.outcome, RunStatus::Aborted);
    assert_deadline_aborted(&events(root, &parent));
    assert_eq!(
        RunState::fold(&events(root, &child)).run_status,
        RunStatus::Aborted,
        "the parked child outlived its live parent's deadline"
    );
}

/// A run released with no driver (`drive_released`) ends at its deadline
/// too: the waker watches every park, not only pauses.
#[test]
fn a_released_run_ends_at_its_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = Env::set(&[("APB_WAKER_POLL_MS", "50")]);
    let far = apb_core::clock::now_ms_u64() + 3_600_000;
    seed_parked(
        root,
        "rl-1",
        1000,
        &[EventPayload::DriveReleased {
            wake_at_ms: far,
            nodes: vec!["s".into()],
        }],
    );
    waker(root, "rl-1").recv_timeout(WAIT).unwrap();
    assert_deadline_aborted(&events(root, "rl-1"));
}

/// A person's pause freezes the clock: the waker ends without stopping
/// the run.
#[test]
fn a_persons_pause_is_left_to_the_person() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = Env::set(&[("APB_WAKER_POLL_MS", "50")]);
    seed_parked(root, "pp-1", 1000, &[paused_by(PauseBy::Operator, None)]);
    std::thread::sleep(Duration::from_millis(1200));
    waker(root, "pp-1").recv_timeout(WAIT).unwrap();
    let ev = events(root, "pp-1");
    assert_eq!(RunState::fold(&ev).run_status, RunStatus::Paused);
}

/// A quota pause whose `until` came after its deadline: the pause waker
/// ends the run instead of probing (no probe marker is journaled).
#[test]
fn a_waker_does_not_probe_a_run_past_its_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = Env::set(&[("APB_WAKER_POLL_MS", "50")]);
    let until = apb_core::clock::now_ms_u64() + 1500;
    let pause = apb_engine::pause::quota::quota_pause("claude", "haiku", until);
    seed_parked(root, "qp-1", 500, &[pause.to_event()]);
    let seq = events(root, "qp-1").last().unwrap().seq;
    let woke = apb_engine::pause::run_waker(root, "qp-1", seq).unwrap();
    assert_eq!(woke, apb_engine::pause::WakeOutcome::Superseded);
    let ev = events(root, "qp-1");
    assert_deadline_aborted(&ev);
    assert!(
        !ev.iter().any(|e| matches!(&e.payload,
            EventPayload::SupervisorAction { action, .. } if action == "quota_probe")),
        "a probe ran for a run past its deadline"
    );
}

/// A quota pause replaced by its probe's re-pause while the deadline
/// waker waits: the waker keeps watching the new park and ends the run at
/// its deadline, with no probe after the re-pause.
#[test]
fn a_quota_repause_keeps_the_deadline_watch() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = Env::set(&[("APB_WAKER_POLL_MS", "50")]);
    let far = apb_core::clock::now_ms_u64() + 3_600_000;
    let quota = |until| apb_engine::pause::quota::quota_pause("claude", "haiku", until).to_event();
    let run = seed_parked(root, "qr-1", 1500, &[quota(far)]);
    let done = waker(root, "qr-1");
    std::thread::sleep(Duration::from_millis(400));
    // The probe's re-pause: a marker and a new engine pause over the old.
    let mut log = apb_engine::event::EventLog::open(&run).unwrap();
    log.append(EventPayload::SupervisorAction {
        action: "quota_probe".into(),
        node: Some("work".into()),
        detail: "quota (test)".into(),
    })
    .unwrap();
    log.append(quota(far + 1)).unwrap();
    drop(log);
    done.recv_timeout(WAIT).unwrap();
    let ev = events(root, "qr-1");
    assert_deadline_aborted(&ev);
    let probes = ev
        .iter()
        .filter(|e| {
            matches!(&e.payload,
            EventPayload::SupervisorAction { action, .. } if action == "quota_probe")
        })
        .count();
    assert_eq!(probes, 1, "a probe ran after the re-pause");
}

/// `apb resume --all` after a reboot: a driverless run past its deadline
/// is ended (`DeadlineStopped`), one with time left is left parked.
#[test]
fn resume_all_ends_an_expired_parked_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_parked(root, "ra-1", 1, &[paused_by(PauseBy::Engine, None)]);
    seed_parked(root, "ra-2", 3_600_000, &[paused_by(PauseBy::Engine, None)]);
    std::thread::sleep(Duration::from_millis(20));
    let entries = apb_engine::pause::resume_all::resume_all(root).unwrap();
    let action = |id: &str| {
        entries
            .iter()
            .find(|e| e.run_id == id)
            .map(|e| e.action.clone())
    };
    assert_eq!(
        action("ra-1"),
        Some(apb_engine::pause::resume_all::ResumeAllAction::DeadlineStopped)
    );
    assert_deadline_aborted(&events(root, "ra-1"));
    assert_eq!(
        RunState::fold(&events(root, "ra-2")).run_status,
        RunStatus::Paused
    );
}

// ---- Children, batches and the races decided under the event lock ----

fn seed_parent_child(root: &Path) {
    let d = root.display();
    seed(
        root,
        "child",
        "schema: 2\nid: child\nname: child\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: w, type: script, script: \"scripts/w.sh\", runner: sh }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: w }\n  - { from: w, to: f }\n",
        &[(
            "w.sh",
            format!(
                "trap 'touch \"{d}/child_term\"; exit 1' TERM\ntouch \"{d}/child_running\"\nwhile [ -f \"{d}/slow\" ]; do sleep 0.05; done\necho ok\n"
            ),
        )],
    );
    seed(
        root,
        "parent",
        "schema: 2\nid: parent\nname: parent\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: c, type: playbook, playbook: child }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: c }\n  - { from: c, to: f }\n",
        &[],
    );
}

fn child_run_id(parent_events: &[Event]) -> Option<String> {
    parent_events.iter().find_map(|e| match &e.payload {
        EventPayload::ChildRunStarted { run_id, .. } => Some(run_id.clone()),
        _ => None,
    })
}

/// A parent's deadline passes while its child runs: the child gets the
/// graceful kill and ends aborted, the parent ends `deadline_exceeded`.
#[cfg(unix)]
#[test]
fn a_parent_stops_its_child_at_its_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_parent_child(root);
    let _env = Env::set(&[("APB_PAUSE_GRACE_MS", "3000")]);
    fs::write(root.join("slow"), "").unwrap();
    let res = spawn(root, "parent", opts(1500))
        .recv_timeout(WAIT)
        .unwrap()
        .unwrap();
    assert_eq!(res.outcome, RunStatus::Aborted);
    let parent = events(root, &res.run_id);
    assert_deadline_aborted(&parent);
    assert!(
        root.join("child_term").is_file(),
        "the child got no graceful kill"
    );
    let child = child_run_id(&parent).expect("a child run");
    assert_eq!(
        RunState::fold(&events(root, &child)).run_status,
        RunStatus::Aborted
    );
}

/// A child prepared while its parent's deadline passed never runs a node:
/// its first control scan ends it aborted.
#[cfg(unix)]
#[test]
fn a_child_prepared_during_expiry_never_runs() {
    use apb_engine::run_deadline::test_hooks::{self, Hook};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    seed_parent_child(&root);
    let _env = Env::set(&[("APB_PAUSE_GRACE_MS", "300")]);
    fs::write(root.join("slow"), "").unwrap();
    // Hold the child's preparation until the parent's deadline passed.
    test_hooks::set(Hook::ChildPrepared, &root, |_| {
        std::thread::sleep(Duration::from_millis(2000));
    });
    let res = spawn(&root, "parent", opts(1000))
        .recv_timeout(WAIT)
        .unwrap()
        .unwrap();
    test_hooks::clear(&root);
    assert_eq!(res.outcome, RunStatus::Aborted);
    let parent = events(&root, &res.run_id);
    assert_deadline_aborted(&parent);
    let child = child_run_id(&parent).expect("a child run");
    let cev = events(&root, &child);
    assert!(
        cev.iter()
            .any(|e| matches!(e.payload, EventPayload::RunAborted { .. }))
    );
    assert!(
        !cev.iter()
            .any(|e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == "w")),
        "the child ran a node after its parent's deadline"
    );
    assert!(!root.join("child_running").exists());
}

/// No work starts after expiry: with the run watch held back, only the
/// admission checks stop a batch whose queued members would start late.
#[cfg(unix)]
#[test]
fn no_work_starts_after_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let d = root.display();
    let yaml = "schema: 2\nid: nw\nname: nw\nversion: 1.0.0\ndefaults:\n  max_parallel: 1\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: script, script: \"scripts/x.sh\", runner: sh }\n  - { id: b, type: script, script: \"scripts/x.sh\", runner: sh }\n  - { id: c, type: script, script: \"scripts/x.sh\", runner: sh }\n  - { id: m, type: prompt, prompt: m }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: start, to: b }\n  - { from: start, to: c }\n  - { from: a, to: m }\n  - { from: b, to: m }\n  - { from: c, to: m }\n  - { from: m, to: done }\n";
    seed(
        root,
        "nw",
        yaml,
        &[(
            "x.sh",
            format!("echo x >> \"{d}/starts\"\nsleep 0.8\necho ok\n"),
        )],
    );
    let _env = Env::set(&[
        ("APB_PAUSE_GRACE_MS", "300"),
        ("APB_TEST_DEADLINE_WATCH_DELAY_MS", "5000"),
    ]);
    let res = spawn(root, "nw", opts(1200))
        .recv_timeout(WAIT)
        .unwrap()
        .unwrap();
    assert_eq!(res.outcome, RunStatus::Aborted);
    let ev = events(root, &res.run_id);
    assert_deadline_aborted(&ev);
    let start = ev
        .iter()
        .find(|e| matches!(e.payload, EventPayload::RunStarted { .. }))
        .unwrap()
        .ts;
    let end = start + 1200;
    for e in &ev {
        let late = e.ts >= end
            && matches!(
                e.payload,
                EventPayload::AttemptStarted { .. } | EventPayload::NodeStarted { .. }
            );
        assert!(!late, "work started after the deadline: {:?}", e.payload);
    }
    let started = fs::read_to_string(root.join("starts")).unwrap_or_default();
    assert!(started.lines().count() < 3, "every member ran: {started}");
}

/// The finish answer's composer has its own attempt loop: a composer that
/// fails around the deadline, with the run watch held back, spawns no
/// retry after it.
#[cfg(unix)]
#[test]
fn no_composer_attempt_starts_after_expiry() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let d = root.display();
    let yaml = "schema: 2\nid: fa\nname: fa\nversion: 1.0.0\ndefaults:\n  profile: main\n  max_retries: 5\nnodes:\n  - { id: start, type: start }\n  - { id: f, type: finish, outcome: success, prompt: \"compose\" }\nedges:\n  - { from: start, to: f }\n";
    seed(root, "fa", yaml, &[]);
    let stub = root.join("composer.sh");
    common::write_sync(
        &stub,
        &format!("#!/bin/sh\necho x >> \"{d}/composed\"\nsleep 0.8\nexit 1\n"),
    );
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let _env = Env::set(&[
        ("APB_PAUSE_GRACE_MS", "300"),
        ("APB_TEST_DEADLINE_WATCH_DELAY_MS", "5000"),
    ]);
    let _agent = common::EnvVar::set("APB_AGENT_CMD", &stub);
    let _home = common::EnvVar::set("HOME", home.path());
    let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let res = spawn(root, "fa", opts(1200))
        .recv_timeout(WAIT)
        .unwrap()
        .unwrap();
    let ev = events(root, &res.run_id);
    let start = ev
        .iter()
        .find(|e| matches!(e.payload, EventPayload::RunStarted { .. }))
        .unwrap()
        .ts;
    for e in &ev {
        // `attempt_started` is journaled at spawn; `retry_started` rides
        // the return batch, so its time says nothing.
        let late = e.ts >= start + 1200 && matches!(e.payload, EventPayload::AttemptStarted { .. });
        assert!(
            !late,
            "a composer attempt started after the deadline: {:?}",
            e.payload
        );
    }
    let composed = fs::read_to_string(root.join("composed")).unwrap_or_default();
    assert!(
        composed.lines().count() <= 2,
        "composed {} times",
        composed.lines().count()
    );
}

/// The list half of [`no_work_starts_after_expiry`]: a list node of many
/// items one at a time starts no item after the deadline, with the run
/// watch held back.
#[cfg(unix)]
#[test]
fn no_list_item_starts_after_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let d = root.display();
    let items: Vec<String> = (1..=20).map(|i| format!("{{ id: i{i} }}")).collect();
    let yaml = format!(
        "schema: 2\nid: nl\nname: nl\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - id: l\n    type: script\n    script: \"scripts/x.sh\"\n    runner: sh\n    list:\n      items: {{ inline: [{}] }}\n      batch: {{ size: 1, parallel: 1, retries: 0 }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: l }}\n  - {{ from: l, to: done }}\n",
        items.join(", ")
    );
    seed(
        root,
        "nl",
        &yaml,
        &[(
            "x.sh",
            format!("echo x >> \"{d}/starts\"\nsleep 0.4\necho ok\n"),
        )],
    );
    let _env = Env::set(&[
        ("APB_PAUSE_GRACE_MS", "300"),
        ("APB_TEST_DEADLINE_WATCH_DELAY_MS", "5000"),
    ]);
    let res = spawn(root, "nl", opts(1500))
        .recv_timeout(WAIT)
        .unwrap()
        .unwrap();
    assert_eq!(res.outcome, RunStatus::Aborted);
    let ev = events(root, &res.run_id);
    assert_deadline_aborted(&ev);
    let start = ev
        .iter()
        .find(|e| matches!(e.payload, EventPayload::RunStarted { .. }))
        .unwrap()
        .ts;
    let end = start + 1500;
    for e in &ev {
        let late = e.ts >= end
            && matches!(
                e.payload,
                EventPayload::AttemptStarted { .. }
                    | EventPayload::NodeStarted { .. }
                    | EventPayload::ListBatchStarted { .. }
            );
        assert!(!late, "an item started after the deadline: {:?}", e.payload);
    }
    let started = fs::read_to_string(root.join("starts")).unwrap_or_default();
    assert!(
        (1..20).contains(&started.lines().count()),
        "items: {started}"
    );
}

/// A start queued behind a tree another run holds ends at its deadline in
/// the queue, aborted `deadline_exceeded`, without taking the lock or
/// starting a node.
#[cfg(unix)]
#[test]
fn a_tree_queued_run_ends_at_its_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        "tq",
        &one_script("tq"),
        &[("s.sh", "echo ok\n".into())],
    );
    let holder = apb_engine::workdir::acquire(root, false).unwrap().unwrap();
    let started = Instant::now();
    let rx = spawn(
        root,
        "tq",
        RunOptions {
            deadline_ms: Some(1500),
            workdir_queue_wait: Some(Duration::from_secs(60)),
            ..RunOptions::default()
        },
    );
    let run_id = find_run(root, "tq-");
    let _ = rx.recv_timeout(WAIT).unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
    let ev = events(root, &run_id);
    assert_deadline_aborted(&ev);
    assert!(
        !ev.iter()
            .any(|e| matches!(e.payload, EventPayload::NodeStarted { .. })),
        "a queued run started a node"
    );
    drop(holder);
}

/// The detached driver of a queued run (`drive_run_from_dir`, the body of
/// `apb __drive-run`) whose tree stays busy past the deadline ends the run
/// itself: its own `driver.pid` is not a live driver that will consume an
/// abort, and the wait ends at the deadline, not after the handover's
/// floor.
#[cfg(unix)]
#[test]
fn a_detached_queued_run_ends_at_its_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        "dq",
        &one_script("dq"),
        &[("s.sh", "echo ok\n".into())],
    );
    let prepared = apb_engine::scheduler::prepare_supervised_background(
        root,
        "dq",
        None,
        RunOptions {
            deadline_ms: Some(1500),
            workdir_queue_wait: Some(Duration::from_secs(60)),
            ..RunOptions::default()
        },
    )
    .unwrap();
    let run_id = prepared.run_id().to_string();
    let started = Instant::now();
    drop(prepared);
    // Another live process holds the tree, and the spawner published this
    // process as the run's driver, as `hand_to_detached_driver` does.
    let mut other = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    fs::write(
        apb_engine::workdir::tree_lock_path(root, None),
        other.id().to_string(),
    )
    .unwrap();
    apb_engine::driver::publish_driver_pid(&run_dir(root, &run_id), std::process::id());
    let res = apb_engine::drive_run_from_dir(root, &run_id);
    let took = started.elapsed();
    let _ = other.kill();
    let _ = other.wait();
    assert!(res.is_err(), "{res:?}");
    assert!(took < Duration::from_secs(4), "{took:?}");
    let ev = events(root, &run_id);
    assert_deadline_aborted(&ev);
    assert!(
        !ev.iter()
            .any(|e| matches!(e.payload, EventPayload::NodeStarted { .. })),
        "a queued run started a node"
    );
    assert!(
        apb_engine::driver::read_driver_pid(&run_dir(root, &run_id)).is_none(),
        "the driver that never drove left its pid behind"
    );
}

/// A person's pause that lands between the watch's firing and its stop
/// wins: the stop is not due (`Expect::Driving` no longer holds), the run
/// ends paused, and a plain resume of it is refused `deadline_passed`.
#[cfg(unix)]
#[test]
fn a_person_pause_before_the_stop_wins() {
    use apb_engine::run_deadline::test_hooks::{self, Hook};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    seed(
        &root,
        "pw",
        &one_script("pw"),
        &[("s.sh", slow_script(&root, "s"))],
    );
    let _env = Env::set(&[("APB_PAUSE_GRACE_MS", "300")]);
    fs::write(root.join("slow"), "").unwrap();
    let hook_root = root.clone();
    test_hooks::set(Hook::WatchBeforeStop, &root, move |run_dir| {
        let run_id = run_dir.file_name().unwrap().to_string_lossy().into_owned();
        apb_engine::pause::request::request_pause(
            &hook_root,
            &run_id,
            &apb_engine::pause::PauseRequest {
                reason: Some("mine".into()),
                by: Some(PauseBy::Operator),
                mode: apb_engine::pause::PauseMode::Now,
                ..apb_engine::pause::PauseRequest::default()
            },
        )
        .unwrap();
        // Let the drive journal the pause before the stop is decided.
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            let ev = read_all(run_dir).unwrap_or_default();
            if RunState::fold(&ev).run_status == RunStatus::Paused {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let res = spawn(&root, "pw", opts(1000))
        .recv_timeout(WAIT)
        .unwrap()
        .unwrap();
    // Give the watch thread time to decide (and not stop) after the pause.
    std::thread::sleep(Duration::from_millis(300));
    test_hooks::clear(&root);
    assert_eq!(res.outcome, RunStatus::Paused);
    let ev = events(&root, &res.run_id);
    assert!(
        aborted_reason(&ev).is_none(),
        "the stop ignored a person's pause"
    );
    let e = resume_req(&root, &res.run_id, &ResumeRequest::operator()).unwrap_err();
    assert!(e.to_string().contains("deadline_passed"), "{e}");
}

/// A grant a resume commits after a stop's caller read an expired journal
/// and before the stop takes the event lock: the stop rereads the journal
/// under the lock, is not due, and the run goes on to finish under its
/// grant with no `deadline_exceeded` abort.
#[cfg(unix)]
#[test]
fn a_grant_committed_first_is_not_aborted() {
    use apb_engine::run_deadline::test_hooks::{self, Hook};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let _env = Env::set(&[]);
    seed(
        &root,
        "parked",
        &one_script("parked"),
        &[("s.sh", slow_script(&root, "s"))],
    );
    let run = seed_parked(&root, "gc-1", 1, &[paused_by(PauseBy::Engine, None)]);
    let vdir = root.join(".apb/playbooks/parked/1.0.0");
    fs::copy(vdir.join("playbook.yaml"), run.join("playbook.yaml")).unwrap();
    fs::create_dir_all(run.join("scripts")).unwrap();
    fs::copy(vdir.join("scripts/s.sh"), run.join("scripts/s.sh")).unwrap();
    std::thread::sleep(Duration::from_millis(10));
    assert!(apb_engine::run_deadline::expired(
        &events(&root, "gc-1"),
        apb_core::clock::now_ms()
    ));
    fs::write(root.join("slow"), "").unwrap();
    // Between the stop's own read and its event lock, a person's resume
    // with a new deadline commits and its drive holds the run.
    let resume = std::sync::Arc::new(std::sync::Mutex::new(None));
    let slot = std::sync::Arc::clone(&resume);
    let hook_root = root.clone();
    test_hooks::set(Hook::StopBeforeLock, &root, move |_| {
        if slot.lock().unwrap().is_some() {
            return;
        }
        let r = hook_root.clone();
        let t = std::thread::spawn(move || {
            resume_req(
                &r,
                "gc-1",
                &ResumeRequest {
                    deadline_ms: Some(3_600_000),
                    ..ResumeRequest::operator()
                },
            )
        });
        poll("the resumed drive to run its node", || {
            hook_root.join("s_running").is_file().then_some(())
        });
        *slot.lock().unwrap() = Some(t);
    });
    let stopped = apb_engine::run_deadline::stop_if_expired(
        &root,
        "gc-1",
        apb_engine::run_deadline::Expect::Any,
    );
    test_hooks::clear(&root);
    fs::remove_file(root.join("slow")).unwrap();
    let handle = resume
        .lock()
        .unwrap()
        .take()
        .expect("the hook ran the resume");
    let resumed = handle.join().unwrap().expect("the granted resume");
    assert!(!stopped.unwrap(), "a stop went through after the grant");
    assert_eq!(resumed.outcome, RunStatus::Succeeded);
    let ev = events(&root, "gc-1");
    assert!(
        ev.iter().any(|e| matches!(
            &e.payload,
            EventPayload::RunResumed {
                deadline: Some(_),
                ..
            }
        )),
        "no grant was journaled"
    );
    assert!(
        aborted_reason(&ev).is_none(),
        "a deadline abort after a grant that left time"
    );
}

/// A plain resume whose preflight saw time left and whose deadline passed
/// before its commit is refused under the lock: no `run_resumed`.
#[test]
fn a_resume_that_expires_before_its_commit_is_refused() {
    use apb_engine::run_deadline::test_hooks::{self, Hook};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let _env = Env::set(&[]);
    seed(
        &root,
        "parked",
        &one_script("parked"),
        &[("s.sh", "echo ok\n".into())],
    );
    seed_parked(&root, "ex-1", 1000, &[paused_by(PauseBy::Engine, None)]);
    fs::copy(
        root.join(".apb/playbooks/parked/1.0.0/playbook.yaml"),
        run_dir(&root, "ex-1").join("playbook.yaml"),
    )
    .unwrap();
    test_hooks::set(Hook::BeforeResumeCommit, &root, |_| {
        std::thread::sleep(Duration::from_millis(2000));
    });
    let r = resume_req(
        &root,
        "ex-1",
        &ResumeRequest {
            by: PauseBy::Operator,
            ..ResumeRequest::operator()
        },
    );
    test_hooks::clear(&root);
    let e = r.unwrap_err();
    assert!(e.to_string().contains("deadline_passed"), "{e}");
    assert!(
        !events(&root, "ex-1")
            .iter()
            .any(|e| matches!(e.payload, EventPayload::RunResumed { .. }))
    );
}

// ---- Engine reads on the drive thread: the baseline check's connector
// read and its credential helper are bounded by the time the run has left,
// and a read cut short journals nothing ----

const CRM: &str = r#"
name: crm
version: 0.1.0
account_fields:
  - name: base_url
    required: true
  - name: token
    secret: true
functions:
  - name: count
    description: A count read
    read_only: true
    method: GET
    url: "{{account.base_url}}/count"
    timeout_sec: 600
"#;

/// A service stand-in: the first `fast` requests are answered at once
/// with a complete count of 0, every later one only after 30 s. `fast`
/// can be raised while it runs.
struct Crm {
    base_url: String,
    fast: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    seen: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

fn crm(fast: usize) -> Crm {
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let fast = Arc::new(AtomicUsize::new(fast));
    let seen = Arc::new(AtomicUsize::new(0));
    let (f, s) = (fast.clone(), seen.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let n = s.fetch_add(1, Ordering::SeqCst);
            let quick = n < f.load(Ordering::SeqCst);
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                if !quick {
                    std::thread::sleep(Duration::from_secs(30));
                    return;
                }
                let body = r#"{"totalCount": 0, "pageInfo": {"hasNextPage": false}}"#;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    Crm {
        base_url,
        fast,
        seen,
    }
}

/// A playbook whose one baseline check reads `crm.count` (a check bound of
/// `timeout` seconds) before a quick script; the account's token, when
/// given, is a `{{cmd:...}}` helper. Returns the run options
/// that pin the connector and account digests.
fn seed_baseline(
    root: &Path,
    cfg: &Path,
    id: &str,
    base_url: &str,
    timeout: u64,
    token: Option<&str>,
) -> RunOptions {
    let cdir = cfg.join("connectors/crm");
    fs::create_dir_all(&cdir).unwrap();
    fs::write(cdir.join("connector.yaml"), CRM).unwrap();
    let yaml = format!(
        "schema: 2\nid: {id}\nname: B\nversion: 1.0.0\nbaseline_check:\n  checks:\n    - id: deals\n      connector: {{ name: crm, function: count }}\n      complete_when: {{ path: pageInfo.hasNextPage, equals: false }}\n      timeout_seconds: {timeout}\n      observe:\n        - {{ id: count, type: number, path: totalCount, expect: {{ equals: 0 }} }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, type: script, script: \"scripts/w.sh\", runner: sh }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: done }}\n"
    );
    seed(root, id, &yaml, &[("w.sh", "true\n".to_string())]);
    // A `{{cmd:...}}` helper is allowed only in the global connector config.
    let (acct, line) = match token {
        Some(t) => (
            apb_core::connector::config::global_config_path("crm").unwrap(),
            format!("    token: \"{t}\"\n"),
        ),
        None => (
            apb_core::connector::config::project_config_path(root, "crm"),
            String::new(),
        ),
    };
    fs::create_dir_all(acct.parent().unwrap()).unwrap();
    fs::write(
        &acct,
        format!("accounts:\n  - name: main\n    base_url: {base_url}\n{line}"),
    )
    .unwrap();
    let pb = apb_core::schema::Playbook::from_yaml(&yaml).unwrap();
    let resolved = apb_core::connector::resolve::resolve_playbook(root, &pb).unwrap();
    let mut opts = RunOptions::default();
    for (name, c) in &resolved.connectors {
        opts.expected_connectors
            .insert(name.clone(), c.loaded.digest.clone());
        for a in &c.accounts {
            opts.expected_connector_accounts.insert(
                format!("{name}/{}", a.name),
                apb_core::connector::config::account_digest(a),
            );
        }
    }
    opts
}

/// The snapshot events journaled for the check `deals` in `phase`.
fn snapshots(ev: &[Event], phase: &str) -> Vec<String> {
    ev.iter()
        .filter_map(|e| match &e.payload {
            EventPayload::BaselineObserved {
                check,
                phase: p,
                status,
                ..
            } if check == "deals" && p == phase => Some(status.clone()),
            _ => None,
        })
        .collect()
}

/// The outcome view's reading of the check: not observed before, never met.
fn assert_not_observed(root: &Path, run_id: &str, ev: &[Event]) {
    let yaml = fs::read_to_string(run_dir(root, run_id).join("playbook.yaml")).unwrap();
    let pb = apb_core::schema::Playbook::from_yaml(&yaml).unwrap();
    let view = apb_engine::run_outcome::baseline_from(Some(&pb), ev).expect("a baseline");
    let check = &view.checks[0];
    assert_eq!(check.before_status, None, "{check:?}");
    assert!(
        check.observations.iter().all(|o| o.verdict != "met"),
        "{check:?}"
    );
}

/// A connector read slower than the run's time left ends with the
/// deadline: the run is aborted `deadline_exceeded` within a few seconds of
/// it, nothing is journaled for the check, and the outcome says it was not
/// observed. Bounding only the check's 300 s would keep the read for 30 s.
#[test]
fn a_baseline_read_is_bounded_by_the_deadline() {
    let cfg = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = Env::set(&[("APB_CONFIG_DIR", &cfg.path().display().to_string())]);
    let server = crm(0);
    let mut o = seed_baseline(root, cfg.path(), "bl", &server.base_url, 300, None);
    o.deadline_ms = Some(3000);
    let started = Instant::now();
    let res = run(root, "bl", None, o).unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(12),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(res.outcome, RunStatus::Aborted);
    let ev = events(root, &res.run_id);
    assert_deadline_aborted(&ev);
    assert!(
        snapshots(&ev, "before").is_empty(),
        "a cut read was journaled"
    );
    assert_not_observed(root, &res.run_id, &ev);
}

/// A `{{cmd:...}}` credential helper slower than the time left is bounded
/// too: without the lowered bound the helper keeps its own 10 s.
#[cfg(unix)]
#[test]
fn a_slow_credential_helper_is_bounded_by_the_deadline() {
    let cfg = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = Env::set(&[("APB_CONFIG_DIR", &cfg.path().display().to_string())]);
    let helper = root.join("slow-helper.sh");
    fs::write(&helper, "#!/bin/sh\nsleep 20\necho token\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let server = crm(usize::MAX);
    let token = format!("{{{{cmd:{}}}}}", helper.display());
    let mut o = seed_baseline(root, cfg.path(), "hl", &server.base_url, 300, Some(&token));
    o.deadline_ms = Some(3000);
    let started = Instant::now();
    let res = run(root, "hl", None, o).unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(9),
        "the helper outlived the deadline: {:?}",
        started.elapsed()
    );
    let ev = events(root, &res.run_id);
    assert_deadline_aborted(&ev);
    assert!(snapshots(&ev, "before").is_empty());
}

/// How a read is cut short: the run's deadline passes while it waits, or
/// a stop latches the cancel flag while it waits.
#[derive(Clone, Copy, Debug)]
enum Cut {
    Deadline,
    Stop,
}

/// Runs the baseline playbook with `fast` quick answers and cuts the next
/// read the way `cut` says. Returns the run's journal.
fn cut_run(id: &str, fast: usize, cut: Cut) -> Vec<Event> {
    let cfg = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = Env::set(&[
        ("APB_CONFIG_DIR", &cfg.path().display().to_string()),
        ("APB_PAUSE_GRACE_MS", "300"),
    ]);
    let server = crm(fast);
    // A stop does not end a blocking read: its bound (2 s) does.
    let timeout = match cut {
        Cut::Deadline => 300,
        Cut::Stop => 2,
    };
    let mut o = seed_baseline(root, cfg.path(), id, &server.base_url, timeout, None);
    if matches!(cut, Cut::Deadline) {
        o.deadline_ms = Some(3000);
    }
    let rx = spawn(root, id, o);
    if matches!(cut, Cut::Stop) {
        let run_id = find_run(root, &format!("{id}-"));
        poll("the slow read", || {
            (server.seen.load(std::sync::atomic::Ordering::SeqCst) > fast).then_some(())
        });
        apb_engine::stop::stop_run(root, &run_id).unwrap();
    }
    let res = rx.recv_timeout(WAIT).unwrap().unwrap();
    assert_eq!(res.outcome, RunStatus::Aborted, "{cut:?}");
    events(root, &res.run_id)
}

/// A before read cut short, by a stop or by the deadline, journals no
/// snapshot: the next drive takes it afresh.
#[test]
fn a_cut_before_read_journals_nothing() {
    for cut in [Cut::Deadline, Cut::Stop] {
        let ev = cut_run("cb", 0, cut);
        assert!(snapshots(&ev, "before").is_empty(), "{cut:?}");
        assert!(snapshots(&ev, "after").is_empty(), "{cut:?}");
    }
}

/// An after read cut short journals no after snapshot (the before one,
/// read in time, stays).
#[test]
fn a_cut_after_read_journals_nothing() {
    for cut in [Cut::Deadline, Cut::Stop] {
        let ev = cut_run("ca", 1, cut);
        assert_eq!(
            snapshots(&ev, "before"),
            vec!["observed".to_string()],
            "{cut:?}"
        );
        assert!(snapshots(&ev, "after").is_empty(), "{cut:?}");
    }
}

/// A person who resumes a run its deadline cut during the before read
/// gets a fresh read: the service now answers, and the before snapshot is
/// observed.
#[test]
fn a_resume_with_more_time_reads_afresh() {
    let cfg = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = Env::set(&[("APB_CONFIG_DIR", &cfg.path().display().to_string())]);
    let server = crm(0);
    let mut o = seed_baseline(root, cfg.path(), "rf", &server.base_url, 300, None);
    o.deadline_ms = Some(3000);
    let res = run(root, "rf", None, o).unwrap();
    assert_eq!(res.outcome, RunStatus::Aborted);
    assert!(snapshots(&events(root, &res.run_id), "before").is_empty());
    server
        .fast
        .store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
    let resumed = resume_req(
        root,
        &res.run_id,
        &ResumeRequest {
            from_node: Some("work"),
            deadline_ms: Some(600_000),
            ..ResumeRequest::operator()
        },
    )
    .unwrap();
    assert_eq!(resumed.outcome, RunStatus::Succeeded);
    assert_eq!(
        snapshots(&events(root, &res.run_id), "before"),
        vec!["observed".to_string()]
    );
}
