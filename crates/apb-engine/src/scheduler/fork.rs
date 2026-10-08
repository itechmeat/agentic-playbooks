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

use apb_core::fork::{ForkRegion, enclosing_forks, fork_region};
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

fn on_failure_of(playbook: &Playbook, fork: &str) -> Option<String> {
    playbook
        .node(fork)
        .and_then(|n| n.fork.as_ref())
        .and_then(|f| f.on_failure.clone())
}

/// The policy that governs `node`'s failure, if any. Only an autonomous run
/// applies one: a supervised run parks every failure for its supervisor, who
/// decides what happens to the other branches.
///
/// The innermost fork with a policy decides first. When the routing it leaves
/// the failure with (its `on_failure`, or the failed node's own successors
/// under `cancel_siblings`) has no target inside an enclosing fork's branches,
/// the failure was not handled inside that fork either, and it escalates: the
/// enclosing fork's branches are cancelled too, and an enclosing `fail_fast`
/// takes over the routing with its own `on_failure`. `state` must already
/// carry the failed node's finish.
pub(crate) fn detect(
    playbook: &Playbook,
    node: &str,
    status: NodeStatus,
    mode: RunMode,
    state: &RunState,
) -> Option<ForkFailure> {
    if mode.parks_on_failure() || !matches!(status, NodeStatus::Failed | NodeStatus::TimedOut) {
        return None;
    }
    let mut chain = enclosing_forks(playbook, node).into_iter();
    let (region, policy) = chain.next()?;
    let target = match policy {
        BranchFailurePolicy::FailFast => on_failure_of(playbook, &region.fork),
        _ => None,
    };
    let mut f = ForkFailure {
        region,
        policy,
        node: node.to_string(),
        target,
    };
    for (outer, outer_policy) in chain {
        let routes: Vec<String> = match &f.target {
            Some(t) => vec![t.clone()],
            None => parallel::successors(playbook, node, state),
        };
        if routes.iter().any(|t| outer.contains(t)) {
            break;
        }
        let target = match outer_policy {
            BranchFailurePolicy::FailFast => on_failure_of(playbook, &outer.fork),
            _ => f.target.take(),
        };
        f = ForkFailure {
            region: outer,
            policy: match target.is_some() {
                true => BranchFailurePolicy::FailFast,
                false => BranchFailurePolicy::CancelSiblings,
            },
            node: node.to_string(),
            target,
        };
    }
    Some(f)
}

/// Where the drive leaves the reason a fork policy is about to interrupt a
/// running node, for the host adapter's closing note (`fork_cancel/<node>`).
fn cancel_note_path(run_dir: &Path, node: &str) -> Option<PathBuf> {
    apb_core::registry::is_safe_segment(node).then(|| run_dir.join("fork_cancel").join(node))
}

/// Records why `node` is about to be interrupted, before its cancel flag is
/// set. Best effort: a missing note only makes the host task's closing note
/// generic.
pub(crate) fn note_cancel(run_dir: &Path, f: &ForkFailure, node: &str) {
    if let Some(path) = cancel_note_path(run_dir, node) {
        let note = format!(
            "cancelled by fork `{}` (on_branch_failure: {}) after `{}` failed",
            f.region.fork,
            f.policy.as_str(),
            f.node
        );
        let _ = std::fs::create_dir_all(run_dir.join("fork_cancel"));
        let _ = apb_core::fsutil::atomic_write_private(&path, note.as_bytes());
    }
}

/// The note a cancelled host task closes with: the fork policy's reason when
/// one was recorded for `node`, otherwise the stop.
pub(crate) fn cancel_note(run_dir: &Path, node: &str) -> String {
    cancel_note_path(run_dir, node)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_else(|| "the run was stopped".to_string())
}

