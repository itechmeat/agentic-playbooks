//! Pure run-progress fold (spec 2026-07-17): a percent computed from persisted
//! events plus the playbook version bound to the run, exactly like
//! `RunState::fold`. No mutable state, so resume and a server restart show the
//! same number.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use apb_core::schema::{NodeKind, Playbook};
use serde::Serialize;

use crate::event::{Event, EventPayload};
use crate::state::{NodeStatus, RunState, RunStatus};

/// The kind of node a run is waiting on. Serializes to the same strings the web
/// badge matches on (`"human_review"` / `"wait"`); the enum gives the fold
/// compile-time exhaustiveness instead of a free-form string.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitingKind {
    HumanReview,
    Wait,
}

/// The run-progress summary surfaced by the server and MCP `run_status`.
#[derive(Debug, Clone, Serialize)]
pub struct ProgressSummary {
    pub percent: u8,
    pub label: Option<String>,
    pub waiting_on: Option<String>,
    /// The kind of node the run is waiting on when `waiting_on` is set.
    /// `None` whenever `waiting_on` is `None`. Feeds the web badge copy
    /// (spec section 6 wants distinct texts).
    pub waiting_kind: Option<WaitingKind>,
    /// Deterministic identity of the work plan behind this percent (spec
    /// section 3): the playbook version bound to the run plus the latest
    /// reported `total` of each cyclic group. It changes exactly when a report
    /// raises or lowers a group's total, or the run migrates to a patched
    /// version; it does NOT change on ordinary `done`/`label` updates. The web
    /// uses it as the monotonicity reset signal, never the display `label`.
    pub plan_key: String,
}

/// Strongly-connected components of the playbook graph (iterative Tarjan),
/// returned as groups of node ids. Node order follows `playbook.nodes`.
fn sccs(playbook: &Playbook) -> Vec<Vec<String>> {
    let ids: Vec<&str> = playbook.nodes.iter().map(|n| n.id.as_str()).collect();
    let index_of: BTreeMap<&str, usize> = ids.iter().enumerate().map(|(i, s)| (*s, i)).collect();
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); ids.len()];
    for e in &playbook.edges {
        if let (Some(&a), Some(&b)) = (index_of.get(e.from.as_str()), index_of.get(e.to.as_str())) {
            adj[a].push(b);
        }
    }
    let n = ids.len();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut counter = 0usize;
    let mut out: Vec<Vec<String>> = Vec::new();
    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        let mut call: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(&(v, ei)) = call.last() {
            if ei == 0 {
                index[v] = counter;
                low[v] = counter;
                counter += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if ei < adj[v].len() {
                call.last_mut().expect("frame exists").1 += 1;
                let w = adj[v][ei];
                if index[w] == usize::MAX {
                    call.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                if low[v] == index[v] {
                    let mut comp = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        comp.push(ids[w].to_string());
                        if w == v {
                            break;
                        }
                    }
                    out.push(comp);
                }
                call.pop();
                if let Some(&(parent, _)) = call.last() {
                    low[parent] = low[parent].min(low[v]);
                }
            }
        }
    }
    out
}

/// Loads the run's immutable playbook snapshot from `<run_dir>/playbook.yaml`.
/// Returns `None` when the snapshot is missing or fails to parse.
///
/// Parsing goes through the shared run-snapshot compatibility parser
/// (`legacy_snapshot::parse_snapshot_playbook`), so schema-1 snapshots that the
/// strict `Playbook::from_yaml` rejects still yield progress (the same
/// tolerance the resume path uses), without weakening the strict parser for
/// live definitions.
///
/// F3: a missing snapshot is a silent `None` (very old runs never captured
/// one). An existing-but-unparseable snapshot also collapses to `None`, but is
/// not silent: it writes one stderr warning naming the run dir. apb-engine has
/// no tracing facility and we add no dependency for this one branch; snapshots
/// are immutable and engine-written, so a parse failure is a filesystem-level
/// fault worth a terminal signal rather than an authoring one.
pub fn load_run_playbook(run_dir: &Path) -> Option<Playbook> {
    let yaml = std::fs::read_to_string(run_dir.join("playbook.yaml")).ok()?;
    match crate::legacy_snapshot::parse_snapshot_playbook(&yaml) {
        Ok(pb) => Some(pb),
        Err(e) => {
            eprintln!(
                "apb: warning: run snapshot {} unparseable: {e}",
                run_dir.display()
            );
            None
        }
    }
}

