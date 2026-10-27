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
    assert_eq!(kinds, vec![(9, "future_event"), (19, "future_event")]);
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
        r#"{"ts":1,"type":"future_event"}"#,
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
    assert!(err.contains("future_event"), "{err}");
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
        format!(r#"{{"seq":{seq},"ts":1,"type":"future_event","use":"completion_check"}}"#)
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

/// A newer apb may report usage in a shape this binary cannot decode (a new
/// `source`, a new required field). The usage block is optional data on a
/// known event, so the attempt still reads, without its usage, instead of
/// failing the whole journal.
#[test]
fn an_attempt_usage_block_of_a_newer_shape_does_not_break_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let line = |seq: u64, body: &str| format!(r#"{{"seq":{seq},"ts":1,{body}}}"#);
    let journal = [
        line(0, r#""type":"run_started","playbook":"p","version":"1.0.0""#),
        line(
            1,
            r#""type":"attempt_finished","node":"w","attempt":1,"status":"succeeded","usage":{"input_tokens":1,"output_tokens":2,"source":"measured"}"#,
        ),
        line(
            2,
            r#""type":"node_finished","node":"w","status":"succeeded","attempt":1,"output":"ok""#,
        ),
    ]
    .join("\n");
    std::fs::write(dir.path().join("events.jsonl"), journal + "\n").unwrap();

    let read = apb_engine::event::read_journal(dir.path()).expect("journal reads");
    assert!(read.unknown.is_empty());
    let usage = read.events.iter().find_map(|e| match &e.payload {
        apb_engine::event::EventPayload::AttemptFinished { usage, .. } => Some(usage.clone()),
        _ => None,
    });
    assert_eq!(usage, Some(None));
    apb_engine::event::read_all(dir.path()).expect("the engine reads it too");
}

/// The same for the output loss list: an entry shape this binary cannot
/// decode reads as no recorded loss, and the attempt still reads.
#[test]
fn an_output_loss_list_of_a_newer_shape_does_not_break_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let line = |seq: u64, body: &str| format!(r#"{{"seq":{seq},"ts":1,{body}}}"#);
    let journal = [
        line(0, r#""type":"run_started","playbook":"p","version":"1.0.0""#),
        line(
            1,
            r#""type":"attempt_finished","node":"w","attempt":1,"status":"failed","output_truncated":{"streams":[{"stream":"stdout","kept_bytes":"1 MiB"}]},"transcript_incomplete":true"#,
        ),
        line(
            2,
            r#""type":"node_finished","node":"w","status":"failed","attempt":1,"output":"""#,
        ),
    ]
    .join("\n");
    std::fs::write(dir.path().join("events.jsonl"), journal + "\n").unwrap();

    let read = apb_engine::event::read_journal(dir.path()).expect("journal reads");
    assert!(read.unknown.is_empty());
    let attempt = read.events.iter().find_map(|e| match &e.payload {
        apb_engine::event::EventPayload::AttemptFinished {
            output_truncated,
            transcript_incomplete,
            ..
        } => Some((output_truncated.len(), *transcript_incomplete)),
        _ => None,
    });
    assert_eq!(attempt, Some((0, true)));
    apb_engine::event::read_all(dir.path()).expect("the engine reads it too");
}

/// Stopping never re-drives a run and only appends `run_aborted`, itself a
/// checkpoint, so an older apb can stop a run whose journal holds an unknown
/// event after the last checkpoint. Resuming the same journal still refuses.
#[test]
fn stop_goes_through_an_unknown_event_newer_than_the_last_checkpoint() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path());
    let res = run(tmp.path(), "lin", None, RunOptions::default()).unwrap();
    crash_after_a_with_future_event(tmp.path(), &res.run_id, true);
    let dir = tmp.path().join(".apb/runs").join(&res.run_id);

    let err = resume(tmp.path(), &res.run_id, None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("newer apb"), "{err}");

    let outcome = apb_engine::stop_run(tmp.path(), &res.run_id).unwrap();
    assert_eq!(outcome, apb_engine::StopOutcome::FinalizedDeadRun);
    let journal = read_journal(&dir).unwrap();
    assert_eq!(journal.unknown.len(), 1, "the unknown event stays on disk");
    let last = journal.events.last().unwrap();
    assert!(
        matches!(last.payload, EventPayload::RunAborted { .. }),
        "{last:?}"
    );
    assert!(last.seq > journal.unknown[0].seq, "the abort follows it");
    assert_eq!(
        RunView::load(&dir, &res.run_id).unwrap().state.run_status,
        RunStatus::Aborted
    );
}

#[test]
fn old_pause_and_resume_lines_decode_with_the_new_fields_absent() {
    // Issue #200 added `by`, `mode`, `until` and `keep_lock` to `run_paused`
    // and `by` to `run_resumed`; a journal written before them still reads.
    let paused: Event = serde_json::from_str(
        r#"{"seq":3,"ts":10,"type":"run_paused","reason":"supervisor pause"}"#,
    )
    .unwrap();
    assert!(matches!(
        paused.payload,
        EventPayload::RunPaused { ref reason, by: None, mode: None, until: None, keep_lock: false }
            if reason == "supervisor pause"
    ));
    let resumed: Event =
        serde_json::from_str(r#"{"seq":4,"ts":20,"type":"run_resumed","from_node":"a"}"#).unwrap();
    assert!(matches!(
        resumed.payload,
        EventPayload::RunResumed { ref from_node, by: None, .. } if from_node == "a"
    ));
    // And an event without the new fields writes the old shape back.
    assert_eq!(
        serde_json::to_string(&paused).unwrap(),
        r#"{"seq":3,"ts":10,"type":"run_paused","reason":"supervisor pause"}"#
    );
}

#[test]
fn new_pause_fields_round_trip() {
    use apb_engine::pause::{PauseBy, PauseMode};
    let line = r#"{"seq":3,"ts":10,"type":"run_paused","reason":"lunch","by":"operator","mode":"now","until":5000,"keep_lock":true}"#;
    let e: Event = serde_json::from_str(line).unwrap();
    assert!(matches!(
        e.payload,
        EventPayload::RunPaused {
            by: Some(PauseBy::Operator),
            mode: Some(PauseMode::Now),
            until: Some(5000),
            keep_lock: true,
            ..
        }
    ));
    assert_eq!(serde_json::to_string(&e).unwrap(), line);
    let line = r#"{"seq":4,"ts":20,"type":"run_resumed","from_node":"a","by":"engine"}"#;
    let e: Event = serde_json::from_str(line).unwrap();
    assert_eq!(serde_json::to_string(&e).unwrap(), line);
}

/// The completeness fields added to known events read as absent from old
/// lines and write the old shape back when unset, so a journal without them
/// is byte-for-byte what it was.
#[test]
fn old_connector_call_and_attempt_lines_keep_their_shape() {
    let lines = [
        r#"{"seq":5,"ts":9,"type":"connector_call","node_id":"n","connector":"c","function":"f","account":"a","url":"u","outcome":"ok","http_status":200,"duration_ms":3,"smtp_subject":null,"smtp_recipients":null}"#,
        r#"{"seq":6,"ts":10,"type":"attempt_finished","node":"n","attempt":1,"status":"succeeded","duration_ms":null,"session":null,"summary":null}"#,
    ];
    for line in lines {
        let e: Event = serde_json::from_str(line).unwrap();
        assert_eq!(serde_json::to_string(&e).unwrap(), line);
    }
    let call: Event = serde_json::from_str(lines[0]).unwrap();
    assert!(matches!(
        call.payload,
        EventPayload::ConnectorCall {
            response_complete: None,
            ..
        }
    ));
    let cut = lines[0].replace("}", r#","response_complete":false}"#);
    let e: Event = serde_json::from_str(&cut).unwrap();
    assert!(matches!(
        e.payload,
        EventPayload::ConnectorCall {
            response_complete: Some(false),
            ..
        }
    ));
}

/// `inbox_gap` is a reporting record: it round trips, changes no run state,
/// and a binary that does not know it skips it because the node that made the
/// read still ends with its own `node_finished` checkpoint.
#[test]
fn inbox_gap_round_trips_and_is_skippable_before_a_checkpoint() {
    let line = r#"{"seq":2,"ts":3,"type":"inbox_gap","node_id":"a","connector":"c","account":"main","from":3,"through":6}"#;
    let e: Event = serde_json::from_str(line).unwrap();
    assert_eq!(serde_json::to_string(&e).unwrap(), line);
    assert!(!e.payload.is_checkpoint());

    // The same journal as an older binary sees it: the type is unknown, and a
    // checkpoint follows it, so the engine read still succeeds.
    let tmp = tempfile::tempdir().unwrap();
    let journal = [
        r#"{"seq":0,"ts":1,"type":"run_started","playbook":"p","version":"1.0.0"}"#.to_string(),
        r#"{"seq":1,"ts":2,"type":"node_started","node":"a","attempt":1}"#.to_string(),
        line.replace("inbox_gap", "inbox_gap_from_the_future"),
        r#"{"seq":3,"ts":4,"type":"node_finished","node":"a","status":"succeeded","attempt":1,"output":"","artifacts":[]}"#.to_string(),
        String::new(),
    ]
    .join("\n");
    let dir = write_run(tmp.path(), "r1", &journal);
    let events = read_all(&dir).unwrap();
    assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), [0, 1, 3]);
}

/// The send ledger's two records round trip, change no run state, and an
/// older binary that does not know them skips them while a checkpoint
/// follows, and refuses to drive on when none does (the general rule).
#[test]
fn send_intent_records_round_trip_and_are_skippable_before_a_checkpoint() {
    let recorded = r#"{"seq":2,"ts":3,"type":"send_intent_recorded","intent":"si-1","nonce":"n","node":"a","attempt":1,"attempt_seq":1,"connector":"c","function":"f","account":"main","mode":"key","label":"x","pid":7,"deadline_ms":9,"replay_of":"si-0"}"#;
    let settled = r#"{"seq":3,"ts":4,"type":"send_intent_settled","intent":"si-1","outcome":"confirmed","source":"key_replay","effect_ref":"m-1"}"#;
    for line in [recorded, settled] {
        let e: Event = serde_json::from_str(line).unwrap();
        assert_eq!(serde_json::to_string(&e).unwrap(), line);
        assert!(!e.payload.is_checkpoint());
    }
    let head = [
        r#"{"seq":0,"ts":1,"type":"run_started","playbook":"p","version":"1.0.0"}"#.to_string(),
        r#"{"seq":1,"ts":2,"type":"node_started","node":"a","attempt":1}"#.to_string(),
        recorded.replace("send_intent_recorded", "send_intent_from_the_future"),
    ];
    let finished = r#"{"seq":3,"ts":4,"type":"node_finished","node":"a","status":"succeeded","attempt":1,"output":"","artifacts":[]}"#;
    let tmp = tempfile::tempdir().unwrap();
    let settled_run = [&head[..], &[finished.to_string(), String::new()]].concat();
    let dir = write_run(tmp.path(), "r1", &settled_run.join("\n"));
    let events = read_all(&dir).unwrap();
    assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), [0, 1, 3]);

    let open_run = [&head[..], &[String::new()]].concat();
    let dir = write_run(tmp.path(), "r2", &open_run.join("\n"));
    assert!(
        read_all(&dir).is_err(),
        "an unsettled unknown event refuses"
    );
}

