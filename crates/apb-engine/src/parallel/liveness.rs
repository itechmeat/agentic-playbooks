//! Join liveness: which nodes a run can still execute before a join runs,
//! so a join waits for a branch that can still arrive and writes off one
//! that cannot.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use apb_core::schema::Playbook;

use crate::state::RunState;

/// Where a finished node actually routed (`parallel::routed_targets`).
pub(super) type Routed<'a> = &'a dyn Fn(&str) -> BTreeSet<String>;

/// The nodes a run can still execute before `join` runs, walked forward
/// from `active` over the RESIDUAL graph: out of a node whose latest
/// execution stands (it finished, nothing delivered into it since, and the
/// walk did not reach it from a node that runs) only the edges that
/// execution selected are followed, because the edges it did not take can
/// never be traversed now. Out of a node that runs (not finished, delivered
/// into again after it finished in an earlier loop round, or downstream of
/// one that runs) every outgoing edge is followed - any of them could still
/// be taken. The walk
/// never passes through `join` itself: what lies only behind the join cannot
/// arrive before it runs.
///
/// Walking the STRUCTURAL graph instead would keep whole branches "live" that
/// the run has already routed away from, which is what makes a shared failure
/// sink (`w1 -> aborted` taken, `w1 -> w2 -> aborted` abandoned) wait forever
/// for an input that can no longer arrive.
pub(super) fn live_nodes(
    playbook: &Playbook,
    state: &RunState,
    active: &[String],
    join: &str,
    routed: Routed<'_>,
) -> BTreeSet<String> {
    // Each live node with whether it runs (again): then every edge out of
    // it may still be taken, and so may every edge out of what follows it.
    let mut live: BTreeMap<String, bool> = BTreeMap::new();
    let mut queue: VecDeque<(String, bool)> = active.iter().map(|n| (n.clone(), false)).collect();
    while let Some((id, from_runs)) = queue.pop_front() {
        // Already walked as a node that runs: nothing more to learn.
        if live.get(&id) == Some(&true) {
            continue;
        }
        let runs = from_runs
            || !state.current_status(&id).is_finished()
            || delivered_again(playbook, state, &id, routed);
        if live.contains_key(&id) && !runs {
            continue;
        }
        live.insert(id.clone(), runs);
        if id == join {
            continue;
        }
        let next: BTreeSet<String> = match runs {
            false => routed(&id),
            true => playbook
                .edges
                .iter()
                .filter(|e| e.from == id)
                .map(|e| e.to.clone())
                .collect(),
        };
        queue.extend(next.into_iter().map(|n| (n, runs)));
    }
    live.into_keys().collect()
}

/// Whether a node that finished was delivered into again since: a
/// predecessor finished after it and routed into it, so it runs again (the
/// next loop round reaches it, or a re-run passes through it).
fn delivered_again(playbook: &Playbook, state: &RunState, node: &str, routed: Routed<'_>) -> bool {
    let Some(done) = state.seqs.finished.get(node) else {
        return false;
    };
    playbook.edges.iter().any(|e| {
        e.to == node
            && e.from != node
            && state.seqs.finished.get(&e.from).is_some_and(|f| f > done)
            && routed(&e.from).contains(node)
    })
}