/// Computes the progress summary for a run directory, or `None` when the
/// playbook snapshot is missing or unparseable. The rule "missing or
/// unparseable snapshot means no progress" lives here and only here.
pub fn from_run_dir(run_dir: &Path, events: &[Event]) -> Option<ProgressSummary> {
    load_run_playbook(run_dir).map(|pb| compute(&pb, events))
}

/// Computes the progress summary from events + the run's playbook version.
pub fn compute(playbook: &Playbook, events: &[Event]) -> ProgressSummary {
    let state = RunState::fold(events);
    let status = |id: &str| state.nodes.get(id).copied().unwrap_or(NodeStatus::Pending);
    let counted = |id: &str| !matches!(status(id), NodeStatus::Skipped | NodeStatus::Cancelled);

    // Nodes that have EVER reached a successful `NodeFinished` in the event
    // history. A cyclic group without a report credits one pass off this set,
    // not the CURRENT status: a back edge re-runs a node (NodeStarted flips it
    // to Running), and keying on the live status would drop the already-earned
    // pass and roll the percent back on loop re-entry (B1).
    let ever_succeeded: BTreeSet<&str> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, status, .. }
                if NodeStatus::from_label(status) == NodeStatus::Succeeded =>
            {
                Some(node.as_str())
            }
            _ => None,
        })
        .collect();

    let groups = sccs(playbook);
    let mut group_of: BTreeMap<&str, usize> = BTreeMap::new();
    for (gi, g) in groups.iter().enumerate() {
        for id in g {
            group_of.insert(id.as_str(), gi);
        }
    }
    let self_loops: BTreeSet<&str> = playbook
        .edges
        .iter()
        .filter(|e| e.from == e.to)
        .map(|e| e.from.as_str())
        .collect();
    let cyclic: Vec<bool> = groups
        .iter()
        .map(|g| g.len() > 1 || (g.len() == 1 && self_loops.contains(g[0].as_str())))
        .collect();

    // Latest report per group and the latest label overall.
    let mut report_of: BTreeMap<usize, (u64, u64)> = BTreeMap::new();
    let mut label: Option<String> = None;
    for e in events {
        if let EventPayload::RunProgress {
            node_id,
            done,
            total,
            label: lbl,
        } = &e.payload
        {
            if let Some(&gi) = group_of.get(node_id.as_str()) {
                let total_c = if *total == 0 { 1 } else { *total };
                let done_c = (*done).min(total_c);
                report_of.insert(gi, (done_c, total_c));
            }
            if lbl.is_some() {
                label = lbl.clone();
            }
        }
    }

    let mut total: u128 = 0;
    let mut done: u128 = 0;
    for (gi, g) in groups.iter().enumerate() {
        let one_pass: u64 = g
            .iter()
            .filter(|id| counted(id))
            .filter_map(|id| playbook.node(id))
            .map(|n| n.expected_seconds())
            .sum();
        if one_pass == 0 {
            continue;
        }
        if cyclic[gi] {
            if let Some(&(d, t)) = report_of.get(&gi) {
                total += (t as u128) * one_pass as u128;
                done += (d as u128) * one_pass as u128;
            } else {
                total += one_pass as u128;
                // One-pass credit that survives loop re-entry: a node counts as
                // done once it has ever succeeded, regardless of its current
                // status, capped at a single pass (B1).
                let succ: u64 = g
                    .iter()
                    .filter(|id| counted(id) && ever_succeeded.contains(id.as_str()))
                    .filter_map(|id| playbook.node(id))
                    .map(|n| n.expected_seconds())
                    .sum();
                done += succ.min(one_pass) as u128;
            }
        } else {
            let id = &g[0];
            if let Some(n) = playbook.node(id) {
                let w = n.expected_seconds() as u128;
                total += w;
                if status(id) == NodeStatus::Succeeded {
                    done += w;
                }
            }
        }
    }

    let mut percent: u8 = done
        .saturating_mul(100)
        .checked_div(total)
        .unwrap_or(0)
        .min(100) as u8;
    if matches!(state.run_status, RunStatus::Succeeded) {
        percent = 100;
    }

    // A human_review node is "waiting" when its ReviewRequested count exceeds
    // its ReviewDecided count: the scheduler declares the request and spins
    // while the node is still Pending, emitting NodeStarted (Running) only once
    // the decision arrives. Keying on Running would miss that whole window (and
    // would linger while Running after the decision), so count the events here.
    // A wait node does reach Running while it blocks, so it keeps that check.
    let review_pending = |id: &str| {
        let requested = events
            .iter()
            .filter(
                |e| matches!(&e.payload, EventPayload::ReviewRequested { node, .. } if node == id),
            )
            .count();
        let decided = events
            .iter()
            .filter(
                |e| matches!(&e.payload, EventPayload::ReviewDecided { node, .. } if node == id),
            )
            .count();
        requested > decided
    };
    let waiting = playbook.nodes.iter().find_map(|n| match &n.kind {
        NodeKind::HumanReview { .. } if review_pending(&n.id) => {
            Some((n.id.clone(), WaitingKind::HumanReview))
        }
        NodeKind::Wait { .. } if status(&n.id) == NodeStatus::Running => {
            Some((n.id.clone(), WaitingKind::Wait))
        }
        _ => None,
    });
    let waiting_on = waiting.as_ref().map(|(id, _)| id.clone());
    let waiting_kind = waiting.map(|(_, kind)| kind);

    // Plan identity (B4): the run's playbook version plus the latest reported
    // total of each cyclic group, entries ordered by the stable SCC group
    // index. This is what actually defines the amount of work, so it changes
    // only when a report moves a group's total or the run migrates to a patched
    // version - never on a plain done/label update.
    let mut plan_parts: Vec<String> = Vec::new();
    for (gi, _) in groups.iter().enumerate() {
        if cyclic[gi]
            && let Some(&(_, t)) = report_of.get(&gi)
        {
            plan_parts.push(format!("g{gi}={t}"));
        }
    }
    let plan_key = format!("{}|{}", playbook.version, plan_parts.join(","));

    ProgressSummary {
        percent,
        label,
        waiting_on,
        waiting_kind,
        plan_key,
    }
}

