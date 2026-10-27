//! A run's list nodes in its outcome: the item check as evidence, each item
//! that needs a look as a gap (capped), a list without a check as
//! self-reported, the needs-a-look count of the listing and the goal's
//! `list` criterion before the finish node checked it.

use std::path::Path;

use apb_engine::run_outcome::lists::ITEM_GAP_CAP;
use apb_engine::run_outcome::summary::{EvidenceSource, GapKind, NeedsLookReason};
use apb_engine::run_outcome::{NeedsLook, OutcomeSummary};
use apb_engine::run_view::RunView;
use serde_json::{Value, json};

const PLAYBOOK: &str = r#"
schema: 2
id: lists
name: lists
version: 1.0.0
defaults: { profile: x }
goal:
  statement: Every row is handled
  criteria:
    - { description: every row checked, check: { type: list, node: check_rows } }
nodes:
  - { id: start, type: start }
  - id: check_rows
    type: agent_task
    prompt: Check the row in the item section.
    list:
      items: { inline: [ { id: a }, { id: b } ] }
      item_outputs: [ok]
      check: { output: { field: ok, equals: "1" } }
  - id: note_rows
    type: agent_task
    prompt: Note the row in the item section.
    list:
      items: { inline: [ { id: a }, { id: b } ] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: check_rows }
  - { from: check_rows, to: note_rows }
  - { from: note_rows, to: done }
"#;

fn started(node: &str, total: u32) -> Value {
    json!({ "type": "list_started", "node": node, "total": total, "items_digest": "d",
            "batch_size": 10, "parallel": 1 })
}

fn settled(
    node: &str,
    item: &str,
    state: &str,
    verdict: Option<&str>,
    reason: Option<&str>,
) -> Value {
    json!({ "type": "list_item_settled", "node": node, "item": item, "state": state,
            "verdict": verdict, "reason": reason, "note": reason.map(|_| "no match"), "round": 0 })
}

fn finished(node: &str) -> Value {
    json!({ "type": "node_finished", "node": node, "status": "succeeded", "attempt": 1, "output": "" })
}

fn seed(dir: &Path, events: &[Value]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    let journal: String = events
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let mut e = e.clone();
            e["seq"] = json!(i as u64 + 1);
            e["ts"] = json!(1000 + i as u64);
            format!("{e}\n")
        })
        .collect();
    std::fs::write(dir.join("events.jsonl"), journal).unwrap();
}

fn outcome_of(events: &[Value]) -> OutcomeSummary {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("r1");
    seed(&dir, events);
    let view = RunView::load(&dir, "r1").unwrap();
    OutcomeSummary::build(&view, &dir)
}

/// `check_rows`: a passed, b needs a look; `note_rows`: both passed with no
/// check.
fn run() -> Vec<Value> {
    vec![
        started("check_rows", 2),
        settled("check_rows", "a", "passed", Some("accept_with_note"), None),
        settled("check_rows", "b", "needs_look", None, Some("rejected")),
        finished("check_rows"),
        started("note_rows", 2),
        settled("note_rows", "a", "passed", Some("accept"), None),
        settled("note_rows", "b", "passed", Some("accept"), None),
        finished("note_rows"),
    ]
}

#[test]
fn an_item_check_is_evidence_and_a_list_without_one_is_self_reported() {
    let o = outcome_of(&run());
    let check = o
        .verified
        .iter()
        .find(|e| e.source == EvidenceSource::ItemCheck)
        .expect("the item check is evidence");
    assert_eq!(check.subject, "check_rows");
    assert_eq!(check.result, "failed", "an item needs a look");
    assert_eq!(
        check.detail.as_deref(),
        Some("1 of 2 items passed, output ok equals 1")
    );
    assert_eq!(
        o.verified
            .iter()
            .filter(|e| e.source == EvidenceSource::ItemCheck)
            .count(),
        1,
        "the list without a check gives no evidence"
    );
    let kinds: Vec<(GapKind, &str)> = o
        .unverified
        .iter()
        .map(|g| (g.kind, g.subject.as_str()))
        .collect();
    assert!(kinds.contains(&(GapKind::SelfReportedItems, "note_rows")));
    assert!(kinds.contains(&(GapKind::ItemNeedsLook, "check_rows/b")));
    assert!(
        !kinds.iter().any(|(k, _)| *k == GapKind::SelfReportedNode),
        "a list node is told by its items, not as one self-reported step"
    );
    let look = o.needs_look.as_ref().expect("an item needs a look");
    assert_eq!(look.list_items, 1);
    assert!(look.reasons.contains(&NeedsLookReason::ListItems));
    let lines = o.lines();
    assert_eq!(
        lines[1],
        "List check_rows: 2 processed, 1 passed (1 with a note), 1 needs a look"
    );
    assert_eq!(
        lines[2],
        "List note_rows: 2 processed, 2 passed, 0 need a look"
    );
    assert!(
        lines
            .iter()
            .any(|l| l == "  [item needs a look] check_rows/b: its check rejected it and its re-runs are spent: no match")
    );
}

