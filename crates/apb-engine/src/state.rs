use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::event::{Event, EventPayload};

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Pending,
    Ready,
    Running,
    Succeeded,
    Failed,
    Unknown,
    TimedOut,
    Interrupted,
    Skipped,
    Cancelled,
}

impl NodeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeStatus::Pending => "pending",
            NodeStatus::Ready => "ready",
            NodeStatus::Running => "running",
            NodeStatus::Succeeded => "succeeded",
            NodeStatus::Failed => "failed",
            NodeStatus::Unknown => "unknown",
            NodeStatus::TimedOut => "timed_out",
            NodeStatus::Interrupted => "interrupted",
            NodeStatus::Skipped => "skipped",
            NodeStatus::Cancelled => "cancelled",
        }
    }
    /// Whether this status represents a node that has completed one execution
    /// (a `node_finished` was folded): the one "node finished" predicate. A
    /// join counts a finished branch with it, the cache bypass on a loop
    /// re-execution uses it (a node that already finished once must run again
    /// rather than replay its first verdict), and the reaper uses it to leave a
    /// node that reported a verdict alone. `Interrupted` and `Unknown` are not
    /// finished: a later attempt still decides them.
    pub fn is_finished(&self) -> bool {
        matches!(
            self,
            NodeStatus::Succeeded
                | NodeStatus::Failed
                | NodeStatus::TimedOut
                | NodeStatus::Skipped
                | NodeStatus::Cancelled
        )
    }
    /// Whether the node has left the not-yet-run states and is not running
    /// now: finished, or stopped part-way (`Interrupted`, `Unknown`). A patch
    /// treats these as already executed, since their effects may exist, and
    /// refuses a migration that would drop or rewrite them. Deliberately wider
    /// than [`NodeStatus::is_finished`].
    pub fn has_executed(&self) -> bool {
        !matches!(
            self,
            NodeStatus::Pending | NodeStatus::Ready | NodeStatus::Running
        )
    }

    /// Whether this status is a node that ended badly (failed, timed out, or
    /// cut off mid-attempt), which `doctor --run` flags.
    pub fn ended_badly(&self) -> bool {
        matches!(
            self,
            NodeStatus::Failed | NodeStatus::TimedOut | NodeStatus::Interrupted
        )
    }

    pub fn from_label(s: &str) -> NodeStatus {
        match s {
            "pending" => NodeStatus::Pending,
            "ready" => NodeStatus::Ready,
            "running" => NodeStatus::Running,
            "succeeded" => NodeStatus::Succeeded,
            "failed" => NodeStatus::Failed,
            "timed_out" => NodeStatus::TimedOut,
            "interrupted" => NodeStatus::Interrupted,
            "skipped" => NodeStatus::Skipped,
            "cancelled" => NodeStatus::Cancelled,
            _ => NodeStatus::Unknown,
        }
    }
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    #[default]
    Created,
    Running,
    Paused,
    Succeeded,
    Failed,
    Aborted,
    Interrupted,
}

impl RunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunStatus::Created => "created",
            RunStatus::Running => "running",
            RunStatus::Paused => "paused",
            RunStatus::Succeeded => "succeeded",
            RunStatus::Failed => "failed",
            RunStatus::Aborted => "aborted",
            RunStatus::Interrupted => "interrupted",
        }
    }

    /// Whether the journal has already recorded an outcome for this run.
    /// `Interrupted` is deliberately not one: it is a fold of an unfinished
    /// run whose driver went away, and a later terminal event still decides
    /// it. Callers use this to avoid writing a SECOND outcome over one that
    /// is already there, which would rewrite what the run actually did.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunStatus::Succeeded | RunStatus::Failed | RunStatus::Aborted
        )
    }
}

/// A human_review node decision: the chosen option and the reviewer's note.
#[derive(Debug, Clone)]
pub struct ReviewDecision {
    pub decision: String,
    pub note: String,
}

/// The most recent `RunError` folded from the journal (issue #42 finding 3):
/// the verbatim engine error text behind a run that ended `failed`, and the
/// node it is attributable to when known. `run_status` and `doctor --run`
/// surface this so an operator does not have to read events.jsonl by hand.
#[derive(Debug, Clone)]
pub struct FailureReason {
    pub node: Option<String>,
    pub reason: String,
}