/// The run answer (spec B): the non-empty output of the succeeded finish node.
/// Derived purely by fold from the run's events + snapshot; `None` when the
/// finish had no prompt (empty output), the finish has not run, or the snapshot
/// is missing. Multiple finish nodes: the first with a non-empty output wins.
pub fn run_answer(run_dir: &Path, events: &[Event]) -> Option<String> {
    let pb = load_run_playbook(run_dir)?;
    let state = RunState::fold(events);
    for n in &pb.nodes {
        if matches!(n.kind, NodeKind::Finish { .. })
            && state.nodes.get(&n.id).copied() == Some(NodeStatus::Succeeded)
            && let Some(out) = state.outputs.get(&n.id)
            && !out.is_empty()
        {
            return Some(out.clone());
        }
    }
    None
}

/// Measured wall time per node in whole seconds, from NodeStarted to
/// NodeFinished; the last completion of a node wins (loops overwrite).
pub fn node_durations_seconds(events: &[Event]) -> BTreeMap<String, u64> {
    let mut start: BTreeMap<String, u128> = BTreeMap::new();
    let mut out: BTreeMap<String, u64> = BTreeMap::new();
    for e in events {
        match &e.payload {
            EventPayload::NodeStarted { node, .. } => {
                start.insert(node.clone(), e.ts);
            }
            EventPayload::NodeFinished { node, .. } => {
                if let Some(s) = start.get(node) {
                    out.insert(node.clone(), (e.ts.saturating_sub(*s) / 1000) as u64);
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, EventPayload, now_millis};
    use apb_core::schema::Playbook;

    fn ev(seq: u64, payload: EventPayload) -> Event {
        Event {
            seq,
            ts: now_millis(),
            payload,
        }
    }

    fn linear_pb() -> Playbook {
        Playbook::from_yaml(
            r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi, expected_duration: 100 }
  - { id: b, type: agent_task, prompt: hi, expected_duration: 300 }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: b }
  - { from: b, to: f }
"#,
        )
        .unwrap()
    }

    #[test]
    fn weights_by_expected_seconds() {
        let pb = linear_pb();
        let events = vec![
            ev(
                0,
                EventPayload::RunStarted {
                    playbook: "p".into(),
                    version: "1.0.0".into(),
                },
            ),
            ev(
                1,
                EventPayload::NodeFinished {
                    node: "a".into(),
                    status: "succeeded".into(),
                    attempt: 1,
                    output: String::new(),
                },
            ),
        ];
        // done=100 of total=400 (a+b; start/finish weigh 0) -> 25
        assert_eq!(compute(&pb, &events).percent, 25);
    }

    #[test]
    fn retry_does_not_move_backward() {
        let pb = linear_pb();
        let mut events = vec![
            ev(
                0,
                EventPayload::RunStarted {
                    playbook: "p".into(),
                    version: "1.0.0".into(),
                },
            ),
            ev(
                1,
                EventPayload::NodeFinished {
                    node: "a".into(),
                    status: "succeeded".into(),
                    attempt: 1,
                    output: String::new(),
                },
            ),
        ];
        let before = compute(&pb, &events).percent;
        events.push(ev(
            2,
            EventPayload::RetryStarted {
                node: "b".into(),
                attempt: 2,
            },
        ));
        assert_eq!(compute(&pb, &events).percent, before);
    }

    #[test]
    fn skipped_leaves_denominator() {
        let pb = linear_pb();
        let events = vec![
            ev(
                0,
                EventPayload::RunStarted {
                    playbook: "p".into(),
                    version: "1.0.0".into(),
                },
            ),
            ev(
                1,
                EventPayload::NodeFinished {
                    node: "a".into(),
                    status: "succeeded".into(),
                    attempt: 1,
                    output: String::new(),
                },
            ),
            ev(
                2,
                EventPayload::NodeFinished {
                    node: "b".into(),
                    status: "skipped".into(),
                    attempt: 1,
                    output: String::new(),
                },
            ),
        ];
        // b left the denominator; a is all remaining work -> 100
        assert_eq!(compute(&pb, &events).percent, 100);
    }

    fn cyclic_pb() -> Playbook {
        Playbook::from_yaml(
            r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: work, type: agent_task, prompt: hi, expected_duration: 100 }
  - { id: gate, type: condition, max_loops: 20 }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: work }
  - { from: work, to: gate }
  - { from: gate, to: work, condition: { type: node_status, node: work, equals: failure } }
  - { from: gate, to: f, condition: { type: node_status, node: work, equals: success } }
"#,
        )
        .unwrap()
    }

    #[test]
    fn cycle_scales_with_report_and_clamps() {
        let pb = cyclic_pb();
        // report done=3 of total=14 -> 3/14 of one-pass (100s) over 14*100 total.
        let events = vec![
            ev(
                0,
                EventPayload::RunStarted {
                    playbook: "p".into(),
                    version: "1.0.0".into(),
                },
            ),
            ev(
                1,
                EventPayload::RunProgress {
                    node_id: "work".into(),
                    done: 3,
                    total: 14,
                    label: Some("chapter 3 of 14".into()),
                },
            ),
        ];
        let p = compute(&pb, &events);
        assert_eq!(p.percent, 21); // 300 done of 1400 total, rounded down
        assert_eq!(p.label.as_deref(), Some("chapter 3 of 14"));

        // clamp: done>total and total=0 must never panic
        let bad = vec![
            ev(
                0,
                EventPayload::RunStarted {
                    playbook: "p".into(),
                    version: "1.0.0".into(),
                },
            ),
            ev(
                1,
                EventPayload::RunProgress {
                    node_id: "work".into(),
                    done: 99,
                    total: 0,
                    label: None,
                },
            ),
        ];
        assert_eq!(compute(&pb, &bad).percent, 100);
    }

    #[test]
    fn succeeded_run_pins_to_100() {
        let pb = linear_pb();
        let events = vec![
            ev(
                0,
                EventPayload::RunStarted {
                    playbook: "p".into(),
                    version: "1.0.0".into(),
                },
            ),
            ev(
                1,
                EventPayload::RunFinished {
                    outcome: "succeeded".into(),
                },
            ),
        ];
        assert_eq!(compute(&pb, &events).percent, 100);
    }

    fn review_pb() -> Playbook {
        Playbook::from_yaml(
            r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: r, type: human_review, options: [approve, reject] }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: r }
  - { from: r, to: f }