/// The attempt and intent fields of a connector call read as absent from an
/// old line and stay off the wire when unset.
#[test]
fn a_connector_call_carries_its_attempt_and_intent_only_when_set() {
    let old = r#"{"seq":5,"ts":9,"type":"connector_call","node_id":"n","connector":"c","function":"f","account":"a","url":"u","outcome":"ok","http_status":200,"duration_ms":3,"smtp_subject":null,"smtp_recipients":null}"#;
    let e: Event = serde_json::from_str(old).unwrap();
    assert!(matches!(
        e.payload,
        EventPayload::ConnectorCall {
            attempt: None,
            attempt_seq: None,
            intent: None,
            origin: None,
            ..
        }
    ));
    let new = old.replace(
        "}",
        r#","attempt":2,"attempt_seq":11,"intent":"si-1","origin":"reconcile"}"#,
    );
    let e: Event = serde_json::from_str(&new).unwrap();
    assert_eq!(serde_json::to_string(&e).unwrap(), new);
}

/// A person's decision lands after the run paused: past the last
/// checkpoint. An older binary that does not know the event refuses to
/// drive the run on, while a reading surface still shows every event.
#[test]
fn a_settlement_after_a_pause_is_past_the_last_checkpoint() {
    let lines = [
        r#"{"seq":0,"ts":1,"type":"run_started","playbook":"p","version":"1.0.0"}"#,
        r#"{"seq":1,"ts":2,"type":"run_paused","reason":"lunch","by":"operator","mode":"drain"}"#,
        r#"{"seq":2,"ts":3,"type":"send_intent_settled","intent":"si-1","outcome":"not_sent","source":"person","by":"ana"}"#,
    ];
    let tmp = tempfile::tempdir().unwrap();
    let paused = write_run(tmp.path(), "r1", &(lines.join("\n") + "\n"));
    let events = read_all(&paused).unwrap();
    assert!(
        events[1].payload.is_checkpoint(),
        "the pause is the checkpoint"
    );
    assert!(matches!(
        events[2].payload,
        EventPayload::SendIntentSettled { .. }
    ));

    let older = lines.map(|l| l.replace("send_intent_settled", "send_intent_from_the_future"));
    let dir = write_run(tmp.path(), "r2", &(older.join("\n") + "\n"));
    assert!(read_all(&dir).is_err(), "an older apb does not drive on");
    assert_eq!(
        apb_engine::event::read_all_lossy_tail(&dir).unwrap().len(),
        2,
        "a reading surface skips it"
    );
}

