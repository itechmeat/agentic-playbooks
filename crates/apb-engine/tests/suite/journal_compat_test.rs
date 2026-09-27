//! Forward-compatible journal reading: an event type this binary does not
//! know (one a newer apb wrote) is skipped and counted, never a read error,
//! and the known events keep their exact wire format.

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventLog, EventPayload, read_all, read_journal};
use apb_engine::run_view::RunView;
use apb_engine::scheduler::{RunOptions, list_runs, resume, run};
use apb_engine::state::RunStatus;
use std::fs;
use std::path::Path;

/// A journal written by this version's serializer: every line is exactly
/// what `EventLog::append` produces for its event.
const KNOWN: &str = include_str!("../fixtures/journal/known_events.jsonl");

/// The same run with two events of a type no apb knows yet, one before the
/// first `node_finished` and one before the last.
const WITH_FUTURE: &str = include_str!("../fixtures/journal/with_future_events.jsonl");

fn write_run(root: &Path, run_id: &str, journal: &str) -> std::path::PathBuf {
    let dir = root.join(".apb/runs").join(run_id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("events.jsonl"), journal).unwrap();
    dir
}

#[test]
fn a_journal_without_unknown_events_round_trips_byte_for_byte() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = write_run(tmp.path(), "r1", KNOWN);
    let events = read_all(&dir).unwrap();
    let rewritten: String = events
        .iter()
        .map(|e| serde_json::to_string(e).unwrap() + "\n")
        .collect();
    assert_eq!(rewritten, KNOWN);
}

#[test]
fn unknown_event_types_are_skipped_and_counted() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = write_run(tmp.path(), "r1", WITH_FUTURE);
    let journal = read_journal(&dir).unwrap();
    let kinds: Vec<(u64, &str)> = journal
        .unknown
        .iter()
        .map(|u| (u.seq, u.kind.as_str()))
        .collect();
    assert_eq!(kinds, vec![(9, "decision_made"), (19, "decision_made")]);
    // Every known line is still there, in order, with its own seq.
    let known: Vec<u64> = journal.events.iter().map(|e| e.seq).collect();
    let expected: Vec<u64> = (0..=22).filter(|s| *s != 9 && *s != 19).collect();
    assert_eq!(known, expected);
}

#[test]
fn a_known_event_with_a_broken_body_is_still_an_error() {
    // Skipping covers event TYPES this binary does not know. A known type
    // whose fields do not parse is corruption and stays a read error, as
    // does a line that is not an event at all.
    let tmp = tempfile::tempdir().unwrap();
    for bad in [
        r#"{"seq":0,"ts":1,"type":"node_started","node":"a","attempt":"one"}"#,
        r#"{"seq":0,"ts":1,"type":"node_started"}"#,
        r#"{"seq":0,"ts":1}"#,
        r#"{"ts":1,"type":"decision_made"}"#,
        // Not the shape of an event tag: corruption, never a newer type
        // (and never echoed into a terminal message).
        r#"{"seq":0,"ts":1,"type":"Decision\u001b[2J"}"#,
    ] {
        let dir = write_run(tmp.path(), "bad", &format!("{bad}\n{bad}\n"));
        assert!(read_journal(&dir).is_err(), "{bad} must not read");
        assert!(read_all(&dir).is_err(), "{bad} must not read strictly");
    }
}

#[test]
fn read_only_surfaces_show_a_run_with_unknown_events() {
    let tmp = tempfile::tempdir().unwrap();
    init_project(tmp.path()).unwrap();
    let dir = write_run(tmp.path(), "demo-1", WITH_FUTURE);

    let view = RunView::load(&dir, "demo-1").unwrap();
    assert_eq!(view.run_status, RunStatus::Succeeded);
    assert_eq!(view.unknown.len(), 2);
    assert_eq!(view.state.outputs["review"], "approve");

    let runs = list_runs(tmp.path()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "succeeded");
    assert_eq!(runs[0].unknown_events, 2);
}