"#,
        )
        .unwrap()
    }

    #[test]
    fn pending_human_review_waits_with_kind() {
        let pb = review_pb();
        // A pending review: ReviewRequested with no matching ReviewDecided. The
        // node is still Pending (NodeStarted fires only when the decision lands),
        // so detection must key on the request/decision counts, not Running.
        let events = vec![
            ev(
                0,
                EventPayload::RunStarted {
                    playbook: "p".into(),
                    version: "1.0.0".into(),
                },
            ),
            ev(
                1,
                EventPayload::ReviewRequested {
                    node: "r".into(),
                    options: vec!["approve".into(), "reject".into()],
                },
            ),
        ];
        let p = compute(&pb, &events);
        assert_eq!(p.waiting_on.as_deref(), Some("r"));
        assert_eq!(p.waiting_kind, Some(WaitingKind::HumanReview));
    }

    #[test]
    fn decided_human_review_clears_waiting() {
        let pb = review_pb();
        let events = vec![
            ev(
                0,
                EventPayload::RunStarted {
                    playbook: "p".into(),
                    version: "1.0.0".into(),
                },
            ),
            ev(
                1,
                EventPayload::ReviewRequested {
                    node: "r".into(),
                    options: vec!["approve".into(), "reject".into()],
                },
            ),
            ev(
                2,
                EventPayload::NodeStarted {
                    node: "r".into(),
                    attempt: 1,
                },
            ),
            ev(
                3,
                EventPayload::ReviewDecided {
                    node: "r".into(),
                    decision: "approve".into(),
                    note: String::new(),
                },
            ),
        ];
        let p = compute(&pb, &events);
        assert_eq!(p.waiting_on, None);
        assert_eq!(p.waiting_kind, None);
    }

    fn wait_pb() -> Playbook {
        Playbook::from_yaml(
            r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: w, type: wait, wait_for: { type: timer, seconds: 60 }, timeout_seconds: 120 }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: w }
  - { from: w, to: f }
