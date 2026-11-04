//! The passkey gate: while this machine holds at least one passkey, a
//! person-only gate and a bound approval take a decision only with a fresh
//! WebAuthn assertion, which only the dashboard route posts, and the drive
//! consumes nothing else for such a gate. With no passkey enrolled
//! everything stays as it was (covered by the channel suites).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::decision_channel::{CallerRefusal, DecisionChannel};
use apb_engine::error::EngineError;
use apb_engine::event::{Event, EventPayload, read_all};

use crate::common;
use apb_engine::review::{ReviewCommand, post_decision_passkey, post_decision_via};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

const DEADLINE: Duration = Duration::from_secs(60);

// Every test here holds the suite's shared env lock for its whole body:
// they redirect APB_CONFIG_DIR, and a config dir another test deleted
// mid-run is exactly the race that lock avoids.
const PERSON_ONLY: &str = r#"schema: 2
id: po
name: po
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: g, type: human_review, options: [approve, reject], person_only: true }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: g }
  - { from: g, to: done }
"#;

const PLAIN: &str = r#"schema: 2
id: po
name: po
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: g, type: human_review, options: [approve, reject] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: g }
  - { from: g, to: done }
"#;

/// Points APB_CONFIG_DIR at a fresh directory with one passkey enrolled,
/// for the duration of `f`. The real config dir is never touched.
fn with_passkey_enrolled(f: impl FnOnce(&Path)) {
    let cfg = tempfile::tempdir().unwrap();
    let file = cfg.path().join("passkeys.yaml");
    apb_core::passkey::add_into(&file, "laptop", "Q0VER0lKUg", "A0VER0lKUg==", -7, 0).unwrap();
    let old = std::env::var("APB_CONFIG_DIR").ok();
    // SAFETY (tests): the suites already redirect APB_CONFIG_DIR elsewhere.
    unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
    f(cfg.path());
    match old {
        Some(v) => unsafe { std::env::set_var("APB_CONFIG_DIR", v) },
        None => unsafe { std::env::remove_var("APB_CONFIG_DIR") },
    }
}

fn seed(root: &Path, yaml: &str) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/po/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/po/current"), "1.0.0").unwrap();
}

fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let t = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(t.elapsed() < DEADLINE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn started(root: &Path, yaml: &str) -> (PathBuf, u64, mpsc::Receiver<(RunStatus, Vec<Event>)>) {
    seed(root, yaml);
    let (tx, rx) = mpsc::channel();
    let owned = root.to_path_buf();
    std::thread::spawn(move || {
        let r = run(&owned, "po", None, RunOptions::default()).unwrap();
        let rd = owned.join(".apb/runs").join(&r.run_id);
        let _ = tx.send((r.outcome, read_all(&rd).unwrap()));
    });
    let rd = poll("the run dir", || {
        fs::read_dir(root.join(".apb/runs"))
            .ok()?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| p.is_dir())
    });
    let seq = poll("the request", || {
        read_all(&rd).ok()?.iter().find_map(|e| match &e.payload {
            EventPayload::ReviewRequested { node, .. } if node == "g" => Some(e.seq),
            _ => None,
        })
    });
    (rd, seq, rx)
}

fn approve() -> ReviewCommand {
    ReviewCommand {
        node: "g".into(),
        decision: "approve".into(),
        note: String::new(),
    }
}

/// With a passkey enrolled, the terminal is no longer a channel for a
/// person-only gate: the refusal names the passkey and points at the
/// dashboard, and nothing is queued.
#[test]
fn a_passkey_enrolled_takes_the_terminal_off_a_person_only_gate() {
    let _guard = common::env_lock();
    with_passkey_enrolled(|_| {
        let dir = tempfile::tempdir().unwrap();
        let (rd, _, _rx) = started(dir.path(), PERSON_ONLY);
        for channel in [DecisionChannel::Terminal, DecisionChannel::CliUnattended] {
            match post_decision_via(&rd, approve(), None, None, channel) {
                Err(EngineError::ChannelRefused { refusal, message }) => {
                    assert_eq!(refusal, CallerRefusal::PasskeyRequired, "{channel:?}");
                    assert!(message.contains("dashboard"), "{message}");
                    assert!(message.contains("passkey"), "{message}");
                }
                other => panic!("{channel:?}: {other:?}"),
            }
        }
        let queued = fs::read_to_string(rd.join("reviews.jsonl")).unwrap_or_default();
        assert!(
            !queued.contains("approve"),
            "a refused decision must not be queued: {queued}"
        );
    });
}

