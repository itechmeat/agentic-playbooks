//! Pipelined batch admission (issue #195): routing a batch member the moment
//! it succeeds, so its newly ready successors start while the other members
//! still run. Shares the parent module's imports via `use super::*`.

use super::*;

/// Routes the batch member `node`, which just succeeded, and admits its newly
/// ready batchable successors into the running batch: each joins `batch` (so
/// every later liveness question counts it), gets its own cancel flag
/// registered with the run-level stop fanout, and is queued for a slot.
/// Non-batchable successors stay in the frontier for after the batch, exactly
/// as before.
#[allow(clippy::too_many_arguments)]
pub(crate) fn pipeline_member(
    playbook: &Playbook,
    run_dir: &Path,
    journal: &Journal<'_>,
    node: &str,
    finished: &[String],
    frontier: &mut Vec<String>,
    batch: &mut Vec<String>,
    queue: &mut std::collections::VecDeque<String>,
    member_flags: &mut BTreeMap<String, Arc<AtomicBool>>,
    member_fans: &mut Vec<crate::stop::FanoutGuard>,
    fanout: &Arc<crate::stop::CancelFanout>,
    steps: &mut usize,
) -> Result<(), EngineError> {
    let state = RunState::fold(&read_all(run_dir)?);
    // The members still queued or running sit in the frontier for the advance,
    // as they did when the batch was routed only at its end: a successor that
    // is one of them is not pushed a second time.
    let pending: Vec<String> = batch
        .iter()
        .filter(|m| !finished.iter().any(|f| f == *m) && !frontier.contains(*m))
        .cloned()
        .collect();
    frontier.extend(pending.iter().cloned());
    let advanced =
        journal.with_log(|log| advance_frontier(playbook, node, &state, frontier, batch, log));
    frontier.retain(|n| !pending.contains(n));
    advanced?;
    // A member re-entered by a loop (a self-edge) stays in the frontier for
    // after the batch: one execution per member per batch.
    let ready: Vec<String> = frontier
        .iter()
        .filter(|n| is_batchable(playbook, n) && !batch.contains(*n))
        .cloned()
        .collect();
    frontier.retain(|n| !ready.contains(n));
    for n in ready {
        let flag = Arc::new(AtomicBool::new(false));
        member_fans.push(fanout.register(&flag));
        member_flags.insert(n.clone(), flag);
        batch.push(n.clone());
        queue.push_back(n);
        *steps += 1;
    }
    Ok(())
}

/// The batch members that have ended, in the order they did.
pub(crate) fn finished_members(results: &[(String, NodeStatus, String)]) -> Vec<String> {
    results.iter().map(|(n, _, _)| n.clone()).collect()
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
