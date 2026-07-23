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
    /// An interactive `agent_task` is parked on a question it asked that has
    /// not yet been answered (spec 2026-07-20-interactive-nodes). Serializes
    /// to `"question"`.
    Question,
    /// A supervised run raised a wake after a node failure/timeout and is
    /// parked waiting for a supervisor command (issue #45 finding 4).
    /// Serializes to `"supervisor"`.
    Supervisor,
}

/// The pending question for a run whose `waiting_kind` is
/// `Some(WaitingKind::Question)` (spec 2026-07-20-interactive-nodes). Built
/// from the `questions.jsonl` / `answers.jsonl` channel files directly
/// (`pending_question_for_run`), not from the event log, so it is visible
/// even before drive journals `QuestionAsked` for it.
#[derive(Debug, Clone, Serialize)]
pub struct PendingQuestion {
    pub node: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
    /// `"human"` or `"supervisor"`, from the node's `answer_by` in the run's
    /// playbook snapshot. Defaults to `"human"` when the snapshot or node is
    /// missing (fail safe, same default as `question::answer_by_for`).
    pub answer_by: String,
    /// Milliseconds since epoch when the question was asked, taken from the
    /// matching `QuestionAsked` event's `ts` when drive has journaled it.
    /// `0` before that (the channel entry exists but drive has not yet
    /// observed it); the web treats `0` as "just now" rather than
    /// synthesizing a non-deterministic `now_millis` here.
    pub asked_at: u128,
}

/// The pending human-review gate for a run whose `waiting_kind` is
/// `Some(WaitingKind::HumanReview)` (issue #42 finding 4). Unlike
/// `PendingQuestion`, it is derived from the event log plus the run's playbook
/// snapshot alone (no side channel), so the pure `compute_with` fold populates
/// it. It exists so an intermediary that reads `run_status` is forced to see
/// that a decision is expected, what the options are, and how to answer -
/// rather than the run silently waiting forever.
#[derive(Debug, Clone, Serialize)]
pub struct PendingReview {
    pub node: String,
    /// The gate node's title from the playbook, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// A single self-contained owner-facing line: names the gate, its options,
    /// and how to decide. This is the text a supervising agent relays verbatim.
    pub instruction: String,
    /// The configured decisions (for a structured UI); empty falls back to
    /// approve/reject in the instruction text.
    pub options: Vec<String>,
    /// The decision mechanics alone (apb review CLI / review_decide MCP tool),
    /// for callers that render options and mechanics separately.
    pub how_to_decide: String,
}

/// The decision mechanics for a human-review gate: how an owner (or an agent
/// relaying on their behalf) actually records a decision. Single source of the
/// how-to wording, composed into the full `review_instruction` and also
/// surfaced on its own in `PendingReview::how_to_decide`.
pub fn review_how_to_decide(node_id: &str) -> String {
    format!(
        "To decide, run `apb review <run-id> {node_id} --decision <option>` \
         or call the review_decide MCP tool with the same node and decision."
    )
}

/// The owner-facing pending instruction for a human-review gate (issue #42
/// finding 4): one self-contained line naming the gate, listing the available
/// decisions, and appending `review_how_to_decide`. Shared by the
/// `ReviewRequested` event and the `run_status` `pending_review` block so the
/// two never drift.
pub fn review_instruction(node_id: &str, title: Option<&str>, options: &[String]) -> String {
    let label = title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(node_id);
    let opts = if options.is_empty() {
        "approve or reject".to_string()
    } else {
        options.join(", ")
    };
    format!(
        "The run is paused at the human-review gate \"{label}\" and needs your decision. \
         Available decisions: {opts}. {}",
        review_how_to_decide(node_id)
    )
}

/// Assembles the `PendingReview` block for a gate node from its id, title, and
/// options, reusing `review_instruction` so the instruction matches the event.
pub fn pending_review(node_id: &str, title: Option<&str>, options: &[String]) -> PendingReview {
    PendingReview {
        node: node_id.to_string(),
        title: title
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string),
        instruction: review_instruction(node_id, title, options),
        options: options.to_vec(),
        how_to_decide: review_how_to_decide(node_id),
    }
}

/// Supervisor options available after a node failure/timeout wake (issue #45
/// finding 4). Shared by the progress block and doctor so the lists never drift.
pub fn supervisor_decision_options() -> Vec<String> {
    vec![
        "retry".to_string(),
        "continue_from".to_string(),
        "abort".to_string(),
    ]
}

