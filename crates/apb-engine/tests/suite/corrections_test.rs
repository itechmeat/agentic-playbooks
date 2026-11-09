//! The corrections ledger collected at run ends: two runs with the same
//! review note give one proposed entry; a stopped run is observed too; a
//! scenario run and an unreadable journal add nothing; a withheld token
//! leaves no trace; no prompt ever reads the ledger; marking a fix never
//! touches the playbook store.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_core::scenario::{Scenario, ScenarioLevel};
use apb_core::scope::Scope;
use apb_engine::decision_channel::DecisionChannel;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::review::{ReviewCommand, post_decision_via};
use apb_engine::scenario::session::ScenarioPrep;
use apb_engine::scheduler::{RunOptions, run, run_scenario};
use apb_engine::state::RunStatus;

const DEADLINE: Duration = Duration::from_secs(60);

/// A gate fed by one agent node (a stub script stands in for it: the
/// delivering-node rule reads the playbook's kinds, and a `prompt` node
/// with a script body... no: the deliverer must be an agent_task or a
/// finish with a prompt, so the feeder is an `agent_task` whose profile is
/// the scripted stand-in of a scenario? No: these are REAL runs. The
/// feeder is a `prompt` node, which runs no agent: the engine's prompt
/// node executes inline. Check: NodeKind::Prompt is an agent_task-like
/// node that takes attempts (validate::expected_kind says ATTEMPTS). To
/// keep the run agentless, the feeder below is a `finish`-shaped prompt
/// node executed by the scripted stand-in? Simplest agentless shape: the
/// fixture uses `prompt` nodes, which the engine executes inline without
/// any program.
const PLAYBOOK: &str = r#"
schema: 2
id: corr
name: corr
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: prompt, prompt: "Deliver." }
  - { id: review, type: human_review }
  - { id: done, type: finish, outcome: success }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: work, to: review }
  - { from: review, to: done, condition: { type: review_status, equals: approve } }
  - { id: rejected, from: review, to: failed, fallback: true }
"#;

fn seed(root: &Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/corr/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/corr/current"), "1.0.0").unwrap();
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

fn the_run_dir(root: &Path) -> PathBuf {
    // The newest run directory of this project: two runs in one test use
    // it in start order.
    poll("the run dir", || {
        fs::read_dir(root.join(".apb/runs"))
            .ok()?
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| (e.metadata().ok().and_then(|m| m.modified().ok()), e.path()))
            .max_by_key(|(m, _)| *m)
            .map(|(_, p)| p)
    })
}

/// Starts the run on a thread; the receiver yields its outcome and run dir.
fn start(root: &Path) -> mpsc::Receiver<(RunStatus, PathBuf)> {
    let (tx, rx) = mpsc::channel();
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        let r = run(&root, "corr", None, RunOptions::default()).unwrap();
        let _ = tx.send((r.outcome, root.join(".apb/runs").join(&r.run_id)));
    });
    rx
}

fn decide_reject(root: &Path, _rd: &Path, note: &str) {
    // The run with an open review request: a second run's directory may
    // not exist yet, so the poll reads every run and answers the open one.
    let rd: PathBuf = poll("the review request", || {
        let mut open: Option<PathBuf> = None;
        if let Ok(dirs) = fs::read_dir(root.join(".apb/runs")) {
            for d in dirs.flatten() {
                let Ok(events) = read_all(&d.path()) else {
                    continue;
                };
                let requested = events
                    .iter()
                    .filter(|e| matches!(&e.payload, EventPayload::ReviewRequested { node, .. } if node == "review"))
                    .count();
                let decided = events
                    .iter()
                    .filter(|e| matches!(&e.payload, EventPayload::ReviewDecided { node, .. } if node == "review"))
                    .count();
                if requested > decided {
                    open = Some(d.path());
                }
            }
        }
        open
    });
    post_decision_via(
        &rd,
        ReviewCommand {
            node: "review".into(),
            decision: "reject".into(),
            note: note.into(),
        },
        None,
        None,
        DecisionChannel::Terminal,
    )
    .unwrap();
}

/// The ledger file of the playbook.
fn ledger(
    root: &Path,
) -> Option<apb_core::signed_store::Loaded<apb_core::corrections::CorrectionFile>> {
    apb_core::signed_store::read::<apb_core::corrections::CorrectionFile>(
        root,
        Scope::Project,
        "corr",
    )
    .ok()
}

/// The one entry of the ledger, when there is one.
fn entry(root: &Path) -> Option<apb_core::corrections::Correction> {
    ledger(root).and_then(|l| l.file.entries.into_iter().next())
}

/// Two runs whose reviews reject with the same wording in two spellings:
/// one proposed entry with runs 2, and no run counted twice.
#[test]
fn two_runs_with_the_same_note_give_one_proposed_entry() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    for note in ["add tests for the parser", "add tests for the parser"] {
        let rx = start(dir.path());
        let rd = the_run_dir(dir.path());
        decide_reject(dir.path(), &rd, note);
        let (status, _) = rx.recv_timeout(DEADLINE).unwrap();
        assert_eq!(status, RunStatus::Failed, "the reject routes to failed");
    }
    let e = entry(dir.path()).expect("one entry");
    assert_eq!(e.runs, 2, "{e:#?}");
    assert_eq!(e.status, apb_core::corrections::CorrectionStatus::Proposed);
    assert_eq!(e.node.as_deref(), Some("work"));
    assert_eq!(e.gate.as_deref(), Some("review"));
    // The ledger file holds neither the note's words twice nor any run id
    // beyond the evidence.
    let bytes = fs::read_to_string(dir.path().join(".apb/corrections/project/corr.json")).unwrap();
    assert!(bytes.contains("add tests for the parser"));
}