"#,
        )
        .unwrap()
    }

    #[test]
    fn running_wait_node_waits_with_kind() {
        let pb = wait_pb();
        // A wait node emits NodeStarted (Running) while it blocks, so the
        // Running-based detection still applies to it.
        let events = vec![
            ev(
                0,
                EventPayload::RunStarted {
                    playbook: "p".into(),
                    version: "1.0.0".into(),
                },
            ),
            ev(
                1,
                EventPayload::NodeStarted {
                    node: "w".into(),
                    attempt: 1,
                },
            ),
        ];
        let p = compute(&pb, &events);
        assert_eq!(p.waiting_on.as_deref(), Some("w"));
        assert_eq!(p.waiting_kind, Some(WaitingKind::Wait));
    }

    /// A cycle group ({work, gate}) with a following non-cyclic node ({after}).
    /// One pass of the cycle is 100s, `after` is 100s, so a finished cycle pass
    /// is 50 percent.
    fn cyclic_then_linear_pb() -> Playbook {
        Playbook::from_yaml(
            r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: work, type: agent_task, prompt: hi, expected_duration: 100 }
  - { id: gate, type: condition, max_loops: 20 }
  - { id: after, type: agent_task, prompt: hi, expected_duration: 100 }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: work }
  - { from: work, to: gate }
  - { from: gate, to: work, condition: { type: node_status, node: work, equals: failure } }
  - { from: gate, to: after, condition: { type: node_status, node: work, equals: success } }
  - { from: after, to: f }