/// Decision mechanics for a supervised failure/timeout wake: how a supervisor
/// (or an operator) actually posts a command. Single source of the how-to
/// wording.
pub fn supervisor_how_to_decide(node_id: &str) -> String {
    format!(
        "To decide, use the supervisor tools: node_retry (re-run node `{node_id}`), \
         run_continue_from (advance from a chosen node), or run_abort."
    )
}

/// Owner/supervisor-facing pending instruction for a failure/timeout wake.
pub fn supervisor_instruction(node_id: &str, trigger: &str) -> String {
    let what = match trigger {
        "node_timeout" => "timed out",
        _ => "failed",
    };
    let opts = supervisor_decision_options().join(", ");
    format!(
        "The supervised run is waiting for a supervisor decision after node `{node_id}` {what}. \
         Available options: {opts}. {}",
        supervisor_how_to_decide(node_id)
    )
}

/// The pending supervisor decision for a run whose `waiting_kind` is
/// `Some(WaitingKind::Supervisor)` (issue #45 finding 4). Derived from the
/// event log alone: a `WakeRaised` for node failure/timeout with no later
/// resolving supervisor action and a non-terminal run.
#[derive(Debug, Clone, Serialize)]
pub struct PendingSupervisor {
    pub node: String,
    /// `"node_failed"` or `"node_timeout"`, matching `WakeTrigger` serde names.
    pub trigger: String,
    /// Self-contained line naming the wake, the failed node, the options, and
    /// how to decide.
    pub instruction: String,
    pub options: Vec<String>,
    pub how_to_decide: String,
}

/// Whether a `SupervisorAction` resolves a prior failure/timeout wake. Only
/// retry and continue_from leave the park loop to resume driving; context and
/// progress notes do not. Abort/pause clear the pending state via their own
/// terminal events (`RunAborted` / `RunPaused`), not via this action list.
fn supervisor_action_resolves_wake(action: &str) -> bool {
    matches!(action, "node_retry" | "run_continue_from")
}

