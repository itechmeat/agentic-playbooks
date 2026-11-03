//! The run guard: an older apb refuses to continue a run whose definition
//! uses a feature it would drop.

use apb_engine::event::{Event, read_journal};
use std::fs;

/// `run_guard` is a known, non-checkpoint record of this binary: it round
/// trips, and a journal whose last checkpoint it follows reads without an
/// unknown event. What an older apb does with it is the journal corpus's to
/// check against the real binaries.
#[test]
fn run_guard_round_trips_and_is_no_checkpoint() {
    let line =
        r#"{"seq":4,"ts":5,"type":"run_guard","requires":["approval_binding","person_only"]}"#;
    let e: Event = serde_json::from_str(line).unwrap();
    assert_eq!(serde_json::to_string(&e).unwrap(), line);
    assert!(!e.payload.is_checkpoint());

    let tmp = tempfile::tempdir().unwrap();
    let journal = [
        r#"{"seq":0,"ts":1,"type":"run_started","playbook":"p","version":"1.0.0"}"#,
        r#"{"seq":1,"ts":2,"type":"node_started","node":"a","attempt":1}"#,
        r#"{"seq":2,"ts":3,"type":"node_finished","node":"a","status":"succeeded","attempt":1,"output":"","artifacts":[]}"#,
        r#"{"seq":3,"ts":4,"type":"run_guard","requires":["person_only"]}"#,
        "",
    ]
    .join("\n");
    let dir = tmp.path().join(".apb/runs/r1");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("events.jsonl"), journal).unwrap();
    let journal = read_journal(&dir).unwrap();
    assert!(journal.unknown.is_empty());
    assert_eq!(journal.events.len(), 4);
}

use apb_engine::event::{EventLog, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use std::path::PathBuf;

use crate::common::{self, EnvVar};
use crate::deferred_retry_test::{agent, seed};

/// A run directory under a fresh project whose journal is `lines`.
fn journal_dir(lines: &[&str]) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(".apb/runs/r1");
    fs::create_dir_all(&dir).unwrap();
    let mut body = lines.join("\n");
    body.push('\n');
    fs::write(dir.join("events.jsonl"), body).unwrap();
    (tmp, dir)
}

const STARTED: &str = r#"{"seq":0,"ts":1,"type":"run_started","playbook":"p","version":"1.0.0"}"#;

fn guard(seq: u64, requires: &[&str]) -> String {
    serde_json::json!({"seq": seq, "ts": 2, "type": "run_guard", "requires": requires}).to_string()
}

/// The `requires` of the run guard right after each checkpoint (`None`
/// where a checkpoint has none), skipping a deferral guard between them.
fn guards_after_checkpoints(events: &[Event]) -> Vec<Option<Vec<String>>> {
    events
        .iter()
        .enumerate()
        .filter(|(_, e)| e.payload.is_checkpoint())
        .map(|(i, _)| {
            events[i + 1..]
                .iter()
                .find(|e| !matches!(e.payload, EventPayload::DeferredRetryGuard { .. }))
                .and_then(|e| match &e.payload {
                    EventPayload::RunGuard { requires } => Some(requires.clone()),
                    _ => None,
                })
        })
        .collect()
}

fn names(v: &[&str]) -> Option<Vec<String>> {
    Some(v.iter().map(|s| s.to_string()).collect())
}

/// A run of a definition using approvals journals its guard right after
/// `run_provenance`, and every checkpoint after it carries the guard in
/// its own write, so the journal never ends on a checkpoint an older apb
/// would continue from.
#[test]
fn a_guarded_definition_journals_its_guard_after_provenance_and_every_checkpoint() {
    let yaml = "schema: 2\nid: gp\nname: gp\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: agent_task, prompt: w }\n  - { id: gate, type: human_review, options: [approve, reject], approves: { results: [w] }, unattended: { decide: approve } }\n  - { id: ship, type: agent_task, prompt: ship, requires_approval: { gate: gate } }\n  - { id: done, type: finish, outcome: success }\n  - { id: no, type: finish, outcome: failure }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: gate }\n  - { from: gate, to: ship, condition: { type: review_status, equals: approve } }\n  - { from: gate, to: no, condition: { type: review_status, equals: reject } }\n  - { from: ship, to: done }\n";
    let _lock = common::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let root = tmp.path();
    seed(root, "gp", yaml, &[]);
    let _env = [
        EnvVar::set("APB_AGENT_CMD", agent(root, "0")),
        EnvVar::set("APB_CONFIG_DIR", cfg.path()),
    ];
    let opts = RunOptions {
        unattended: true,
        ..RunOptions::default()
    };
    let res = run(root, "gp", None, opts).unwrap();
    let events = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();

    let want = names(&["approval_binding", "required_approval"]);
    let provenance = events
        .iter()
        .position(|e| matches!(e.payload, EventPayload::RunProvenance { .. }))
        .unwrap();
    let EventPayload::RunGuard { requires } = &events[provenance + 1].payload else {
        panic!(
            "no run guard after provenance: {:?}",
            events[provenance + 1]
        );
    };
    assert_eq!(Some(requires.clone()), want);
    let after = guards_after_checkpoints(&events);
    assert!(after.len() >= 4, "{after:?}");
    assert!(after.iter().all(|g| *g == want), "{after:?}");
    assert!(matches!(
        events.last().unwrap().payload,
        EventPayload::RunGuard { .. }
    ));
}