/// Drops a recorded cancel reason once the member it was for has returned.
pub(crate) fn clear_cancel_note(run_dir: &Path, node: &str) {
    if let Some(path) = cancel_note_path(run_dir, node) {
        let _ = std::fs::remove_file(path);
    }
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
    run_dir: &Path,
    log: &mut EventLog,
    f: &ForkFailure,
    interrupted: &[String],
) -> Result<(), EngineError> {
    for n in interrupted {
        cancel_one(log, f, n)?;
        clear_cancel_note(run_dir, n);
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
    cancel_waiting(playbook, run_dir, log, f, frontier)?;
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

/// The cancellation half of [`settle`]: writes off every branch or join node
/// still waiting, never a routing target. Returns the nodes written off.
fn cancel_waiting(
    playbook: &Playbook,
    run_dir: &Path,
    log: &mut EventLog,
    f: &ForkFailure,
    frontier: &mut Vec<String>,
) -> Result<Vec<String>, EngineError> {
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
    Ok(cancel)
}

/// Finishes a fork policy a previous drive left half done, before a drive
/// over an existing run reads its starting state (issue #195). Two shapes:
///
/// - a branch node finished failed under a fork policy and the driver died
///   before `branch_failed` was journaled: the policy is applied now, as long
///   as nothing ran after the failure (no node started, no wake raised for it);
/// - `branch_failed` is journaled but the cancellation never completed (a
///   batch that paused, or a driver that died): nothing but cancellations ran
///   since, so the branch and join nodes still waiting are written off now.
///   The routing needs no help: the `on_failure` hop is journaled and the
///   failed node's own targets are pending heads.
///
/// Returns the nodes written off.
pub(crate) fn recover(
    playbook: &Playbook,
    run_dir: &Path,
    log: &mut EventLog,
    mode: RunMode,
) -> Result<Vec<String>, EngineError> {
    let events = read_all(run_dir)?;
    let state = RunState::fold(&events);
    for (node, status) in &state.nodes {
        let Some(at) = last_finish(&events, node) else {
            continue;
        };
        let untouched = events[at + 1..].iter().all(|e| match &e.payload {
            EventPayload::NodeStarted { .. } => false,
            EventPayload::WakeRaised { node: n, .. } => n != node,
            EventPayload::BranchFailed { node: n, .. } => n != node,
            _ => true,
        });
        if untouched && let Some(f) = detect(playbook, node, *status, mode, &state) {
            begin(log, &f)?;
        }
    }
    let events = read_all(run_dir)?;
    let mut written_off: Vec<String> = Vec::new();
    for (i, e) in events.iter().enumerate() {
        let EventPayload::BranchFailed {
            fork,
            node,
            policy,
            target,
        } = &e.payload
        else {
            continue;
        };
        if last_finish(&events, node).is_some_and(|at| at > i) || !unsettled(&events, i) {
            continue;
        }
        let Some(region) = fork_region(playbook, fork) else {
            continue;
        };
        let f = ForkFailure {
            region,
            policy: match policy.as_str() {
                "fail_fast" => BranchFailurePolicy::FailFast,
                _ => BranchFailurePolicy::CancelSiblings,
            },
            node: node.clone(),
            target: target.clone(),
        };
        written_off.extend(cancel_waiting(playbook, run_dir, log, &f, &mut Vec::new())?);
    }
    Ok(written_off)
}

fn last_finish(events: &[Event], node: &str) -> Option<usize> {
    events.iter().rposition(
        |e| matches!(&e.payload, EventPayload::NodeFinished { node: n, .. } if n == node),
    )
}

/// Whether only cancellations ran after the `branch_failed` at `at`: every
/// node started since finished `cancelled`.
fn unsettled(events: &[Event], at: usize) -> bool {
    let rest = &events[at + 1..];
    rest.iter().enumerate().all(|(i, e)| match &e.payload {
        EventPayload::NodeStarted { node, .. } => {
            rest[i + 1..].iter().find_map(|x| match &x.payload {
                EventPayload::NodeFinished {
                    node: n, status, ..
                } if n == node => Some(status == NodeStatus::Cancelled.as_str()),
                _ => None,
            }) == Some(true)
        }
        _ => true,
    })
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