/// The outstanding failure/timeout wake, if the supervised driver is parked
/// waiting for a supervisor command (issue #45 finding 4).
///
/// Pure journal fold: finds the latest `WakeRaised` with trigger
/// `node_failed`/`node_timeout` that is not followed by a resolving
/// `SupervisorAction` or a terminal/paused run event. Anomaly wakes (questions,
/// stalls) are not included - those surface through other waiting kinds.
pub fn pending_supervisor_decision(events: &[Event]) -> Option<PendingSupervisor> {
    use crate::event::WakeTrigger;
    use crate::state::RunStatus;

    let state = RunState::fold(events);
    // A terminal or paused pure-fold run is not waiting on the supervisor park
    // loop (that loop only exists while the drive is still inside the
    // supervised failure branch). Interrupted with no live attempt is a crash
    // shape, not a deliberate supervisor wait.
    if matches!(
        state.run_status,
        RunStatus::Succeeded
            | RunStatus::Failed
            | RunStatus::Aborted
            | RunStatus::Paused
            | RunStatus::Interrupted
            | RunStatus::Created
    ) {
        return None;
    }

    let mut pending: Option<(String, &'static str)> = None;
    for e in events {
        match &e.payload {
            EventPayload::WakeRaised {
                trigger: WakeTrigger::NodeFailed,
                node,
                ..
            } => {
                pending = Some((node.clone(), "node_failed"));
            }
            EventPayload::WakeRaised {
                trigger: WakeTrigger::NodeTimeout,
                node,
                ..
            } => {
                pending = Some((node.clone(), "node_timeout"));
            }
            EventPayload::SupervisorAction { action, .. }
                if supervisor_action_resolves_wake(action) =>
            {
                pending = None;
            }
            EventPayload::RunFinished { .. }
            | EventPayload::RunAborted { .. }
            | EventPayload::RunPaused { .. }
            | EventPayload::PatchApplied { .. } => {
                pending = None;
            }
            _ => {}
        }
    }
    pending.map(|(node, trigger)| PendingSupervisor {
        instruction: supervisor_instruction(&node, trigger),
        how_to_decide: supervisor_how_to_decide(&node),
        options: supervisor_decision_options(),
        node,
        trigger: trigger.to_string(),
    })
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
    /// The pending question when `waiting_kind == Some(WaitingKind::Question)`.
    /// `None` otherwise, and `None` for the plain `compute`/`compute_with`
    /// path (no run dir to read the channel files from) - only the
    /// `from_run_dir` family populates it.
    pub pending_question: Option<PendingQuestion>,
    /// The pending human-review gate when `waiting_kind ==
    /// Some(WaitingKind::HumanReview)` (issue #42 finding 4). Populated by the
    /// pure `compute_with` fold (it needs only events plus the playbook), so
    /// unlike `pending_question` it is present on the `compute`/`compute_with`
    /// path too. `None` whenever the run is not waiting on a gate.
    pub pending_review: Option<PendingReview>,
    /// The pending supervisor decision when `waiting_kind ==
    /// Some(WaitingKind::Supervisor)` (issue #45 finding 4). Populated by the
    /// pure fold from the event log alone.
    pub pending_supervisor: Option<PendingSupervisor>,
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

/// Loads the run's immutable playbook snapshot from `<run_dir>/playbook.yaml`
/// (spec 2026-07-20, Task 5 dependency-cycle fix: defined in
/// `legacy_snapshot` - the module it is actually built from - and re-exported
/// here for API stability; `question.rs` needs it too, and `progress.rs` <->
/// `question.rs` would otherwise be a mutual module cycle). See
/// `legacy_snapshot::load_run_playbook` for the full doc comment.
pub use crate::legacy_snapshot::load_run_playbook;

/// The first interactive `agent_task` node with a pending question, read
/// directly from the `questions.jsonl` / `answers.jsonl` channel files
/// (spec 2026-07-20-interactive-nodes) rather than the event log, so it is
/// visible even before drive journals `QuestionAsked` for it (the live
/// transport in a later task depends on this exact property). Node order
/// follows `playbook.nodes`, giving a deterministic pick when more than one
/// interactive node happens to be pending at once.
fn pending_question_for_run(
    run_dir: &Path,
    playbook: &Playbook,
    events: &[Event],
) -> Option<PendingQuestion> {
    playbook.nodes.iter().find_map(|n| {
        if matches!(
            n.kind,
            NodeKind::AgentTask {
                interactive: true,
                ..
            }
        ) {
            pending_question_for_node(run_dir, playbook, events, &n.id)
        } else {
            None
        }
    })
}

/// A single node's pending question, or `None` when every asked question has
/// a matching answer. Mirrors `compute_with`'s `review_pending` count-based
/// detection: the node has a pending question when its asked-question count
/// exceeds its answered count, and the pending question is the first
/// unanswered entry (index == the answered count, since answers consume
/// questions in posting order).
fn pending_question_for_node(
    run_dir: &Path,
    playbook: &Playbook,
    events: &[Event],
    node_id: &str,
) -> Option<PendingQuestion> {
    // A read failure (corrupt/unreadable questions.jsonl or answers.jsonl) is
    // not silent: it writes one stderr warning naming the file and error,
    // the same handling `load_run_playbook` gives an unparseable snapshot,
    // then collapses to "no pending question" (the channel files are
    // engine-written and append-only, so a read failure here is a
    // filesystem-level fault worth a terminal signal rather than an
    // authoring one).
    let questions: Vec<_> = match crate::question::read_questions_after(run_dir, None) {
        Ok(qs) => qs,
        Err(e) => {
            eprintln!(
                "apb: warning: run {} questions.jsonl unreadable: {e}",
                run_dir.display()
            );
            return None;
        }
    }
    .into_iter()
    .filter(|q| q.node == node_id)
    .collect();
    let answered = match crate::question::read_answers_after(run_dir, None) {
        Ok(ans) => ans,
        Err(e) => {
            eprintln!(
                "apb: warning: run {} answers.jsonl unreadable: {e}",
                run_dir.display()
            );
            return None;
        }
    }
    .into_iter()
    .filter(|a| a.node == node_id)
    .count();
    if questions.len() <= answered {
        return None;
    }
    let q = &questions[answered];
    let answer_by = playbook
        .node(node_id)
        .and_then(|n| match &n.kind {
            NodeKind::AgentTask { answer_by, .. } => Some(answer_by.as_str().to_string()),
            _ => None,
        })
        .unwrap_or_else(|| "human".to_string());
    // asked_at: the ts of the (answered-count)-th `QuestionAsked` event
    // journaled for this node, positionally matching the channel order,
    // since drive journals them in the order it observes new channel
    // entries. `0` when drive has not journaled it yet (the web treats `0`
    // as "just now" rather than a synthesized now_millis, which would be
    // non-deterministic at fold time).
    let asked_at = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::QuestionAsked { node, .. } if node == node_id => Some(e.ts),
            _ => None,
        })
        .nth(answered)
        .unwrap_or(0);
    Some(PendingQuestion {
        node: node_id.to_string(),
        question: q.question.clone(),
        options: q.options.clone(),
        answer_by,
        asked_at,
    })
}

