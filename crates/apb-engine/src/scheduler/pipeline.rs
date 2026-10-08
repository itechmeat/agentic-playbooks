//! Pipelined batch admission (issue #195): routing a batch member the moment
//! it succeeds, so its newly ready successors start while the other members
//! still run. Shares the parent module's imports via `use super::*`.

use super::*;

/// The running batch a member is routed into: its members (in admission
/// order), the slot queue, and the per-member cancel flags.
pub(crate) struct BatchSlots<'a> {
    /// The members that have ended, in the order they did.
    pub finished: &'a [String],
    pub batch: &'a mut Vec<String>,
    pub queue: &'a mut std::collections::VecDeque<String>,
    pub member_flags: &'a mut BTreeMap<String, Arc<AtomicBool>>,
    pub member_fans: &'a mut Vec<crate::stop::FanoutGuard>,
    pub fanout: &'a Arc<crate::stop::CancelFanout>,
    pub steps: &'a mut usize,
}

/// Whether `node`, which just succeeded, delivered into a `join: any` that is
/// now won: the batch's other members are then cancelled.
pub(crate) fn feeds_ready_any(
    playbook: &Playbook,
    node: &str,
    state: &RunState,
    frontier: &[String],
    batch: &[String],
) -> bool {
    let active = active_set(node, frontier, batch);
    parallel::successors(playbook, node, state)
        .into_iter()
        .any(|s| {
            parallel::is_join(playbook, &s)
                && parallel::join_mode(playbook, &s) == parallel::JoinMode::Any
                && matches!(
                    parallel::join_readiness(playbook, &s, state, &active),
                    JoinReadiness::ReadySuccess
                )
        })
}

/// Routes the batch member `node`, which just succeeded, and, when `admit`
/// is set, admits its newly ready batchable successors into the running
/// batch: each joins `batch` (so every later liveness question counts it),
/// gets its own cancel flag registered with the run-level stop fanout, and is
/// queued for a slot. Everything else the routing produces stays in the
/// frontier for after the batch, where the sequential arm (with its control
/// scan, heartbeat and compaction) takes over.
///
/// Returns `false` without routing when a successor is an explicit
/// `join: any`: its race cancels frontier heads, and the members still
/// running must never be taken by it; the batch tail routes such a member.
///
/// A successor that is already a member is not pushed again, unless it was
/// admitted before `node` (or is `node`): that is a loop re-entry, and it stays
/// in the frontier for after the batch. One execution per member per batch.
pub(crate) fn pipeline_member(
    playbook: &Playbook,
    run_dir: &Path,
    journal: &Journal<'_>,
    node: &str,
    frontier: &mut Vec<String>,
    slots: BatchSlots<'_>,
    admit: bool,
) -> Result<bool, EngineError> {
    let state = RunState::fold(&read_all(run_dir)?);
    let any_join = parallel::selected_edges(playbook, node, &state)
        .iter()
        .any(|e| {
            matches!(
                parallel::join_kind(playbook, &e.to),
                Some(parallel::JoinKind::Explicit(parallel::JoinMode::Any))
            )
        });
    if any_join {
        return Ok(false);
    }
    let finished = slots.finished;
    let me = slots.batch.iter().position(|m| m == node).unwrap_or(0);
    // The members admitted after `node`, and those still queued or running,
    // sit in the frontier for the advance, so a successor that is one of them
    // reads as delivered, not as new work.
    let later: Vec<String> = slots
        .batch
        .iter()
        .enumerate()
        .filter(|(i, m)| (*i > me || !finished.contains(*m)) && *m != node)
        .map(|(_, m)| m.clone())
        .filter(|m| !frontier.contains(m))
        .collect();
    frontier.extend(later.iter().cloned());
    let routed = journal
        .with_log(|log| advance_frontier(playbook, node, &state, frontier, slots.batch, log));
    frontier.retain(|n| !later.contains(n));
    routed?;
    if !admit {
        return Ok(true);
    }
    // A successor that leads back to a member still queued or running (a
    // cycle through the batch, `F -> {A, B} -> C -> F`) is not admitted: it
    // waits for the batch to end, as it did when the batch was routed only at
    // its end, so a new pass of the loop never starts beside the old one.
    let pending: Vec<&String> = slots
        .batch
        .iter()
        .filter(|m| !finished.contains(*m) && m.as_str() != node)
        .collect();
    let ready: Vec<String> = frontier
        .iter()
        .filter(|n| is_batchable(playbook, n) && !slots.batch.contains(*n))
        .filter(|n| {
            let reach = apb_core::graphutil::reachable(playbook, &[n.as_str()]);
            !pending.iter().any(|m| reach.contains(m.as_str()))
        })
        .cloned()
        .collect();
    frontier.retain(|n| !ready.contains(n));
    for n in ready {
        let flag = Arc::new(AtomicBool::new(false));
        slots.member_fans.push(slots.fanout.register(&flag));
        slots.member_flags.insert(n.clone(), flag);
        slots.batch.push(n.clone());
        slots.queue.push_back(n);
        *slots.steps += 1;
    }
    Ok(true)
}

/// Re-offers the joins the members routed mid-batch delivered into while a
/// sibling was still running (issue #195). Such a join was `NotReady` at that
/// advance; once the batch has ended, a sibling that died or routed elsewhere
/// may have made it ready, and nothing else would offer it again (routing the
/// whole batch at its end used to see every sibling finished at once). Only
/// joins are re-offered, never a plain successor that already ran as a member.
pub(crate) fn reoffer_joins(
    playbook: &Playbook,
    run_dir: &Path,
    log: &mut EventLog,
    advanced: &BTreeSet<String>,
    batch: &[String],
    frontier: &mut Vec<String>,
) -> Result<(), EngineError> {
    if advanced.is_empty() {
        return Ok(());
    }
    let events = read_all(run_dir)?;
    let state = RunState::fold(&events);
    for m in batch.iter().filter(|m| advanced.contains(*m)) {
        // The member's own finish: a join started after it was offered already.
        let since = events
            .iter()
            .rposition(
                |e| matches!(&e.payload, EventPayload::NodeFinished { node, .. } if node == m),
            )
            .unwrap_or(0);
        for e in parallel::selected_edges(playbook, m, &state) {
            let s = &e.to;
            let started_since = events[since..]
                .iter()
                .any(|e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == s));
            if !parallel::is_join(playbook, s)
                || frontier.contains(s)
                || batch.contains(s)
                || started_since
            {
                continue;
            }
            let active = active_set(m, frontier, batch);
            if matches!(
                parallel::join_readiness(playbook, s, &state, &active),
                JoinReadiness::NotReady
            ) {
                continue;
            }
            journal_dead_inputs(log, playbook, s, &state, &active)?;
            if e.max_traversals.is_some() {
                log.append(EventPayload::EdgeTraversed {
                    from: m.clone(),
                    to: s.clone(),
                    via_policy: false,
                    uncounted: false,
                })?;
            }
            frontier.push(s.clone());
        }
    }
    Ok(())
}