/// The dashboard route's decision, posted through
/// `post_decision_passkey`, is stamped with the credential prefix and the
/// drive consumes it: the run finishes.
#[test]
fn a_dashboard_decision_with_a_passkey_stamp_decides_the_gate() {
    let _guard = common::env_lock();
    with_passkey_enrolled(|_| {
        let dir = tempfile::tempdir().unwrap();
        let (rd, _seq, rx) = started(dir.path(), PERSON_ONLY);
        post_decision_passkey(&rd, approve(), None, None, "Q0VER0lK".to_string()).unwrap();
        let (outcome, _) = rx.recv_timeout(DEADLINE).unwrap();
        assert_eq!(outcome, RunStatus::Succeeded, "the gate decided the run");
        let queued = fs::read_to_string(rd.join("reviews.jsonl")).unwrap();
        assert!(queued.contains("\"passkey\":\"Q0VER0lK\""), "{queued}");
    });
}

/// A channel entry a passkey-less machine left behind (a terminal entry
/// with no passkey stamp) is never consumed once this machine holds a
/// passkey: the run keeps waiting until the dashboard's stamped decision
/// arrives, and the doctor names the unstamped line.
#[test]
fn an_unstamped_entry_is_never_consumed_once_a_passkey_is_enrolled() {
    let _guard = common::env_lock();
    with_passkey_enrolled(|_| {
        let dir = tempfile::tempdir().unwrap();
        let (rd, seq, rx) = started(dir.path(), PERSON_ONLY);
        // The line an older apb (or a passkey-less machine) left in the
        // channel file: stamped with the request and the terminal channel,
        // but with no passkey assertion.
        {
            let path = rd.join("reviews.jsonl");
            let len = fs::read_to_string(&path)
                .unwrap_or_default()
                .lines()
                .count() as u64;
            let line = format!(
                "{{\"seq\":{len},\"node\":\"g\",\"decision\":\"approve\",\"request\":{seq},\"channel\":\"terminal\"}}\n"
            );
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(line.as_bytes()).unwrap();
        }
        // Well past the point the unstamped path would have ended the run.
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !matches!(rx.try_recv(), Ok((RunStatus::Succeeded, _))),
            "an unstamped entry must not decide a gated run once a passkey is enrolled"
        );
        post_decision_passkey(&rd, approve(), None, None, "Q0VER0lK".to_string()).unwrap();
        let (outcome, events) = rx.recv_timeout(DEADLINE).unwrap();
        assert_eq!(outcome, RunStatus::Succeeded);
        assert!(
            events.iter().any(
                |e| matches!(&e.payload, EventPayload::ReviewDecided { node, .. } if node == "g")
            ),
            "the stamped decision is what the drive consumed"
        );
    });
}

/// The requirement itself: a person-only or publish gate always needs the
/// assertion while a passkey is enrolled, an ordinary gate without a
/// bound request does not, and a playbook that does not read fails closed.
#[test]
fn passkey_required_follows_the_gate() {
    let _guard = common::env_lock();
    with_passkey_enrolled(|_| {
        let dir = tempfile::tempdir().unwrap();
        let rd = dir.path().join("r");
        fs::create_dir_all(&rd).unwrap();
        let cmd = |node: &str| ReviewCommand {
            node: node.into(),
            decision: "approve".into(),
            note: String::new(),
        };
        fs::write(rd.join("playbook.yaml"), PERSON_ONLY).unwrap();
        assert!(apb_engine::review::passkey_required(&rd, &cmd("g")));
        fs::write(rd.join("playbook.yaml"), PLAIN).unwrap();
        assert!(
            !apb_engine::review::passkey_required(&rd, &cmd("g")),
            "an ordinary gate with no bound request stays as it was"
        );
        fs::write(rd.join("playbook.yaml"), "schema: 2\nid: x\n").unwrap();
        assert!(
            apb_engine::review::passkey_required(&rd, &cmd("g")),
            "a playbook that does not read fails closed"
        );
    });
}

/// With no passkey enrolled, nothing changes: the terminal decides a
/// person-only gate after the typed confirmation, as before.
#[test]
fn no_passkey_enrolled_keeps_today_behaviour() {
    let _guard = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let (rd, _seq, rx) = started(dir.path(), PERSON_ONLY);
    post_decision_via(&rd, approve(), None, None, DecisionChannel::Terminal).unwrap();
    let (outcome, _) = rx.recv_timeout(DEADLINE).unwrap();
    assert_eq!(outcome, RunStatus::Succeeded);
}