/// Two handles append in turn: each folds what the other wrote, so a
/// checkpoint carries the latest guard, and none once a guard ended it.
#[test]
fn every_handle_follows_the_latest_guard_another_handle_wrote() {
    let (_tmp, dir) = journal_dir(&[STARTED, &guard(1, &["person_only"])]);
    let mut a = EventLog::open(&dir).unwrap();
    let mut b = EventLog::open(&dir).unwrap();
    a.append(EventPayload::node_finished("x", "succeeded", 1, ""))
        .unwrap();
    b.append(EventPayload::node_finished("y", "succeeded", 1, ""))
        .unwrap();
    a.append(EventPayload::RunGuard {
        requires: vec!["person_only".into(), "approval_binding".into()],
    })
    .unwrap();
    b.append(EventPayload::node_finished("z", "succeeded", 1, ""))
        .unwrap();
    b.append(EventPayload::RunGuard { requires: vec![] })
        .unwrap();
    a.append(EventPayload::node_finished("w", "succeeded", 1, ""))
        .unwrap();
    let events = read_all(&dir).unwrap();
    assert_eq!(
        guards_after_checkpoints(&events),
        [
            names(&["person_only"]),
            names(&["person_only"]),
            names(&["person_only", "approval_binding"]),
            None,
        ]
    );
    let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, (0..seqs.len() as u64).collect::<Vec<_>>());
}

/// With a deferral open, the checkpoint, its deferral guard and the run
/// guard are three lines with consecutive seqs, in that order.
#[test]
fn the_run_guard_follows_the_deferral_guard_of_the_same_checkpoint() {
    let deferred = r#"{"seq":2,"ts":3,"type":"retry_deferred","node":"a","after_attempt":1,"retry":1,"binding":"main","delay_ms":1000}"#;
    let (_tmp, dir) = journal_dir(&[STARTED, &guard(1, &["person_only"]), deferred]);
    let mut log = EventLog::open(&dir).unwrap();
    log.append(EventPayload::node_finished("b", "succeeded", 1, ""))
        .unwrap();
    let raw = fs::read_to_string(dir.join("events.jsonl")).unwrap();
    let tail: Vec<serde_json::Value> = raw
        .lines()
        .skip(3)
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let shape: Vec<(u64, &str)> = tail
        .iter()
        .map(|v| (v["seq"].as_u64().unwrap(), v["type"].as_str().unwrap()))
        .collect();
    assert_eq!(
        shape,
        [
            (3, "node_finished"),
            (4, "deferred_retry_guard"),
            (5, "run_guard")
        ]
    );
    assert_eq!(tail[2]["requires"], serde_json::json!(["person_only"]));
}

fn refuses_as_newer(err: impl std::fmt::Display) {
    let msg = err.to_string();
    assert!(msg.contains("newer apb"), "{msg}");
}

/// A guard naming a feature this binary does not know (a newer apb's)
/// refuses every reader that continues the run and every writer, while
/// stopping the run goes through.
#[test]
fn a_guard_naming_an_unknown_feature_refuses_all_but_stop() {
    let finished = r#"{"seq":1,"ts":2,"type":"node_finished","node":"a","status":"succeeded","attempt":1,"output":"","artifacts":[]}"#;
    let (tmp, dir) = journal_dir(&[
        STARTED,
        finished,
        &guard(2, &["person_only", "evidence_v2"]),
    ]);
    refuses_as_newer(read_all(&dir).unwrap_err());
    refuses_as_newer(EventLog::open(&dir).err().expect("the open refuses"));
    let journal = read_journal(&dir).unwrap();
    assert_eq!(journal.unknown.len(), 1);
    assert_eq!(journal.unknown[0].seq, 2);

    apb_engine::stop::stop_run(tmp.path(), "r1").unwrap();
    let raw = fs::read_to_string(dir.join("events.jsonl")).unwrap();
    assert!(
        raw.lines().any(|l| l.contains(r#""type":"run_aborted""#)),
        "{raw}"
    );
}

/// A guard whose unknown feature a later checkpoint follows is settled,
/// like any unknown event: the run reads again.
#[test]
fn an_unknown_feature_settled_by_a_later_checkpoint_reads() {
    let finished = r#"{"seq":2,"ts":3,"type":"node_finished","node":"a","status":"succeeded","attempt":1,"output":"","artifacts":[]}"#;
    let (_tmp, dir) = journal_dir(&[STARTED, &guard(1, &["evidence_v2"]), finished]);
    let events = read_all(&dir).unwrap();
    assert_eq!(events.len(), 2);
}