/// Sums, over `events`, every completed `QuestionAsked`-to-`QuestionAnswered`
/// interval (ms) for `node`, matched in journal order: the first open
/// `QuestionAsked` pairs with the next `QuestionAnswered` for the same node,
/// and so on - mirroring the count-based consumption the drive interactive
/// branch already uses (`question_asked_count`/`question_answered_count`
/// in `scheduler.rs`). An open (asked but not yet answered) question
/// contributes nothing: its duration is not yet known, and by construction
/// there is at most one open question per node at a time.
///
/// Pure and bounded (spec 2026-07-20, Task 5): no I/O, just one scan of the
/// given slice. Consumed by the node-timeout exclusion wired into
/// `check_cancel_timeout` (a no-op for the reprompt path within a single
/// attempt - a park always spans a fresh attempt boundary, so no completed
/// interval ever falls inside one attempt's own clock) and, later, the live
/// `ask_user` transport (Task 11), whose single long-lived attempt DOES span
/// a pending question and needs that duration excluded from the node
/// timeout.
pub fn pending_interval_ms(events: &[Event], node: &str) -> u128 {
    let mut total = 0u128;
    let mut open_asked_ts: Option<u128> = None;
    for e in events {
        match &e.payload {
            EventPayload::QuestionAsked { node: n, .. } if n == node => {
                open_asked_ts.get_or_insert(e.ts);
            }
            EventPayload::QuestionAnswered { node: n, .. } if n == node => {
                if let Some(asked_ts) = open_asked_ts.take() {
                    total += e.ts.saturating_sub(asked_ts);
                }
            }
            _ => {}
        }
    }
    total
}

/// Computes the progress summary for a run directory, or `None` when the
/// playbook snapshot is missing or unparseable. The rule "missing or
/// unparseable snapshot means no progress" lives here and only here.
///
/// Delegates to `from_run_dir_with_root` with the run root derived from the
/// run dir: a run lives at `<root>/.apb/runs/<id>`, so three parents up from
/// the run dir is the project root.
pub fn from_run_dir(run_dir: &Path, events: &[Event]) -> Option<ProgressSummary> {
    let root = run_dir
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .unwrap_or(run_dir);
    from_run_dir_with_root(root, run_dir, events)
}

/// Progress for a run dir with child credit (spec C): base weighted totals
/// plus, for each RUNNING `playbook` node whose latest child is non-terminal,
/// a fractional credit `child_percent/100 * expected_seconds(node)` added to
/// done. The pure `compute` fold stays untouched; this enrichment lives here.
pub fn from_run_dir_with_root(
    root: &Path,
    run_dir: &Path,
    events: &[Event],
) -> Option<ProgressSummary> {
    let pb = load_run_playbook(run_dir)?;
    // Build the group structure ONCE and feed it to both folds (review I3),
    // instead of `compute` and `weighted` each rebuilding it independently.
    let gc = GroupContext::build(&pb, events);
    let mut summary = compute_with(&pb, events, &gc);
    // A pending question takes precedence over whatever `compute_with` set -
    // human_review, wait, or nothing - unconditionally: it is read from the
    // questions.jsonl/answers.jsonl channel files, which only a run dir can
    // provide, so this override lives here rather than in the pure
    // `compute_with` fold. This is deliberate even while a Wait node is
    // Running (the `WaitingKind::Wait` case above): a question needs a human,
    // a timer wait resolves itself, so the question is always the tighter
    // block, and `ProgressSummary` can only surface one `waiting_on`. Hiding a
    // pending question behind a self-resolving wait would be worse than
    // always showing it, so this never narrows to "only override when
    // nothing else is waiting".
    if let Some(pq) = pending_question_for_run(run_dir, &pb, events) {
        summary.waiting_on = Some(pq.node.clone());
        summary.waiting_kind = Some(WaitingKind::Question);
        summary.pending_question = Some(pq);
        // The single `waiting_on` now names the question, so a stale gate or
        // supervisor block must not linger alongside it (one-block invariant).
        summary.pending_review = None;
        summary.pending_supervisor = None;
    }
    let (done, total) = weighted_with(&pb, events, &gc);
    if total == 0 {
        return Some(summary);
    }
    let state = RunState::fold(events);
    let mut extra: u128 = 0;
    for n in &pb.nodes {
        if !matches!(n.kind, NodeKind::Playbook { .. }) {
            continue;
        }
        // A node currently Running with a non-terminal child.
        if state.nodes.get(&n.id).copied() != Some(NodeStatus::Running) {
            continue;
        }
        let Some(child) = events.iter().rev().find_map(|e| match &e.payload {
            EventPayload::ChildRunStarted { node_id, run_id } if node_id == &n.id => {
                Some(run_id.clone())
            }
            _ => None,
        }) else {
            continue;
        };
        let child_dir = root.join(".apb/runs").join(&child);
        // Display-only (review I7/R1-I9): this is a progress-percentage
        // enrichment, not a control decision. An unreadable child event log
        // credits 0 extra seconds (the child simply contributes no fractional
        // progress this poll) rather than failing the parent's status read.
        // Unlike `map_child_outcome`/`run_is_terminal`, no correctness or
        // terminality choice hinges on it, so `unwrap_or_default` is deliberate.
        let child_events = crate::event::read_all(&child_dir).unwrap_or_default();
        if let Some(cp) = from_run_dir_with_root(root, &child_dir, &child_events) {
            extra += (cp.percent as u128) * (n.expected_seconds() as u128) / 100;
        }
    }
    let enriched = (done + extra).min(total);
    summary.percent = (enriched.saturating_mul(100) / total).min(100) as u8;
    if matches!(state.run_status, RunStatus::Succeeded) {
        summary.percent = 100;
    }
    Some(summary)
}