/// `notify_set` round trips, changes no run state, and an older binary that
/// does not know it skips it while a checkpoint follows: such a binary then
/// simply sends no notifications.
#[test]
fn notify_set_round_trips_and_is_skippable_before_a_checkpoint() {
    let line = r#"{"seq":1,"ts":2,"type":"notify_set","target":{"connector":"telegram","account":"me","function":"send_message","to":"42","connector_digest":"sha256:c","account_digest":"sha256:a"},"on":["review","finished"],"source":"run"}"#;
    let e: Event = serde_json::from_str(line).unwrap();
    assert_eq!(serde_json::to_string(&e).unwrap(), line);
    assert!(!e.payload.is_checkpoint());
    // A shape a newer apb wrote reads leniently instead of failing.
    let newer = r#"{"seq":1,"ts":2,"type":"notify_set","target":"elsewhere","on":["review","stalled"],"source":"run","by":"robot"}"#;
    assert!(matches!(
        serde_json::from_str::<Event>(newer).unwrap().payload,
        EventPayload::NotifySet {
            target: None,
            by: None,
            ..
        }
    ));

    let tmp = tempfile::tempdir().unwrap();
    let journal = [
        r#"{"seq":0,"ts":1,"type":"run_started","playbook":"p","version":"1.0.0"}"#.to_string(),
        line.replace("notify_set", "notify_set_from_the_future"),
        r#"{"seq":2,"ts":3,"type":"node_started","node":"a","attempt":1}"#.to_string(),
        r#"{"seq":3,"ts":4,"type":"node_finished","node":"a","status":"succeeded","attempt":1,"output":"","artifacts":[]}"#.to_string(),
        String::new(),
    ]
    .join("\n");
    let dir = write_run(tmp.path(), "r1", &journal);
    let events = read_all(&dir).unwrap();
    assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), [0, 2, 3]);
}