impl FailureReason {
    /// A single display line: `node `x`: reason` when a node is known, the
    /// bare reason otherwise.
    pub fn display(&self) -> String {
        match &self.node {
            Some(n) => format!("node `{n}`: {}", self.reason),
            None => self.reason.clone(),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct RunState {
    pub run_status: RunStatus,
    pub nodes: BTreeMap<String, NodeStatus>,
    pub attempts: BTreeMap<String, u32>,
    pub outputs: BTreeMap<String, String>,
    pub reviews: BTreeMap<String, ReviewDecision>,
    pub last_node: Option<String>,
    /// How many times each bounded loop edge `(from, to)` has been traversed,
    /// folded from `EdgeTraversed` events (spec 2026-07-20-run-reliability).
    /// Edge selection blocks an edge once its count reaches `max_traversals`;
    /// resume restores loop progress exactly because the counts come from the
    /// journal.
    pub edge_counts: BTreeMap<(String, String), u32>,
    /// Every `(from, to)` hop the drive loop journaled, folded from every
    /// `EdgeTraversed` record whatever its flags (spec 2026-08-05 Task 4,
    /// widened by #82). This is the union `parallel::routed_targets` used to
    /// compute itself from `edge_counts` (entries with count > 0) and the old
    /// `policy_routes`: on an old journal it receives exactly that same set,
    /// so `routed_targets` returns the identical set bit for bit. A hop that
    /// only ever appears here (never in `edge_counts`) is either a policy
    /// route or a routing decision that crossed no bounded edge; either way,
    /// "which way did this node actually go" reads this set, not `edge_counts`.
    pub journaled_hops: BTreeSet<(String, String)>,
    /// Per-node agent text discarded when a `success_check` rejected an
    /// otherwise-succeeded attempt (spec field-report-robustness), folded from
    /// `AttemptFinished.rejected_output`. Exposed to downstream templates as
    /// `nodes.<id>.rejected_output`. A later rejected attempt overwrites an
    /// earlier one; a node that later succeeds keeps its last rejected text
    /// here (harmless - only a node that references the key reads it).
    pub rejected_outputs: BTreeMap<String, String>,
    /// The most recent `RunError` folded from the journal, if any (issue #42
    /// finding 3). Set even when the run later succeeds via resume/patch past
    /// the failure - `run_status`/doctor only read it while `run_status` is
    /// `Failed`, so a stale value from an earlier attempt is harmless.
    pub failure_reason: Option<FailureReason>,
    /// The run's working tree (issue #67 item 8), folded from the first
    /// `WorktreeResolved`: the directory agent_task and script nodes without
    /// their own `workdir` run in. `None`: the execution root.
    pub worktree: Option<String>,
    /// The judge-edge answers of each source node (issue #165 Part 7), from
    /// its latest error-free `judge_edge` decision.
    pub judge_edges: BTreeMap<String, JudgeEdgeAnswers>,
    /// How many times each node has finished, folded from `NodeFinished`: a
    /// judge-edge answer belongs to the execution it was asked after.
    pub finished_counts: BTreeMap<String, u32>,
    /// Nodes whose own outgoing edges a fork's branch-failure policy took off
    /// the table (issue #195): the failed node under `fail_fast` and every node
    /// the policy cancelled. Edge selection gives them no successor, so a
    /// resume does not follow a route the run decided against. A node leaves
    /// the set when it starts again.
    pub route_suppressed: BTreeSet<String>,
    /// Joins that refused under `require: all_succeeded` (issue #195): they
    /// take only a failure route, never an unconditional edge. A node leaves
    /// the set when it starts again.
    pub refused_joins: BTreeSet<String>,
    /// Nodes a re-run revived (issue #203), folded from `NodesRevived`: the
    /// drive executes them again, so until each one starts again its result
    /// from the earlier pass does not count for routing. `nodes` keeps that
    /// result (it is the run's history every surface reports), and so does
    /// `journaled_hops`: [`RunState::current_status`] and
    /// [`RunState::routes`] are the view routing and join readiness read.
    /// A node leaves the set when it starts or finishes again (a finish node
    /// without a prompt journals only its finish); its old hops are dropped
    /// then. The union of [`RunState::revivals`].
    pub revived: BTreeSet<String>,
    /// The `from_node` of every revival whose node has not started again: the
    /// re-runs a drive was asked for and has not begun. A pause before the
    /// node starts, or a kill while a revived review gate waits, leaves one
    /// here; a resume re-runs it ([`crate::scheduler::plan_resume`]) and the
    /// frontier rebuild puts every other one back. A later explicit re-run
    /// supersedes them (`NodesRevived::superseded`).
    pub revival_starts: BTreeSet<String>,
    /// Each revival still in force: its `from_node` and the nodes it revived
    /// that have not started again.
    pub revivals: Vec<RevivalEntry>,
    /// Hops `(from, to)` into a revived node journaled after its revival: a
    /// fresh delivery of this pass. Such a node is a pending head again even
    /// if it finished in an earlier pass (`parallel::pending_heads`), but
    /// only while the hop still routes ([`RunState::routes`]): a later
    /// revival of the delivering node makes it stale. A pair leaves the set
    /// when its target runs again or its revival ends.
    pub delivered_since_revival: BTreeSet<(String, String)>,
    /// Where each node's latest execution sits in the journal: the seq of
    /// its latest `node_started` and `node_finished`, and of the latest
    /// `edge_traversed` of each hop. Routing reads a node's latest
    /// execution only ([`RunState::hop_of_latest_run`]), and a join inside a
    /// loop does not count a delivery it already consumed
    /// ([`RunState::join_consumed`]): the earlier rounds' results stay in
    /// `nodes` and `journaled_hops`.
    pub seqs: NodeSeqs,
}

/// The journal positions behind [`RunState::seqs`].
#[derive(Debug, Default, Clone)]
pub struct NodeSeqs {
    pub started: BTreeMap<String, u64>,
    pub finished: BTreeMap<String, u64>,
    pub hops: BTreeMap<(String, String), u64>,
}

/// One revival in force (see [`RunState::revivals`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RevivalEntry {
    pub from_node: String,
    pub nodes: BTreeSet<String>,
}

/// One node's judge-edge answers, as journaled by the `judge_edge` decision
/// asked when it finished.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JudgeEdgeAnswers {
    /// The source's execution the answers belong to (its finish count).
    pub execution: u32,
    /// The answers were applied: the `judge_edge` use was at enforce.
    pub applied: bool,
    /// The "yes" probability by question id (`edge_<index>`).
    pub p: BTreeMap<String, f64>,
}

/// The old resume wrote `run_paused { reason: "resume from X" }` as a marker
/// and kept running: not a pause.
pub(crate) fn is_legacy_resume_marker(p: &EventPayload) -> bool {
    matches!(p, EventPayload::RunPaused { reason, by: None, .. } if reason.starts_with("resume from "))
}

impl RunState {
    /// The status routing and join readiness read: `Pending` for a node a
    /// re-run revived and that has not started again, else its folded status.
    pub fn current_status(&self, node: &str) -> NodeStatus {
        match self.revived.contains(node) {
            true => NodeStatus::Pending,
            false => self.nodes.get(node).copied().unwrap_or(NodeStatus::Pending),
        }
    }