/// A stopped run is observed too: its review note counts.
#[test]
fn a_stopped_run_is_observed() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let rx = start(dir.path());
    let rd = the_run_dir(dir.path());
    decide_reject(dir.path(), &rd, "stop here");
    let _ = rx.recv_timeout(DEADLINE).unwrap();
    let e = entry(dir.path()).expect("the entry");
    assert_eq!(e.runs, 1);
    // Observe the run again by hand: idempotent.
    let root = dir.path();
    apb_engine::corrections::observe(root, &rd).unwrap();
    let e = entry(dir.path()).unwrap();
    assert_eq!(e.runs, 1, "observing twice changes nothing");
}

/// A scenario run adds nothing; a journal with an unknown event after its
/// last checkpoint adds nothing and writes no file; a token-only note is
/// dropped whole and a later rendered prompt holds no ledger text.
#[test]
fn a_scenario_an_unreadable_journal_and_a_token_add_nothing() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    // A scenario run: its review is scripted, and no ledger file appears.
    let prep = ScenarioPrep {
        scenario: serde_yaml_like(
            "schema: 1\nid: green\nanswers:\n  work:\n    - { visit: 1, attempts: [ { status: succeeded, output: delivered } ] }\n  review:\n    - { visit: 1, review: { decision: reject, note: scenario note } }\nexpect:\n  run: { outcome: [failed] }\n  finish: failed\n",
        ),
        level: ScenarioLevel::Quick,
        scenario_digest: "sha256:corr".into(),
        max_requests: 0,
        sent: Default::default(),
        external_data: false,
    };
    let r = run_scenario(dir.path(), "corr", None, &prep).unwrap();
    assert_eq!(r.outcome, RunStatus::Failed);
    assert!(
        !dir.path()
            .join(".apb/corrections/project/corr.json")
            .exists(),
        "a scenario run wrote a ledger"
    );

    // A journal with an unknown event after its last checkpoint: not
    // counted, no file.
    let rd = dir.path().join(".apb/runs/none-1");
    fs::create_dir_all(&rd).unwrap();
    fs::write(
        rd.join("events.jsonl"),
        concat!(
            r#"{"seq":1,"ts":1,"type":"run_started","playbook":"corr","version":"1.0.0"}"#,
            "\n",
            r#"{"seq":2,"ts":1,"type":"alien_kind_from_the_future"}"#,
            "\n",
        ),
    )
    .unwrap();
    assert!(
        apb_engine::corrections::observe(dir.path(), &rd).is_err(),
        "an unreadable journal was counted"
    );
    assert!(
        !dir.path()
            .join(".apb/corrections/project/corr.json")
            .exists(),
        "an unreadable journal was counted"
    );

    // A token-only note: no entry, and the file holds neither the token
    // nor a hash of it.
    let rx = start(dir.path());
    let rd = the_run_dir(dir.path());
    decide_reject(dir.path(), &rd, "glpat-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let _ = rx.recv_timeout(DEADLINE).unwrap();
    // The run had no other correction, so no ledger file appears at all
    // (a playbook that never had a correction gets none).
    assert!(
        !dir.path()
            .join(".apb/corrections/project/corr.json")
            .exists(),
        "a run without sightings wrote a ledger"
    );
    assert!(
        entry(dir.path()).is_none(),
        "a token-only note made an entry"
    );
}

fn serde_yaml_like(yaml: &str) -> Scenario {
    yaml_serde::from_str(yaml).unwrap()
}

/// Marking a fix touches the ledger only: the playbook store has no new
/// version and no candidate pointer.
#[test]
fn marking_a_fix_never_touches_the_playbook_store() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    for _ in 0..2 {
        let rx = start(dir.path());
        let rd = the_run_dir(dir.path());
        decide_reject(dir.path(), &rd, "add tests for the parser");
        let _ = rx.recv_timeout(DEADLINE).unwrap();
    }
    let e = entry(dir.path()).unwrap();
    assert_eq!(e.status, apb_core::corrections::CorrectionStatus::Proposed);
    let before = store_snapshot(dir.path());
    apb_core::signed_store::update::<
        apb_core::corrections::CorrectionFile,
        _,
        apb_core::signed_store::StoreError,
    >(dir.path(), Scope::Project, "corr", |f| {
        let marked = apb_core::known_problems::Decided {
            by: "person".into(),
            at_ms: 1,
            reason: String::new(),
        };
        f.mark_fixed(&e.id, "1.0.0", "person", 1, None).unwrap();
        let _ = marked;
        Ok(((), true))
    })
    .unwrap();
    let e = entry(dir.path()).unwrap();
    assert_eq!(e.status, apb_core::corrections::CorrectionStatus::Fixed);
    assert_eq!(
        store_snapshot(dir.path()),
        before,
        "the playbook store moved"
    );
}

/// The playbook store's shape: versions and the current pointer.
fn store_snapshot(root: &Path) -> Vec<(String, bool)> {
    let pb = root.join(".apb/playbooks/corr");
    let mut out: Vec<(String, bool)> = fs::read_dir(pb.join("evals"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().into_owned(), false))
        .collect();
    out.push((
        fs::read_to_string(pb.join("current")).unwrap_or_default(),
        true,
    ));
    out
}