"#,
        )
        .unwrap()
    }

    #[test]
    fn cycle_credit_survives_loop_reentry() {
        // B1: a report-free cyclic group credits a node that has ever succeeded,
        // so a back-edge re-entry (NodeStarted flipping it to Running) must not
        // roll the percent back.
        let pb = cyclic_then_linear_pb();
        let mut events = vec![
            ev(
                0,
                EventPayload::RunStarted {
                    playbook: "p".into(),
                    version: "1.0.0".into(),
                },
            ),
            ev(
                1,
                EventPayload::NodeFinished {
                    node: "work".into(),
                    status: "succeeded".into(),
                    attempt: 1,
                    output: String::new(),
                },
            ),
        ];
        // One cycle pass done (100s) of the 200s plan -> 50 percent.
        assert_eq!(compute(&pb, &events).percent, 50);

        // Loop re-entry: work restarts (Running). Percent must not drop.
        events.push(ev(
            2,
            EventPayload::NodeStarted {
                node: "work".into(),
                attempt: 1,
            },
        ));
        assert_eq!(compute(&pb, &events).percent, 50);
    }

    #[test]
    fn legacy_schema1_snapshot_yields_progress_with_defaults() {
        // B3: a schema-1 snapshot (executors block, no expected_duration) must
        // still produce progress via per-kind defaults, not None.
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("playbook.yaml"),
            "schema: 1\nid: p\nname: P\nversion: 1.0.0\nexecutors:\n  main: { agent: claude, model: haiku }\ndefaults:\n  executor: main\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: agent_task, prompt: \"do\", executor: main }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: a }\n  - { from: a, to: f }\n",
        )
        .unwrap();
        let events = vec![
            ev(
                0,
                EventPayload::RunStarted {
                    playbook: "p".into(),
                    version: "1.0.0".into(),
                },
            ),
            ev(
                1,
                EventPayload::NodeFinished {
                    node: "a".into(),
                    status: "succeeded".into(),
                    attempt: 1,
                    output: String::new(),
                },
            ),
        ];
        let p = from_run_dir(&run_dir, &events).expect("schema-1 snapshot must yield progress");
        // `a` uses the agent_task default (120s) and is the only counted work,
        // so a succeeded `a` pins the percent to 100.
        assert_eq!(p.percent, 100);
    }

    #[test]
    fn plan_key_stable_on_repeat_and_label_change_but_moves_on_total() {
        // B4: plan_key is a work-plan identity, not display copy.
        let pb = cyclic_pb();
        let base = |extra: Vec<Event>| {
            let mut events = vec![ev(
                0,
                EventPayload::RunStarted {
                    playbook: "p".into(),
                    version: "1.0.0".into(),
                },
            )];
            events.extend(extra);
            compute(&pb, &events).plan_key
        };

        let one = base(vec![ev(
            1,
            EventPayload::RunProgress {
                node_id: "work".into(),
                done: 3,
                total: 14,
                label: Some("chapter 3 of 14".into()),
            },
        )]);
        // Same total, advanced done and a different label -> stable plan_key.
        let two = base(vec![ev(
            1,
            EventPayload::RunProgress {
                node_id: "work".into(),
                done: 5,
                total: 14,
                label: Some("chapter 5 of 14".into()),
            },
        )]);
        assert_eq!(one, two, "done/label changes must not move plan_key");

        // Raised total -> plan_key changes.
        let three = base(vec![ev(
            1,
            EventPayload::RunProgress {
                node_id: "work".into(),
                done: 5,
                total: 20,
                label: Some("chapter 5 of 14".into()),
            },
        )]);
        assert_ne!(one, three, "a raised total must move plan_key");

        // The version is part of the identity.
        assert!(one.starts_with("1.0.0|"));
    }

    #[test]
    fn node_durations_measures_started_to_finished() {
        // B8: wall time from NodeStarted to NodeFinished, in whole seconds.
        let events = vec![
            Event {
                seq: 0,
                ts: 1_000,
                payload: EventPayload::NodeStarted {
                    node: "a".into(),
                    attempt: 1,
                },
            },
            Event {
                seq: 1,
                ts: 4_500,
                payload: EventPayload::NodeFinished {
                    node: "a".into(),
                    status: "succeeded".into(),
                    attempt: 1,
                    output: String::new(),
                },
            },
        ];
        // (4500 - 1000) / 1000 = 3 (integer seconds).
        assert_eq!(node_durations_seconds(&events).get("a"), Some(&3));
    }

    #[test]
    fn run_answer_is_the_finish_output() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("playbook.yaml"),
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: f, type: finish, outcome: success, prompt: \"c\" }\nedges:\n  - { from: s, to: f }\n",
        )
        .unwrap();
        let events = vec![
            ev(
                0,
                EventPayload::RunStarted {
                    playbook: "p".into(),
                    version: "1.0.0".into(),
                },
            ),
            ev(
                1,
                EventPayload::NodeFinished {
                    node: "f".into(),
                    status: "succeeded".into(),
                    attempt: 1,
                    output: "THE ANSWER".into(),
                },
            ),
        ];
        assert_eq!(run_answer(&run_dir, &events).as_deref(), Some("THE ANSWER"));
    }

    #[test]
    fn run_answer_none_for_empty_finish() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("playbook.yaml"),
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: f }\n",
        )
        .unwrap();
        let events = vec![
            ev(
                0,
                EventPayload::RunStarted {
                    playbook: "p".into(),
                    version: "1.0.0".into(),
                },
            ),
            ev(
                1,
                EventPayload::NodeFinished {
                    node: "f".into(),
                    status: "succeeded".into(),
                    attempt: 1,
                    output: String::new(),
                },
            ),
        ];
        assert_eq!(run_answer(&run_dir, &events), None);
    }

    #[test]
    fn node_durations_loop_overwrite_last_wins() {
        // B8: two started/finished pairs for the same node - the last wins.
        let events = vec![
            Event {
                seq: 0,
                ts: 1_000,
                payload: EventPayload::NodeStarted {
                    node: "a".into(),
                    attempt: 1,
                },
            },
            Event {
                seq: 1,
                ts: 3_000,
                payload: EventPayload::NodeFinished {
                    node: "a".into(),
                    status: "failed".into(),
                    attempt: 1,
                    output: String::new(),
                },
            },
            Event {
                seq: 2,
                ts: 10_000,
                payload: EventPayload::NodeStarted {
                    node: "a".into(),
                    attempt: 2,
                },
            },
            Event {
                seq: 3,
                ts: 15_000,
                payload: EventPayload::NodeFinished {
                    node: "a".into(),
                    status: "succeeded".into(),
                    attempt: 2,
                    output: String::new(),
                },
            },
        ];
        // Last pass: (15000 - 10000) / 1000 = 5.
        assert_eq!(node_durations_seconds(&events).get("a"), Some(&5));
    }
}