/// An item whose step failed needs a look, but its check never ruled on
/// it: the check's evidence still reads passed, and the item is a gap.
#[test]
fn a_step_failure_is_a_gap_not_a_failed_check() {
    let o = outcome_of(&[
        started("check_rows", 2),
        settled("check_rows", "a", "passed", Some("accept"), None),
        settled("check_rows", "b", "needs_look", None, Some("failed")),
        finished("check_rows"),
    ]);
    let check = o
        .verified
        .iter()
        .find(|e| e.source == EvidenceSource::ItemCheck)
        .expect("the item check is evidence");
    assert_eq!(check.result, "passed");
    assert!(
        o.unverified
            .iter()
            .any(|g| g.kind == GapKind::ItemNeedsLook && g.subject == "check_rows/b")
    );
}

/// Without a readable playbook snapshot the run's detail still names its
/// lists, from the journal, as the listing counts them.
#[test]
fn a_run_without_its_playbook_snapshot_still_tells_its_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("r1");
    seed(&dir, &run());
    std::fs::remove_file(dir.join("playbook.yaml")).unwrap();
    let view = RunView::load(&dir, "r1").unwrap();
    let o = OutcomeSummary::build(&view, &dir);
    assert_eq!(o.needs_look.as_ref().map(|l| l.list_items), Some(1));
    assert!(
        o.unverified
            .iter()
            .any(|g| g.kind == GapKind::ItemNeedsLook && g.subject == "check_rows/b")
    );
}

#[test]
fn item_gaps_stop_at_the_cap_with_one_line_for_the_rest() {
    let n = ITEM_GAP_CAP + 5;
    let mut events = vec![started("check_rows", n as u32)];
    events.extend((0..n).map(|i| {
        settled(
            "check_rows",
            &format!("i{i:03}"),
            "needs_look",
            None,
            Some("failed"),
        )
    }));
    let o = outcome_of(&events);
    let items: Vec<&str> = o
        .unverified
        .iter()
        .filter(|g| g.kind == GapKind::ItemNeedsLook)
        .map(|g| g.subject.as_str())
        .collect();
    assert_eq!(items.len(), ITEM_GAP_CAP + 1);
    assert_eq!(items.last(), Some(&"and 5 more"));
    assert_eq!(
        o.needs_look.unwrap().list_items,
        n as u32,
        "the count is whole"
    );
}

#[test]
fn the_listing_counts_list_items_without_the_playbook() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("r1");
    seed(&dir, &run());
    let events = apb_engine::event::read_all(&dir).unwrap();
    let look = NeedsLook::from_events(None, &events).expect("an item needs a look");
    assert_eq!(look.list_items, 1);
    assert_eq!(look.line(), "1 list item");
}

#[test]
fn a_list_criterion_not_checked_yet_says_where_its_list_stands() {
    let events = vec![
        started("check_rows", 2),
        settled("check_rows", "a", "passed", Some("accept"), None),
    ];
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("r1");
    seed(&dir, &events);
    let view = RunView::load(&dir, "r1").unwrap();
    let goal = view.goal(&dir).unwrap();
    let c = &goal.criteria[0];
    assert_eq!((c.check.as_str(), c.status.as_str()), ("list", "pending"));
    assert_eq!(
        c.detail.as_deref(),
        Some("list `check_rows`: 1 of 2 processed, 1 passed, 0 need a look")
    );
}