/// Group structure shared by `weighted` and `compute`, built ONCE per progress
/// request (review I3). Both folds previously rebuilt the SCC groups, the
/// node->group map, the cyclic flags, and the latest per-group report
/// independently; `from_run_dir_with_root` called both, so these structures
/// were built three times per request. This carries them so a single build
/// feeds every consumer. `group_of` owns its keys (rather than borrowing from
/// `groups`) so the whole context can live in one owned struct.
struct GroupContext {
    groups: Vec<Vec<String>>,
    /// Per group: is it cyclic (multi-node SCC, or a single node with a self-loop).
    cyclic: Vec<bool>,
    /// Latest `RunProgress` (done, total) reported for each cyclic group.
    report_of: BTreeMap<usize, (u64, u64)>,
    /// Latest `RunProgress` label overall (display copy), if any.
    label: Option<String>,
}

impl GroupContext {
    fn build(playbook: &Playbook, events: &[Event]) -> GroupContext {
        let groups = sccs(playbook);
        let mut group_of: BTreeMap<String, usize> = BTreeMap::new();
        for (gi, g) in groups.iter().enumerate() {
            for id in g {
                group_of.insert(id.clone(), gi);
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
        GroupContext {
            groups,
            cyclic,
            report_of,
            label,
        }
    }
}

/// Weighted (done, total) seconds for the run, the raw numerator/denominator
/// behind `compute`'s percent. Pure fold - no child awareness. Takes the shared
/// `GroupContext` so the SCC/group structure is built once per request (review
/// I3); `compute_with` and `from_run_dir_with_root` are the only callers.
fn weighted_with(playbook: &Playbook, events: &[Event], gc: &GroupContext) -> (u128, u128) {
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

    let groups = &gc.groups;
    let cyclic = &gc.cyclic;
    let report_of = &gc.report_of;

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

    (done, total)
}

/// Computes the progress summary from events + the run's playbook version.
/// Thin wrapper that builds a fresh `GroupContext`; hot paths call
/// `compute_with` with a shared one (review I3).
pub fn compute(playbook: &Playbook, events: &[Event]) -> ProgressSummary {
    compute_with(playbook, events, &GroupContext::build(playbook, events))
}

fn compute_with(playbook: &Playbook, events: &[Event], gc: &GroupContext) -> ProgressSummary {
    let state = RunState::fold(events);
    let status = |id: &str| state.nodes.get(id).copied().unwrap_or(NodeStatus::Pending);

    // Group structure, per-group report and the latest label come from the
    // shared `GroupContext` (label/waiting/plan_key logic stays here; the
    // done/total accumulation that also needs them lives in `weighted_with`).
    let groups = &gc.groups;
    let cyclic = &gc.cyclic;
    let report_of = &gc.report_of;
    let label = gc.label.clone();

    let (done, total) = weighted_with(playbook, events, gc);
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
    let mut waiting_on = waiting.as_ref().map(|(id, _)| id.clone());
    let mut waiting_kind = waiting.map(|(_, kind)| kind);

    // The pending-gate block (issue #42 finding 4): only when the run is
    // actually waiting on a human_review gate. Built from the node's title and
    // options in the snapshot, so a `run_status` reader is forced to see the
    // decision, the options, and how to answer.
    let pending_review = match (&waiting_on, waiting_kind) {
        (Some(id), Some(WaitingKind::HumanReview)) => {
            playbook.node(id).and_then(|n| match &n.kind {
                NodeKind::HumanReview { options } => {
                    Some(pending_review(id, n.title.as_deref(), options))
                }
                _ => None,
            })
        }
        _ => None,
    };

    // Supervised failure/timeout park (issue #45 finding 4): only when nothing
    // tighter (human_review / wait) is already waiting. Derived from WakeRaised
    // events in the journal so a run_status reader sees the wake, the failed
    // node, and the options (retry / continue_from / abort).
    let pending_supervisor = if waiting_kind.is_none() {
        let ps = pending_supervisor_decision(events);
        if let Some(ref p) = ps {
            waiting_on = Some(p.node.clone());
            waiting_kind = Some(WaitingKind::Supervisor);
        }
        ps
    } else {
        None
    };

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
        pending_question: None,
        pending_review,
        pending_supervisor,
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
                    artifacts: Vec::new(),
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
                    artifacts: Vec::new(),
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
                    artifacts: Vec::new(),
                },
            ),
            ev(
                2,
                EventPayload::NodeFinished {
                    node: "b".into(),
                    status: "skipped".into(),
                    attempt: 1,
                    output: String::new(),
                    artifacts: Vec::new(),
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
                    title: None,
                    instruction: String::new(),
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
                    title: None,
                    instruction: String::new(),
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

    fn interactive_pb_yaml() -> &'static str {
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: ask, type: agent_task, prompt: hi, interactive: true, answer_by: supervisor }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: ask }\n  - { from: ask, to: f }\n"
    }

    #[test]
    fn pending_question_waits_with_kind_and_clears_on_answer() {
        // The pending question is read from the CHANNEL files
        // (questions.jsonl vs answers.jsonl count difference), not from
        // events, so it is visible before drive ever journals
        // `QuestionAsked`.
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("playbook.yaml"), interactive_pb_yaml()).unwrap();
        crate::question::post_question(
            &run_dir,
            "ask",
            1,
            "which way",
            vec!["left".into(), "right".into()],
        )
        .unwrap();

        let events = vec![ev(
            0,
            EventPayload::RunStarted {
                playbook: "p".into(),
                version: "1.0.0".into(),
            },
        )];
        let p = from_run_dir(&run_dir, &events).expect("run dir must yield progress");
        assert_eq!(p.waiting_on.as_deref(), Some("ask"));
        assert_eq!(p.waiting_kind, Some(WaitingKind::Question));
        let pq = p
            .pending_question
            .expect("pending_question must be Some before an answer lands");
        assert_eq!(pq.node, "ask");
        assert_eq!(pq.question, "which way");
        assert_eq!(pq.options, vec!["left".to_string(), "right".to_string()]);
        assert_eq!(pq.answer_by, "supervisor");
        // No QuestionAsked journaled yet: asked_at falls back to 0 (the web
        // treats 0 as "just now"), never a non-deterministic now_millis.
        assert_eq!(pq.asked_at, 0);

        crate::question::post_answer(&run_dir, Some("ask"), "left", "supervisor").unwrap();
        let p2 = from_run_dir(&run_dir, &events).expect("run dir must yield progress");
        assert_eq!(p2.waiting_on, None);
        assert_eq!(p2.waiting_kind, None);
        assert!(p2.pending_question.is_none());
    }

    #[test]
    fn pending_question_asked_at_comes_from_the_journaled_event() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("playbook.yaml"), interactive_pb_yaml()).unwrap();
        crate::question::post_question(&run_dir, "ask", 1, "which way", Vec::new()).unwrap();

        let events = vec![
            ev(
                0,
                EventPayload::RunStarted {
                    playbook: "p".into(),
                    version: "1.0.0".into(),
                },
            ),
            Event {
                seq: 1,
                ts: 12_345,
                payload: EventPayload::QuestionAsked {
                    node: "ask".into(),
                    question: "which way".into(),
                    options: Vec::new(),
                },
            },
        ];
        let p = from_run_dir(&run_dir, &events).expect("run dir must yield progress");
        let pq = p.pending_question.expect("pending_question must be Some");
        assert_eq!(pq.asked_at, 12_345);
    }

    #[test]
    fn pending_question_takes_precedence_over_pending_review() {
        // spec: a parked question is the tighter interactive wait, so when
        // both a question and a review are pending in the same run, the
        // question wins deterministically.
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("playbook.yaml"),
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: ask, type: agent_task, prompt: hi, interactive: true, answer_by: supervisor }\n  - { id: r, type: human_review, options: [approve, reject] }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: ask }\n  - { from: ask, to: r }\n  - { from: r, to: f }\n",
        )
        .unwrap();
        crate::question::post_question(&run_dir, "ask", 1, "which way", Vec::new()).unwrap();

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
                    title: None,
                    instruction: String::new(),
                },
            ),
        ];
        let p = from_run_dir(&run_dir, &events).expect("run dir must yield progress");
        assert_eq!(p.waiting_on.as_deref(), Some("ask"));
        assert_eq!(p.waiting_kind, Some(WaitingKind::Question));
    }

    #[test]
    fn pending_question_takes_precedence_over_running_wait() {
        // spec: the question/wait override in `from_run_dir_with_root` is
        // unconditional - it replaces whatever `compute_with` produced, even
        // a Wait node that is already Running (the tighter human-blocking
        // question always wins over a self-resolving timer wait).
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("playbook.yaml"),
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: ask, type: agent_task, prompt: hi, interactive: true, answer_by: supervisor }\n  - { id: w, type: wait, wait_for: { type: timer, seconds: 60 }, timeout_seconds: 120 }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: ask }\n  - { from: ask, to: w }\n  - { from: w, to: f }\n",
        )
        .unwrap();
        crate::question::post_question(&run_dir, "ask", 1, "which way", Vec::new()).unwrap();

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
        let p = from_run_dir(&run_dir, &events).expect("run dir must yield progress");
        assert_eq!(p.waiting_on.as_deref(), Some("ask"));
        assert_eq!(p.waiting_kind, Some(WaitingKind::Question));
        assert_eq!(
            p.pending_question
                .expect("pending_question must be Some")
                .node,
            "ask"
        );
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
                    artifacts: Vec::new(),
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
                    artifacts: Vec::new(),
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
                    artifacts: Vec::new(),
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
                    artifacts: Vec::new(),
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
                    artifacts: Vec::new(),
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
                    artifacts: Vec::new(),
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
                    artifacts: Vec::new(),
                },
            },
        ];
        // Last pass: (15000 - 10000) / 1000 = 5.
        assert_eq!(node_durations_seconds(&events).get("a"), Some(&5));
    }

    #[test]
    fn running_child_contributes_fractional_credit() {
        // A parent with one playbook node (expected 100s) plus a 100s task, so a
        // half-done child contributes 50s of 200s -> 25 percent.
        let tmp = tempfile::tempdir().unwrap();
        let parent_dir = tmp.path().join(".apb/runs/parent-1");
        std::fs::create_dir_all(&parent_dir).unwrap();
        std::fs::write(
            parent_dir.join("playbook.yaml"),
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: c, type: playbook, playbook: child, expected_duration: 100 }\n  - { id: a, type: agent_task, prompt: hi, expected_duration: 100 }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: c }\n  - { from: c, to: a }\n  - { from: a, to: f }\n",
        )
        .unwrap();
        // A child run at 50 percent (one 100s task node done of two).
        let child_dir = tmp.path().join(".apb/runs/child-1");
        std::fs::create_dir_all(&child_dir).unwrap();
        std::fs::write(
            child_dir.join("playbook.yaml"),
            "schema: 2\nid: child\nname: child\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: agent_task, prompt: hi, expected_duration: 100 }\n  - { id: b, type: agent_task, prompt: hi, expected_duration: 100 }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: a }\n  - { from: a, to: b }\n  - { from: b, to: f }\n",
        )
        .unwrap();
        std::fs::write(
            child_dir.join("events.jsonl"),
            "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"child\",\"version\":\"1.0.0\"}\n{\"seq\":1,\"ts\":0,\"type\":\"node_finished\",\"node\":\"a\",\"status\":\"succeeded\",\"attempt\":1,\"output\":\"\"}\n{\"seq\":2,\"ts\":0,\"type\":\"node_started\",\"node\":\"b\",\"attempt\":1}\n",
        )
        .unwrap();
        let parent_events = vec![
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
                    node: "c".into(),
                    attempt: 1,
                },
            ),
            ev(
                2,
                EventPayload::ChildRunStarted {
                    node_id: "c".into(),
                    run_id: "child-1".into(),
                },
            ),
        ];
        // Root is the temp dir; from_run_dir must find child-1 under root/.apb/runs.
        let p = from_run_dir_with_root(tmp.path(), &parent_dir, &parent_events).unwrap();
        assert_eq!(p.percent, 25);
    }

    // Task 5 (spec 2026-07-20-interactive-nodes): `pending_interval_ms` is a
    // pure fold over a hand-built journal, so it is tested inline here rather
    // than through a full run harness.

    fn ev_at(seq: u64, ts: u128, payload: EventPayload) -> Event {
        Event { seq, ts, payload }
    }

    fn asked(seq: u64, ts: u128, node: &str) -> Event {
        ev_at(
            seq,
            ts,
            EventPayload::QuestionAsked {
                node: node.into(),
                question: "q".into(),
                options: Vec::new(),
            },
        )
    }

    fn answered(seq: u64, ts: u128, node: &str) -> Event {
        ev_at(
            seq,
            ts,
            EventPayload::QuestionAnswered {
                node: node.into(),
                answer: "a".into(),
                answered_by: "human".into(),
            },
        )
    }

    #[test]
    fn pending_interval_ms_sums_one_completed_round() {
        let events = vec![asked(0, 1000, "ask"), answered(1, 4000, "ask")];
        assert_eq!(pending_interval_ms(&events, "ask"), 3000);
    }

    #[test]
    fn pending_interval_ms_sums_two_rounds() {
        let events = vec![
            asked(0, 1000, "ask"),
            answered(1, 4000, "ask"),
            asked(2, 5000, "ask"),
            answered(3, 6500, "ask"),
        ];
        // 3000 (1000->4000) + 1500 (5000->6500) = 4500
        assert_eq!(pending_interval_ms(&events, "ask"), 4500);
    }

    #[test]
    fn pending_interval_ms_open_question_contributes_nothing() {
        // The second question is asked but never answered: bounded, pure -
        // it must not be counted, and must not panic or loop.
        let events = vec![
            asked(0, 1000, "ask"),
            answered(1, 4000, "ask"),
            asked(2, 5000, "ask"),
        ];
        assert_eq!(pending_interval_ms(&events, "ask"), 3000);
    }

    #[test]
    fn pending_interval_ms_ignores_other_nodes() {
        let events = vec![
            asked(0, 1000, "ask"),
            answered(1, 4000, "other"),
            answered(2, 4000, "ask"),
        ];
        assert_eq!(pending_interval_ms(&events, "ask"), 3000);
        assert_eq!(pending_interval_ms(&events, "other"), 0);
    }

    /// Issue #45 finding 4: a WakeRaised for node_failed with no later
    /// resolving action surfaces as waiting_kind=supervisor with options.
    #[test]
    fn pending_supervisor_decision_after_node_failed_wake() {
        let pb = Playbook::from_yaml(
            r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: work, type: agent_task, prompt: hi }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: work }
  - { from: work, to: f }