#[test]
fn an_unknown_event_after_the_last_checkpoint_stops_the_engine() {
    let tmp = tempfile::tempdir().unwrap();
    let cut: String = WITH_FUTURE
        .lines()
        .take(20)
        .map(|l| format!("{l}\n"))
        .collect();
    // Seqs 0..=19: the second future event (seq 19) is now newer than the
    // last checkpoint (`run_paused` at seq 13).
    let dir = write_run(tmp.path(), "r1", &cut);
    let err = read_all(&dir).unwrap_err().to_string();
    assert!(err.contains("decision_made"), "{err}");
    assert!(err.contains("newer apb"), "{err}");
    assert!(err.contains(env!("CARGO_PKG_VERSION")), "{err}");
    // Appending would write into a run this binary cannot fold: refused too.
    assert!(EventLog::open(&dir).is_err());
    // A read-only surface still shows it.
    let view = RunView::load(&dir, "r1").unwrap();
    assert_eq!(view.unknown.len(), 2);
}

// --- resume over a real run ---

const PLAYBOOK: &str = r#"
schema: 1
id: lin
name: Lin
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "x" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: done }
"#;

fn seed(root: &Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/lin/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/lin/current"), "1.0.0").unwrap();
}

/// Cuts a finished run's journal right after node `a` finished (a crash
/// before the run reached `done`) and inserts a future event, either just
/// before that `node_finished` or after it.
fn crash_after_a_with_future_event(root: &Path, run_id: &str, after_checkpoint: bool) {
    let dir = root.join(".apb/runs").join(run_id);
    let events = read_all(&dir).unwrap();
    let cut = events
        .iter()
        .position(|e| matches!(&e.payload, EventPayload::NodeFinished { node, .. } if node == "a"))
        .expect("node a finished");
    let future = |seq: u64| {
        format!(r#"{{"seq":{seq},"ts":1,"type":"decision_made","use":"completion_check"}}"#)
    };
    let line = |e: &Event| serde_json::to_string(e).unwrap();
    let mut out: Vec<String> = Vec::new();
    for e in &events[..cut] {
        out.push(line(e));
    }
    let finished = events[cut].clone();
    let last_seq = finished.seq;
    if after_checkpoint {
        out.push(line(&finished));
        out.push(future(last_seq + 1));
    } else {
        // Give the future event the checkpoint's seq and move the checkpoint
        // one up, so seqs stay strictly increasing.
        out.push(future(last_seq));
        out.push(line(&Event {
            seq: last_seq + 1,
            ..finished
        }));
    }
    fs::write(dir.join("events.jsonl"), out.join("\n") + "\n").unwrap();
}

#[test]
fn resume_continues_past_unknown_events_older_than_the_last_checkpoint() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path());
    let res = run(tmp.path(), "lin", None, RunOptions::default()).unwrap();
    crash_after_a_with_future_event(tmp.path(), &res.run_id, false);

    let again = resume(tmp.path(), &res.run_id, None).unwrap();
    assert_eq!(again.outcome, RunStatus::Succeeded);
    // The skipped event is still on disk, untouched, and the seqs the resume
    // appended come after it.
    let journal = read_journal(&tmp.path().join(".apb/runs").join(&res.run_id)).unwrap();
    assert_eq!(journal.unknown.len(), 1);
    let unknown_seq = journal.unknown[0].seq;
    assert!(journal.events.iter().all(|e| e.seq != unknown_seq));
}

#[test]
fn resume_refuses_an_unknown_event_newer_than_the_last_checkpoint() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path());
    let res = run(tmp.path(), "lin", None, RunOptions::default()).unwrap();
    crash_after_a_with_future_event(tmp.path(), &res.run_id, true);
    let dir = tmp.path().join(".apb/runs").join(&res.run_id);
    let before = fs::read(dir.join("events.jsonl")).unwrap();

    let err = resume(tmp.path(), &res.run_id, None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("newer apb"), "{err}");
    // Refused before anything was written.
    assert_eq!(fs::read(dir.join("events.jsonl")).unwrap(), before);
}