    /// Whether `node` was revived and still carries a finished result from an
    /// earlier pass: a hop into it recorded before the revival is not a reason
    /// to run it again (see `parallel::pending_heads`).
    ///
    /// A fresh delivery since the revival (a hop journaled after it) is such
    /// a reason, so a node that received one is not counted here.
    pub fn revived_after_finish(&self, node: &str) -> bool {
        self.revived.contains(node)
            && self.nodes.get(node).is_some_and(|s| s.is_finished())
            && !self
                .delivered_since_revival
                .iter()
                .any(|(f, t)| t == node && self.routes(f, t))
    }

    /// Whether the journaled hop `from -> to` counts for routing: a revived
    /// node's hops are its earlier pass's and do not, until it runs again.
    pub fn routes(&self, from: &str, to: &str) -> bool {
        !self.revived.contains(from)
            && self
                .journaled_hops
                .contains(&(from.to_string(), to.to_string()))
    }

    /// Whether the journaled hop `from -> to` was taken by `from`'s latest
    /// execution: recorded after `from` last started. A hop of an earlier
    /// loop round is history, not where the node goes now. With no position
    /// on record (a state built by hand) the hop counts.
    pub fn hop_of_latest_run(&self, from: &str, to: &str) -> bool {
        let hop = self.seqs.hops.get(&(from.to_string(), to.to_string()));
        match (hop, self.seqs.started.get(from)) {
            (Some(hop), Some(start)) => hop > start,
            _ => true,
        }
    }

