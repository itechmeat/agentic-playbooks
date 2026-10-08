//! Fork branch-failure policies and join refusal on the drive side (issue
//! #195). The structure of a fork (its branches and joins) lives in
//! `apb_core::fork`; this module journals what a policy decides and rewires the
//! frontier. Shares the parent module's imports via `use super::*`.
//!
//! The order the drive calls these in is load-bearing:
//!
//! 1. [`begin`] right after the failed node's `NodeFinished`: journals
//!    `branch_failed` (and for `fail_fast` the hop to `on_failure`), so the fold
//!    already knows the failed node's own edges are off the table.
//! 2. The ordinary frontier advance of every node that finished.
//! 3. [`settle`]: cancels whatever the advance left in the fork's branches and
//!    joins, then routes (pushes `on_failure`, or re-advances the failed node so
//!    a failure sink that was waiting for the cancelled siblings fires now).

use super::*;

use apb_core::fork::{ForkRegion, governing_fork};
use apb_core::schema::BranchFailurePolicy;

/// A branch failure a fork's policy takes over.
pub(crate) struct ForkFailure {
    pub region: ForkRegion,
    pub policy: BranchFailurePolicy,
    /// The branch node that failed.
    pub node: String,
    /// `fail_fast`: the fork's `on_failure`.
    pub target: Option<String>,
}

impl ForkFailure {
    /// Whether `node` is one the failure cancels: any other node of the fork's
    /// branches or joins.
    pub(crate) fn in_scope(&self, node: &str) -> bool {
        node != self.node
            && (self.region.branches.contains(node) || self.region.joins.contains(node))
    }

    pub(crate) fn fails_fast(&self) -> bool {
        self.policy == BranchFailurePolicy::FailFast
    }
}

/// The policy that governs `node`'s failure, if any. Only an autonomous run
/// applies one: a supervised run parks every failure for its supervisor, who
/// decides what happens to the other branches.
pub(crate) fn detect(
    playbook: &Playbook,
    node: &str,
    status: NodeStatus,
    mode: RunMode,
) -> Option<ForkFailure> {
    if mode.parks_on_failure() || !matches!(status, NodeStatus::Failed | NodeStatus::TimedOut) {
        return None;
    }
    let (region, policy) = governing_fork(playbook, node)?;
    let target = match policy {
        BranchFailurePolicy::FailFast => playbook
            .node(&region.fork)
            .and_then(|n| n.fork.as_ref())
            .and_then(|f| f.on_failure.clone()),
        _ => None,
    };
    Some(ForkFailure {
        region,
        policy,
        node: node.to_string(),
        target,
    })
}

/// Step 1: journals the policy's decision.
pub(crate) fn begin(log: &mut EventLog, f: &ForkFailure) -> Result<(), EngineError> {
    log.append(EventPayload::BranchFailed {
        fork: f.region.fork.clone(),
        node: f.node.clone(),
        policy: f.policy.as_str().to_string(),
        target: f.target.clone(),
    })?;
    if let Some(target) = &f.target {
        // Journaled as the policy hop it is, so the frontier a resume rebuilds
        // (`parallel::pending_heads`) still has the failure target.
        journal_policy_route(log, &f.node, target)?;
    }
    Ok(())
}

fn cancel_one(log: &mut EventLog, f: &ForkFailure, node: &str) -> Result<(), EngineError> {
    log.append(EventPayload::BranchCancelled {
        fork: f.region.fork.clone(),
        node: node.to_string(),
        failed_node: f.node.clone(),
    })?;
    Ok(())
}

/// Step 1b (a batch only): the members this failure killed or kept from
/// starting. Their `cancelled` finish is already journaled, so they only get
/// their `branch_cancelled`, which takes their own edges off the table before
/// the advance weighs the joins they fed.
pub(crate) fn mark_interrupted(
    log: &mut EventLog,
    f: &ForkFailure,
    interrupted: &[String],
) -> Result<(), EngineError> {
    for n in interrupted {
        cancel_one(log, f, n)?;
    }
    Ok(())
}

/// Step 3: cancels what is left of the fork and routes the run.
///
/// Every branch or join node still waiting (a frontier head, or a join a
/// branch already delivered into) is written off as cancelled. The routing
/// targets themselves are never cancelled.
pub(crate) fn settle(
    playbook: &Playbook,
    run_dir: &Path,
    log: &mut EventLog,
    f: &ForkFailure,
    frontier: &mut Vec<String>,
) -> Result<(), EngineError> {
    let state = RunState::fold(&read_all(run_dir)?);
    let keep: Vec<String> = match &f.target {
        Some(t) => vec![t.clone()],
        None => parallel::successors(playbook, &f.node, &state),
    };
    let mut cancel: Vec<String> = Vec::new();
    let waiting = frontier
        .iter()
        .cloned()
        .chain(parallel::pending_heads(playbook, &state));
    for n in waiting {
        if f.in_scope(&n) && !keep.contains(&n) && !cancel.contains(&n) {
            cancel.push(n);
        }
    }
    frontier.retain(|n| !cancel.contains(n));
    for n in &cancel {
        log.append(EventPayload::NodeStarted {
            node: n.clone(),
            attempt: 1,
        })?;
        log.append(EventPayload::NodeFinished {
            node: n.clone(),
            status: NodeStatus::Cancelled.as_str().into(),
            attempt: 1,
            output: "cancelled".into(),
            artifacts: Vec::new(),
        })?;
        cancel_one(log, f, n)?;
    }
    match &f.target {
        Some(target) => {
            if !frontier.contains(target) {
                frontier.push(target.clone());
            }
        }
        None => {
            // A failure sink fed by the cancelled siblings too was not ready at
            // the ordinary advance; with them cancelled it is now.
            let after = RunState::fold(&read_all(run_dir)?);
            advance_frontier(playbook, &f.node, &after, frontier, &[], log)?;
        }
    }
    Ok(())
}

/// The barrier verdict of a join about to run as `current`, when it must not
/// run: `ReadyFailure` (an explicit join with a failed input) or `Refused`
/// (`require: all_succeeded` with an input that did not arrive succeeded).
pub(crate) fn failing_barrier(
    playbook: &Playbook,
    node: &str,
    state: &RunState,
    active: &[String],
) -> Option<JoinReadiness> {
    if !parallel::is_join(playbook, node) {
        return None;
    }
    match parallel::join_readiness(playbook, node, state, active) {
        v @ (JoinReadiness::ReadyFailure | JoinReadiness::Refused) => Some(v),
        _ => None,
    }
}

/// Journals a `require: all_succeeded` refusal and returns the reason, which is
/// also the refused node's output.
pub(crate) fn refuse_join(
    log: &mut EventLog,
    playbook: &Playbook,
    node: &str,
    state: &RunState,
    active: &[String],
) -> Result<String, EngineError> {
    let sources = parallel::refused_inputs(playbook, node, state, active);
    let reason = format!(
        "join `{node}` refused (require: all_succeeded): {} did not arrive succeeded",
        sources
            .iter()
            .map(|s| format!("`{s}`"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    log.append(EventPayload::JoinRefused {
        node: node.to_string(),
        sources,
        reason: reason.clone(),
    })?;
    Ok(reason)
}