/// Who stopped a run reads as absent from an old `run_aborted` line and
/// stays off the wire when unset.
#[test]
fn run_aborted_carries_who_stopped_it_only_when_known() {
    let old = r#"{"seq":4,"ts":5,"type":"run_aborted","reason":"stop requested"}"#;
    let e: Event = serde_json::from_str(old).unwrap();
    assert!(matches!(
        e.payload,
        EventPayload::RunAborted { by: None, .. }
    ));
    assert_eq!(serde_json::to_string(&e).unwrap(), old);
    let new = r#"{"seq":4,"ts":5,"type":"run_aborted","reason":"stop requested","by":"operator"}"#;
    let e: Event = serde_json::from_str(new).unwrap();
    assert_eq!(serde_json::to_string(&e).unwrap(), new);
}

/// The run quality events round-trip, are no checkpoints, and an older
/// binary that does not know them skips them: each is followed by
/// `run_finished`. `candidate_promoted` reads without its `basis`.
#[test]
fn quality_events_round_trip_and_are_skippable_before_run_finished() {
    let graded = r#"{"seq":3,"ts":4,"type":"run_graded","grade_id":"rules-1-4-abcd1234","grader":"rules/1","levels":{"loops":"good","outcome":"good"}}"#;
    let held = r#"{"seq":4,"ts":5,"type":"candidate_held","version":"1.0.1","run_id":"r1","verdict":"wait","reason":"not enough comparable grades yet"}"#;
    for line in [graded, held] {
        let e: Event = serde_json::from_str(line).unwrap();
        assert_eq!(serde_json::to_string(&e).unwrap(), line);
        assert!(!e.payload.is_checkpoint());
    }
    let old = r#"{"seq":4,"ts":5,"type":"candidate_promoted","version":"1.0.1","run_id":"r1","successes":2}"#;
    let e: Event = serde_json::from_str(old).unwrap();
    assert!(matches!(
        e.payload,
        EventPayload::CandidatePromoted { basis: None, .. }
    ));
    assert_eq!(serde_json::to_string(&e).unwrap(), old);

    let tmp = tempfile::tempdir().unwrap();
    let journal = [
        r#"{"seq":0,"ts":1,"type":"run_started","playbook":"p","version":"1.0.0"}"#.to_string(),
        r#"{"seq":1,"ts":2,"type":"node_started","node":"a","attempt":1}"#.to_string(),
        r#"{"seq":2,"ts":3,"type":"node_finished","node":"a","status":"succeeded","attempt":1,"output":"","artifacts":[]}"#.to_string(),
        graded.replace("run_graded", "run_graded_from_the_future"),
        held.replace("candidate_held", "candidate_held_from_the_future"),
        r#"{"seq":5,"ts":6,"type":"run_finished","outcome":"succeeded"}"#.to_string(),
        String::new(),
    ]
    .join("\n");
    let dir = write_run(tmp.path(), "r1", &journal);
    let events = read_all(&dir).unwrap();
    assert_eq!(
        events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        [0, 1, 2, 5]
    );
}