"#,
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
                    node: "work".into(),
                    status: "failed".into(),
                    attempt: 1,
                    output: "boom".into(),
                    artifacts: Vec::new(),
                },
            ),
            ev(
                2,
                EventPayload::WakeRaised {
                    trigger: crate::event::WakeTrigger::NodeFailed,
                    node: "work".into(),
                    detail: "boom".into(),
                },
            ),
        ];
        let p = compute(&pb, &events);
        assert_eq!(p.waiting_on.as_deref(), Some("work"));
        assert_eq!(p.waiting_kind, Some(WaitingKind::Supervisor));
        let ps = p
            .pending_supervisor
            .expect("pending_supervisor must be set");
        assert_eq!(ps.node, "work");
        assert_eq!(ps.trigger, "node_failed");
        assert!(ps.options.contains(&"retry".to_string()));
        assert!(ps.options.contains(&"continue_from".to_string()));
        assert!(ps.options.contains(&"abort".to_string()));
        assert!(ps.instruction.contains("work"));
    }

    #[test]
    fn supervisor_decision_clears_after_node_retry() {
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
                EventPayload::WakeRaised {
                    trigger: crate::event::WakeTrigger::NodeTimeout,
                    node: "work".into(),
                    detail: String::new(),
                },
            ),
            ev(
                2,
                EventPayload::SupervisorAction {
                    action: "node_retry".into(),
                    node: Some("work".into()),
                    detail: String::new(),
                },
            ),
        ];
        assert!(pending_supervisor_decision(&events).is_none());
    }
}