    /// Whether `join` already consumed `source`'s latest finish: the join's
    /// latest execution finished after it, and the join has neither started
    /// again nor been revived since. Inside a loop such a finish belongs to
    /// the round before (`parallel::arrival` asks only for a source in the
    /// join's own cycle). A join still running (a crash or a pause while it
    /// ran, then a resume) or one a re-run revived has consumed nothing.
    pub fn join_consumed(&self, source: &str, join: &str) -> bool {
        let Some(done) = self.seqs.finished.get(source) else {
            return false;
        };
        let Some(join_done) = self.seqs.finished.get(join) else {
            return false;
        };
        join_done > done
            && self.seqs.started.get(join).is_none_or(|s| s < join_done)
            && !self.revived.contains(join)
    }

    /// Applies a revival without a start of its own (tests, mostly): see
    /// [`RunState::revive_from`].
    pub fn revive<'a>(&mut self, nodes: impl IntoIterator<Item = &'a String>) {
        self.revive_from("", nodes, &[]);
    }

    /// Applies the revival of `from_node`: each of `nodes` reads as not yet
    /// run for routing until it starts again, and `from_node` is a re-run the
    /// drive owes ([`RunState::revival_starts`]). Nothing is deleted: the
    /// overlay is applied on read ([`RunState::current_status`],
    /// [`RunState::routes`]).
    ///
    /// `superseded` are the owed re-runs this explicit request replaces (the
    /// latest choice wins): their revivals end, so the nodes only they
    /// covered read their earlier results and hops again. An owed re-run this
    /// revival covers is owed no longer either: this re-run reaches it.
    pub fn revive_from<'a>(
        &mut self,
        from_node: &str,
        nodes: impl IntoIterator<Item = &'a String>,
        superseded: &[String],
    ) {
        let nodes: BTreeSet<String> = nodes.into_iter().cloned().collect();
        self.revivals
            .retain(|r| !(superseded.contains(&r.from_node) && r.nodes.contains(&r.from_node)));
        for r in &mut self.revivals {
            if nodes.contains(&r.from_node) {
                r.nodes.remove(&r.from_node);
            }
        }
        self.revivals.push(RevivalEntry {
            from_node: from_node.to_string(),
            nodes,
        });
        self.settle_revivals();
    }

    /// A node started or finished again: it leaves every revival, and the
    /// hops of its earlier pass are dropped for good.
    fn ran_again(&mut self, node: &str) {
        if !self.revived.contains(node) {
            return;
        }
        self.journaled_hops.retain(|(from, _)| from != node);
        self.delivered_since_revival.retain(|(_, t)| t != node);
        for r in &mut self.revivals {
            r.nodes.remove(node);
        }
        self.settle_revivals();
    }

    fn settle_revivals(&mut self) {
        self.revivals.retain(|r| !r.nodes.is_empty());
        self.revived = self
            .revivals
            .iter()
            .flat_map(|r| r.nodes.iter().cloned())
            .collect();
        self.revival_starts = self
            .revivals
            .iter()
            .filter(|r| r.nodes.contains(&r.from_node))
            .map(|r| r.from_node.clone())
            .collect();
        let revived = &self.revived;
        self.delivered_since_revival
            .retain(|(_, t)| revived.contains(t));
    }

    /// The run reached a terminal state: a re-run it still owed is owed no
    /// longer (a plain resume of a finished run re-runs nothing), so those
    /// revivals end.
    fn end_owed_revivals(&mut self) {
        self.revivals.retain(|r| !r.nodes.contains(&r.from_node));
        self.settle_revivals();
    }

    pub fn fold(events: &[Event]) -> RunState {
        let mut s = RunState::default();
        // Open attempts by (node, item). Closed by attempt_finished, and every
        // attempt of a node by its node_finished.
        let mut open: BTreeSet<(String, Option<String>)> = BTreeSet::new();
        // The run status came from the old resume's `run_paused { reason:
        // "resume from X" }` marker, which never meant a pause.
        let mut legacy_marker = false;
        for e in events {
            match &e.payload {
                EventPayload::RunStarted { .. } => s.run_status = RunStatus::Running,
                EventPayload::RunProvenance { .. } => {}
                EventPayload::NodeStarted { node, .. } => {
                    s.seqs.started.insert(node.clone(), e.seq);
                    s.nodes.insert(node.clone(), NodeStatus::Running);
                    s.route_suppressed.remove(node);
                    s.refused_joins.remove(node);
                    s.ran_again(node);
                }
                EventPayload::AttemptStarted {
                    node,
                    attempt,
                    item,
                    ..
                } => {
                    s.nodes.insert(node.clone(), NodeStatus::Running);
                    // A list item's attempt numbers are its own.
                    if item.is_none() {
                        s.attempts.insert(node.clone(), *attempt);
                    }
                    open.insert((node.clone(), item.clone()));
                }
                // The attempt stays open until its `attempt_finished`: an exit
                // the drive never finished is a crash to an offline reader.
                // Live reporting reads this event (see `liveness`).
                EventPayload::AttemptExited { .. } => {}
                EventPayload::AttemptFinished {
                    node,
                    rejected_output,
                    item,
                    ..
                } => {
                    open.remove(&(node.clone(), item.clone()));
                    // A list item's rejection is its own, not the node's.
                    if item.is_none()
                        && let Some(text) = rejected_output
                    {
                        s.rejected_outputs.insert(node.clone(), text.clone());
                    }
                }
                EventPayload::NodeFinished {
                    node,
                    status,
                    output,
                    ..
                } => {
                    open.retain(|(n, _)| n != node);
                    s.seqs.finished.insert(node.clone(), e.seq);
                    s.ran_again(node);
                    s.nodes.insert(node.clone(), NodeStatus::from_label(status));
                    s.outputs.insert(node.clone(), output.clone());
                    s.last_node = Some(node.clone());
                    *s.finished_counts.entry(node.clone()).or_insert(0) += 1;
                }
                p @ EventPayload::RunPaused { .. } => {
                    s.run_status = RunStatus::Paused;
                    legacy_marker = is_legacy_resume_marker(p);
                }
                EventPayload::RunResumed { .. } => s.run_status = RunStatus::Running,
                // A queued run is `Running` and stays that way: it was
                // admitted, its parameters are on disk, and it is waiting for
                // the workdir rather than for anybody to decide anything.
                EventPayload::RunQueued { .. } => {}
                EventPayload::WorktreeResolved { path, .. } => {
                    if s.worktree.is_none() {
                        s.worktree = Some(path.clone());
                    }
                }
                EventPayload::RunFinished { outcome } => {
                    s.run_status = match outcome.as_str() {
                        "succeeded" => RunStatus::Succeeded,
                        _ => RunStatus::Failed,
                    };
                    s.end_owed_revivals();
                }
                EventPayload::RetryStarted { .. } | EventPayload::FallbackTriggered { .. } => {}
                EventPayload::RunAborted { .. } => {
                    s.run_status = RunStatus::Aborted;
                    s.end_owed_revivals();
                }
                EventPayload::WakeRaised { .. } => {}
                // A question and its answer are audit-only channel mirrors
                // (spec 2026-07-20-interactive-nodes): they do not change
                // node status. The pending state itself is derived by
                // `progress::from_run_dir` directly from the questions.jsonl
                // / answers.jsonl channel files, not from these events.
                EventPayload::QuestionAsked { .. } | EventPayload::QuestionAnswered { .. } => {}
                EventPayload::SupervisorAction { .. } => {}
                EventPayload::SupervisorLost { .. } => {}
                EventPayload::PatchApplied { .. }
                | EventPayload::PatchRejected { .. }
                | EventPayload::ProfileRebound { .. }
                | EventPayload::RebindRejected { .. }
                | EventPayload::RunMigrated { .. }
                | EventPayload::VersionPromoted { .. }
                | EventPayload::CandidatePromoted { .. }
                | EventPayload::CandidateRejected { .. } => {}
                EventPayload::WaitStarted { .. }
                | EventPayload::WaitSignalled { .. }
                | EventPayload::WaitTimeout { .. } => {}
                // Context compaction is a materialized rendering artifact,
                // it does not affect run state.
                EventPayload::ContextCompacted { .. } => {}
                // A decision record changes no run state (a shadow answer is
                // journal only), except the judge-edge answers routing reads
                // back (issue #165 Part 7): edge selection stays a pure fold.
                EventPayload::DecisionMade {
                    use_site,
                    node: Some(node),
                    attempt: Some(execution),
                    answers,
                    applied,
                    error: None,
                    ..
                } if use_site == "judge_edge" => {
                    s.judge_edges.insert(
                        node.clone(),
                        JudgeEdgeAnswers {
                            execution: *execution,
                            applied: *applied,
                            p: answers
                                .iter()
                                .filter_map(|(k, a)| a.p.map(|p| (k.clone(), p)))
                                .collect(),
                        },
                    );
                }
                EventPayload::DecisionMade { .. } => {}
                // An accepted environment drift is an audit record, it does not change state.
                EventPayload::EnvironmentDriftAccepted { .. } => {}
                // Progress is an audit-only cycle report, it does not change state.
                EventPayload::RunProgress { .. } => {}
                // A child-run marker is an audit record; the node's own status
                // events carry the run-state effect.
                EventPayload::ChildRunStarted { .. } => {}
                EventPayload::RunContinuedFrom { .. } | EventPayload::RunSupersededBy { .. } => {}
                // A connector call is an audit record (spec 6.2); it does not
                // change run state.
                EventPayload::ConnectorCall { .. } => {}
                // An inbox gap is a reporting record; the run surfaces read it straight
                // from the journal, the state fold carries no inbox cursor.
                EventPayload::InboxGap { .. } => {}
                // The send ledger is folded by `connector::intent::ledger`;
                // it carries no run-state effect.
                EventPayload::SendIntentRecorded { .. } | EventPayload::SendIntentSettled { .. } => {}
                // Node-cache events (spec 2026-07-19) are audit records: a hit
                // is reported alongside the node's own NodeStarted/NodeFinished
                // (which carry the run-state effect), and miss/stored/rejected
                // only annotate the admission decision.
                EventPayload::NodeCacheHit { .. }
                | EventPayload::NodeCacheMiss { .. }
                | EventPayload::NodeCacheStored { .. }
                | EventPayload::NodeCacheRejected { .. } => {}
                // A join write-off is an observability record of a readiness
                // verdict (spec 2026-08-05, Task 4): the run-state effect is the
                // join's own execution, journaled right after it. Its revival
                // by a re-run (issue #203) is the same kind of record: the
                // readiness is recomputed from the revived fold, not from it.
                EventPayload::JoinInputDead { .. } | EventPayload::JoinInputRevived { .. } => {}
                // A re-run's revival (issue #203): routing reads the nodes as
                // not yet run until each starts again; history stays.
                EventPayload::NodesRevived {
                    from_node,
                    nodes,
                    superseded,
                } => s.revive_from(from_node, nodes, superseded),
                // Fork and join failure options (issue #195): the routing they
                // decided against is folded so edge selection stays pure.
                EventPayload::BranchFailed { node, policy, .. } => {
                    if policy == apb_core::schema::BranchFailurePolicy::FailFast.as_str() {
                        s.route_suppressed.insert(node.clone());
                    }
                }
                EventPayload::BranchCancelled { node, .. } => {
                    s.route_suppressed.insert(node.clone());
                }
                EventPayload::JoinRefused { node, .. } => {
                    s.refused_joins.insert(node.clone());
                }
                // A declared deliverable that was not captured is a warning about
                // the workspace, not a run-state transition: the node succeeded
                // and its `NodeFinished` (with whatever WAS captured) carries the
                // state effect.
                EventPayload::DeliverableMissing { .. }
                | EventPayload::OutputFieldsMissing { .. }
                | EventPayload::SessionHandoff { .. }
                // Host execution mode (0.23.0): the attempt events around a
                // host task carry the state; the task records are views.
                | EventPayload::HostTaskRequested { .. }
                | EventPayload::HostTaskSubmitted { .. }
                | EventPayload::ExecutionFallback { .. } => {}
                // 0.23.0 records (provenance, goal criteria, protected paths):
                // reports about the tree and the run's goal, no state effect.
                EventPayload::ArtifactsCommitted { .. }
                | EventPayload::GoalChecked { .. }
                | EventPayload::ProtectedPathsModified { .. } => {}
                // The baseline snapshots, their comparisons and an attempt's
                // notes are reports; `crate::baseline::BaselineState` folds
                // the baseline for the drive.
                EventPayload::BaselineObserved { .. }
                | EventPayload::BaselineCompared { .. }
                | EventPayload::AttemptNotes { .. }
                | EventPayload::AgentEnvApplied { .. } => {}
                // A list visit's records; `crate::list::ListFold` folds them.
                EventPayload::ListStarted { .. }
                | EventPayload::ListBatchStarted { .. }
                | EventPayload::ListItemSettled { .. } => {}
                // The budget and its gate; `crate::budget::BudgetFold` folds
                // them.
                EventPayload::BudgetSet { .. } | EventPayload::BudgetGate { .. } => {}
                // The notification subscription; `crate::notify::NotifyState`
                // folds it.
                EventPayload::NotifySet { .. } => {}
                // Every hop the drive loop journaled, whatever kind of edge it
                // crossed. The counting rule is on the flags: a policy route
                // traversed no declared edge, and an `uncounted` record is a
                // routing DECISION rather than the single counting site, so
                // neither spends a bounded edge's traversal budget. An old
                // record carries neither flag and still counts, exactly as
                // before.
                EventPayload::EdgeTraversed {
                    from,
                    to,
                    via_policy,
                    uncounted,
                } => {
                    s.journaled_hops.insert((from.clone(), to.clone()));
                    s.seqs.hops.insert((from.clone(), to.clone()), e.seq);
                    if s.revived.contains(to) && !s.revived.contains(from) {
                        s.delivered_since_revival
                            .insert((from.clone(), to.clone()));
                    }
                    if !via_policy && !uncounted {
                        *s.edge_counts.entry((from.clone(), to.clone())).or_insert(0) += 1;
                    }
                }
                // The explanatory record for an abnormal termination (issue
                // #42 finding 3): does not itself change node/run status
                // (the terminal `run_finished`/`node_finished` right next to
                // it does that) - it only carries the reason forward.
                EventPayload::RunError { node, reason } => {
                    s.failure_reason = Some(FailureReason {
                        node: node.clone(),
                        reason: reason.clone(),
                    });
                }
                EventPayload::ReviewRequested { .. } | EventPayload::ReviewWithdrawn { .. } => {}
                EventPayload::ReviewDecided {
                    node,
                    decision,
                    note,
                } => {
                    s.reviews.insert(
                        node.clone(),
                        ReviewDecision {
                            decision: decision.clone(),
                            note: note.clone(),
                        },
                    );
                }
            }
        }
        // Attempts still open after the last event: interrupted.
        if !open.is_empty() {
            for (node, _) in &open {
                s.nodes.insert(node.clone(), NodeStatus::Interrupted);
            }
            // `Aborted` belongs in this exempt list next to the other explicit
            // terminal verdicts: the shape a crashed driver leaves behind is an
            // open `attempt_started`, and that is exactly the run `stop_run`
            // finalizes with `RunAborted`. Without the exemption the fold
            // immediately downgraded that run back to `Interrupted`, so the
            // stop never became visible (run_status, `apb runs`, the dashboard,
            // `doctor --run` all still read interrupted) and a second stop
            // passed the terminal check again and appended a second
            // `RunAborted`.
            //
            // `Paused` is exempt for the same reason (issue #200): a driver
            // closes its attempts before it journals `run_paused`, so an
            // attempt still open under a pause is one whose driver died, and
            // the pause was journaled for the driverless run afterwards
            // (`request_pause`, `resume --all`). The run is paused; the node
            // still reads interrupted and the resume reaps the attempt. The
            // old resume marker is not a pause: an older journal whose driver
            // died after it still reads interrupted.
            let exempt = match s.run_status {
                RunStatus::Succeeded | RunStatus::Failed | RunStatus::Aborted => true,
                RunStatus::Paused => !legacy_marker,
                _ => false,
            };
            if !exempt {
                s.run_status = RunStatus::Interrupted;
            }
        }
        s
    }
}
