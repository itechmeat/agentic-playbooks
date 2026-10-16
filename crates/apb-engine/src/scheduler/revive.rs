//! The frontier rebuild of a drive that is about to execute nodes again on an
//! operator's or a supervisor's word (issue #203): an explicit `--from-node`
//! resume, and a supervisor retry, continue-from, patch or review-gate move,
//! sequential or after a batch. An automatic restart of interrupted work does
//! not come here: it revives nothing.
//!
//! The rebuild journals the revival as `nodes_revived`, so the fold reads the
//! revived nodes as not yet run from then on ([`RunState::revive`]): every
//! readiness decision of the drive, not only this rebuild, sees the same view
//! until each node starts again. It also journals `join_input_revived` for
//! every earlier write-off that view makes obsolete. [`reoffer_revived_joins`]
//! covers the one gap no edge does: a revived join whose revived input dies.

use super::*;

/// [`restore_frontier`] for a drive that executes every node of `starts`
/// again, `starts[0]` being the drive's `current`: [`revive`], then rebuilds
/// the heads against the revived fold. Used where the drive has lost its
/// frontier or never had one (an operator's `--from-node` resume) and where a
/// batch tail did not advance its members (the batch-path directives).
///
/// `overridden` are parked failed nodes a directive moved the run away from
/// without re-running them: a target only they route into (a failure edge
/// into a rollback, say) is what the supervisor decided against, so it is no
/// head. Their deliveries still count for join readiness.
pub(crate) fn restore_revived_frontier(
    log: &mut EventLog,
    playbook: &Playbook,
    events: &[Event],
    starts: &[String],
    frontier: &mut Vec<String>,
    overridden: &[String],
) -> Result<(), EngineError> {
    let Some(current) = starts.first() else {
        return Ok(());
    };
    let state = revive(log, playbook, events, starts, frontier)?;
    let overridden: Vec<String> = overridden
        .iter()
        .filter(|n| !starts.contains(n))
        .cloned()
        .collect();
    restore_frontier_besides(playbook, &state, current, frontier, &overridden);
    Ok(())
}

/// The revival of a drive that executes every node of `starts` again,
/// `starts[0]` being the drive's `current`, without rebuilding any head:
/// what a supervisor directive at a sequential park or a review gate needs,
/// whose in-memory frontier is still the drive's own. Journals
/// `nodes_revived` per start (and `join_input_revived` per obsolete
/// write-off), drops from `frontier` the revived nodes that are not starts
/// (the re-run reaches them again, e.g. an earlier owed re-run this one
/// covers), puts every other owed re-run back, and returns the revived fold.
pub(crate) fn revive(
    log: &mut EventLog,
    playbook: &Playbook,
    events: &[Event],
    starts: &[String],
    frontier: &mut Vec<String>,
) -> Result<RunState, EngineError> {
    let mut state = RunState::fold(events);
    let Some(current) = starts.first() else {
        return Ok(state);
    };
    let cancelled = cancelled_siblings(events, &state, starts);
    // A cancelled sibling runs again: a plain node as a start of its own, so
    // it goes back on the frontier; a join only revived, so it still waits
    // for its inputs instead of running as soon as it is offered.
    let mut all_starts = starts.to_vec();
    for (_, node) in &cancelled {
        if !parallel::is_join(playbook, node) && !all_starts.contains(node) {
            all_starts.push(node.clone());
        }
    }
    let starts = all_starts;
    let rest = &starts[1..];
    let mut revival = parallel::Revival::new(playbook, &starts);
    for (start, node) in &cancelled {
        if parallel::is_join(playbook, node) {
            revival.add(start, node);
        }
    }
    for (start, nodes) in revival.per_start() {
        if !nodes.iter().any(|n| carries_a_result(&state, n)) {
            continue;
        }
        log.append(EventPayload::NodesRevived {
            from_node: start.to_string(),
            nodes: nodes.iter().cloned().collect(),
        })?;
        // Exactly what the fold does with the record just written.
        state.revive_from(start, nodes);
    }
    let mut also: Vec<String> = rest.to_vec();
    also.extend(parallel::pending_heads(playbook, &state));
    let active = active_set(current, frontier, &also);
    for record in revived_inputs(playbook, events, &state, &revival, &active) {
        log.append(record)?;
    }
    frontier.retain(|n| starts.contains(n) || !revival.contains(n));
    for owed in &state.revival_starts {
        if owed != current && !frontier.contains(owed) {
            frontier.push(owed.clone());
        }
    }
    Ok(state)
}

/// The branch siblings a fork's failure policy cancelled because a start
/// failed (`branch_cancelled` with that `failed_node`, after the start's last
/// `node_started`) and that are still cancelled, as `(start, sibling)`. They
/// lie outside the failed node's own closure, so without them a re-run of it
/// would leave its join to proceed without them.
fn cancelled_siblings(
    events: &[Event],
    state: &RunState,
    starts: &[String],
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for start in starts {
        let since = events
            .iter()
            .rposition(
                |e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == start),
            )
            .unwrap_or(0);
        for e in &events[since..] {
            if let EventPayload::BranchCancelled {
                node, failed_node, ..
            } = &e.payload
                && failed_node == start
                && state.nodes.get(node) == Some(&NodeStatus::Cancelled)
                && !out.iter().any(|(_, n)| n == node)
            {
                out.push((start.clone(), node.clone()));
            }
        }
    }
    out
}

/// Who asks for a re-run, for the wording of [`refuse_inside_owed`].
pub(crate) enum Asker {
    Operator,
    Supervisor,
}

/// Refuses a re-run of `node` that lies inside the revival of a re-run the
/// run still owes (issue #203): a supervisor's retry a pause stopped before
/// it began, say. Re-running `node` first would run it again when the owed
/// re-run reaches it. Checked before anything is journaled.
pub(crate) fn refuse_inside_owed(
    playbook: &Playbook,
    state: &RunState,
    node: &str,
    asker: Asker,
) -> Result<(), EngineError> {
    for owed in &state.revival_starts {
        if owed == node
            || !parallel::Revival::new(playbook, std::slice::from_ref(owed)).contains(node)
        {
            continue;
        }
        let hint = match asker {
            Asker::Operator => format!(
                "resume without --from-node (`{owed}` runs first), or with --from-node `{owed}` or a node upstream of it"
            ),
            Asker::Supervisor => {
                format!("retry or continue from `{owed}` or a node upstream of it instead")
            }
        };
        return Err(EngineError::Invalid(format!(
            "node `{node}` lies downstream of `{owed}`, whose re-run is still owed: {hint}"
        )));
    }
    Ok(())
}

/// Whether the drive over `events` starts from an operator's re-run of
/// `node`: the latest `run_resumed` is an explicit `--from-node` naming it.
/// Only that, and a supervisor's directive, revives (issue #203); an
/// automatic restart of interrupted work does not.
pub(crate) fn operator_rerun(events: &[Event], node: &str) -> bool {
    events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::RunResumed {
                from_node,
                explicit,
                ..
            } => Some(*explicit && from_node == node),
            _ => None,
        })
        .unwrap_or(false)
}

/// Whether reviving `node` changes the fold: it has a result, a hop or a
/// policy mark. A fresh run has none, so it never journals a revival.
fn carries_a_result(state: &RunState, node: &str) -> bool {
    state.nodes.contains_key(node)
        || state.route_suppressed.contains(node)
        || state.refused_joins.contains(node)
        || state.journaled_hops.iter().any(|(from, _)| from == node)
}

/// Offers every revived join that a hop from outside the revival made a
/// head, once its readiness is decided, exactly as [`advance_frontier`] offers
/// one: its dead inputs journaled, then pushed.
///
/// A revived join is offered by the advance of a revived branch that delivers
/// into it. When that branch dies instead (the re-run node fails again, or
/// routes elsewhere), no edge delivers into the join and nothing else would
/// offer it, so the run would end without it. Called after every advance with
/// the state that advance weighed; a no-op unless the run revived something.
/// A join that finished in an earlier pass is no head
/// ([`parallel::pending_heads`]), so it is not run twice on its old inputs.
pub(crate) fn reoffer_revived_joins(
    playbook: &Playbook,
    state: &RunState,
    log: &mut EventLog,
    node: &str,
    also_active: &[String],
    frontier: &mut Vec<String>,
) -> Result<(), EngineError> {
    if state.revived.is_empty() {
        return Ok(());
    }
    for head in parallel::pending_heads(playbook, state) {
        if !state.revived.contains(&head)
            || !parallel::is_join(playbook, &head)
            || frontier.contains(&head)
            || head == node
        {
            continue;
        }
        let active = active_set(node, frontier, also_active);
        if matches!(
            parallel::join_readiness(playbook, &head, state, &active),
            JoinReadiness::NotReady
        ) {
            continue;
        }
        journal_dead_inputs(log, playbook, &head, state, &active)?;
        frontier.push(head);
    }
    Ok(())
}

/// The write-offs still standing: per join, the sources of its latest
/// `JoinInputDead`, minus every source a later `JoinInputRevived` already
/// revived (a second resume must not record the same revival again).
fn standing_write_offs(events: &[Event]) -> BTreeMap<&str, Vec<String>> {
    let mut out: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for e in events {
        match &e.payload {
            EventPayload::JoinInputDead { node, sources } => {
                out.insert(node.as_str(), sources.clone());
            }
            EventPayload::JoinInputRevived { node, sources, .. } => {
                if let Some(dead) = out.get_mut(node.as_str()) {
                    dead.retain(|s| !sources.contains(s));
                    if dead.is_empty() {
                        out.remove(node.as_str());
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The `JoinInputRevived` records for `state` (the revived fold): one per join
/// that has not finished in it and whose standing write-off names a source
/// that can arrive again, listing exactly those sources. `active` is the set
/// the rebuild weighs readiness with, so the record and the verdict agree.
fn revived_inputs(
    playbook: &Playbook,
    events: &[Event],
    state: &RunState,
    revival: &parallel::Revival,
    active: &[String],
) -> Vec<EventPayload> {
    let mut out = Vec::new();
    for (join, dead) in standing_write_offs(events) {
        if state.current_status(join).is_finished() {
            continue;
        }
        let pending = parallel::pending_inputs(playbook, join, state, active);
        let sources: Vec<String> = dead.into_iter().filter(|s| pending.contains(s)).collect();
        let Some(first) = sources.first() else {
            continue;
        };
        let from_node = revival
            .start_of(first)
            .or_else(|| active.first().map(String::as_str))
            .unwrap_or_default()
            .to_string();
        out.push(EventPayload::JoinInputRevived {
            node: join.to_string(),
            sources,
            from_node,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The issue's report shape: `b` delivered into the implicit join `j`, `c`
    /// failed into `failed`, so `c2` was written off. `{join}` is spliced into
    /// both edges into `j`.
    fn report(join: &str) -> Playbook {
        Playbook::from_yaml(&format!(
            r#"
schema: 2
id: r
name: R
version: 1.0.0
nodes:
  - {{ id: start, type: start }}
  - {{ id: a, type: prompt, prompt: "a" }}
  - {{ id: b, type: prompt, prompt: "b" }}
  - {{ id: c, type: prompt, prompt: "c" }}
  - {{ id: c2, type: prompt, prompt: "c2" }}
  - {{ id: j, type: prompt, prompt: "j" }}
  - {{ id: done, type: finish, outcome: success }}
  - {{ id: failed, type: finish, outcome: failure }}
edges:
  - {{ from: start, to: a }}
  - {{ from: a, to: b }}
  - {{ from: a, to: c }}
  - {{ from: b, to: j{join} }}
  - {{ from: c, to: c2, condition: {{ type: node_status, node: c, equals: success }} }}
  - {{ from: c, to: failed, condition: {{ type: node_status, node: c, equals: failure }} }}
  - {{ from: c2, to: j{join} }}
  - {{ from: j, to: done }}
"#
        ))
        .unwrap()
    }

    fn started(node: &str) -> EventPayload {
        EventPayload::NodeStarted {
            node: node.into(),
            attempt: 1,
        }
    }

    fn finished(node: &str, status: &str) -> Vec<EventPayload> {
        vec![
            started(node),
            EventPayload::NodeFinished {
                node: node.into(),
                status: status.into(),
                attempt: 1,
                output: String::new(),
                artifacts: Vec::new(),
            },
        ]
    }

    fn hop(from: &str, to: &str) -> EventPayload {
        EventPayload::EdgeTraversed {
            from: from.into(),
            to: to.into(),
            via_policy: false,
            uncounted: true,
        }
    }

    fn dead(node: &str, sources: &[&str]) -> EventPayload {
        EventPayload::JoinInputDead {
            node: node.into(),
            sources: names(sources),
        }
    }

    fn revived_record(node: &str, sources: &[&str], from: &str) -> EventPayload {
        EventPayload::JoinInputRevived {
            node: node.into(),
            sources: names(sources),
            from_node: from.into(),
        }
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    /// Payloads compared by their debug text (the enum has no `PartialEq`).
    fn dbg(v: &[EventPayload]) -> Vec<String> {
        v.iter().map(|p| format!("{p:?}")).collect()
    }

    /// The first, failed pass of the report.
    fn failed_pass() -> Vec<EventPayload> {
        let mut e = Vec::new();
        e.extend(finished("start", "succeeded"));
        e.push(hop("start", "a"));
        e.extend(finished("a", "succeeded"));
        e.push(hop("a", "b"));
        e.push(hop("a", "c"));
        e.extend(finished("b", "succeeded"));
        e.push(hop("b", "j"));
        e.extend(finished("c", "failed"));
        e.push(hop("c", "failed"));
        e.push(dead("j", &["c2"]));
        e
    }

    /// A run dir holding `payloads` as its journal.
    struct Run {
        dir: tempfile::TempDir,
        log: EventLog,
    }

    impl Run {
        fn new(payloads: Vec<EventPayload>) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let mut log = EventLog::create(dir.path()).unwrap();
            for p in payloads {
                log.append(p).unwrap();
            }
            Run { dir, log }
        }

        fn events(&self) -> Vec<Event> {
            read_all(self.dir.path()).unwrap()
        }

        fn append(&mut self, payloads: Vec<EventPayload>) {
            for p in payloads {
                self.log.append(p).unwrap();
            }
        }
    }

    /// What one revived rebuild did: the revivals and the join records it
    /// journaled, and the frontier it built.
    struct Rebuilt {
        revivals: Vec<(String, Vec<String>)>,
        records: Vec<EventPayload>,
        frontier: Vec<String>,
    }

    fn rebuild(p: &Playbook, run: &mut Run, starts: &[&str]) -> Rebuilt {
        let events = run.events();
        let mut frontier = Vec::new();
        restore_revived_frontier(&mut run.log, p, &events, &names(starts), &mut frontier, &[])
            .unwrap();
        let mut revivals = Vec::new();
        let mut records = Vec::new();
        for e in &run.events()[events.len()..] {
            match &e.payload {
                EventPayload::NodesRevived { from_node, nodes } => {
                    revivals.push((from_node.clone(), nodes.clone()))
                }
                other => records.push(other.clone()),
            }
        }
        Rebuilt {
            revivals,
            records,
            frontier,
        }
    }

    /// The drive's sweep after an advance out of `node`, against the journal.
    fn sweep(
        p: &Playbook,
        run: &mut Run,
        node: &str,
        frontier: &mut Vec<String>,
    ) -> Vec<EventPayload> {
        let events = run.events();
        let state = RunState::fold(&events);
        reoffer_revived_joins(p, &state, &mut run.log, node, &[], frontier).unwrap();
        run.events()[events.len()..]
            .iter()
            .map(|e| e.payload.clone())
            .collect()
    }

    #[test]
    fn a_rerun_journals_the_revival_and_the_revived_source() {
        let p = report("");
        let mut run = Run::new(failed_pass());
        let r = rebuild(&p, &mut run, &["c"]);
        assert_eq!(
            r.revivals,
            vec![("c".to_string(), names(&["c", "c2", "done", "failed", "j"]))]
        );
        assert_eq!(dbg(&r.records), dbg(&[revived_record("j", &["c2"], "c")]));
        // `c` is the drive's current; the join waits, the failure route is gone.
        assert!(r.frontier.is_empty());
        // The fold now carries the revival for every later decision.
        let state = RunState::fold(&run.events());
        assert_eq!(state.current_status("c"), NodeStatus::Pending);
        assert_eq!(state.nodes.get("c"), Some(&NodeStatus::Failed));
    }

    /// A fresh run revives nothing that carries a result: no record at all.
    #[test]
    fn a_fresh_run_journals_no_revival() {
        let p = report("");
        let mut run = Run::new(vec![EventPayload::RunStarted {
            playbook: "r".into(),
            version: "1.0.0".into(),
        }]);
        let r = rebuild(&p, &mut run, &["start"]);
        assert!(r.revivals.is_empty() && r.records.is_empty());
    }

    /// The supervisor retry path hands several starts: the record names the
    /// one whose re-run revives the source, not just the drive's `current`.
    #[test]
    fn a_retry_of_several_members_names_the_reviving_one() {
        let p = report("");
        let mut run = Run::new(failed_pass());
        let r = rebuild(&p, &mut run, &["b", "c"]);
        assert_eq!(dbg(&r.records), dbg(&[revived_record("j", &["c2"], "c")]));
        assert_eq!(r.revivals.len(), 2);
    }

    #[test]
    fn a_rerun_elsewhere_revives_nothing() {
        let p = report("");
        let mut run = Run::new(failed_pass());
        assert!(rebuild(&p, &mut run, &["b"]).records.is_empty());
    }

    #[test]
    fn no_write_off_no_record() {
        let p = report("");
        let mut events = failed_pass();
        events.pop();
        let mut run = Run::new(events);
        assert!(rebuild(&p, &mut run, &["c"]).records.is_empty());
    }

    /// One of two written-off sources revived: the other still stands.
    #[test]
    fn a_partial_revival_leaves_the_other_source_standing() {
        let events = vec![
            ev(dead("j", &["c2", "c3"])),
            ev(revived_record("j", &["c2"], "c")),
        ];
        let standing = standing_write_offs(&events);
        assert_eq!(standing.get("j"), Some(&names(&["c3"])));
    }

    fn ev(payload: EventPayload) -> Event {
        Event {
            seq: 0,
            ts: 0,
            payload,
        }
    }

    /// A second resume after the first one revived the write-off records
    /// nothing new, until a fresh write-off stands again.
    #[test]
    fn a_revived_write_off_is_not_revived_twice() {
        let p = report("");
        let mut events = failed_pass();
        events.push(EventPayload::RunResumed {
            from_node: "c".into(),
            by: None,
            explicit: true,
        });
        events.push(revived_record("j", &["c2"], "c"));
        let mut run = Run::new(events);
        assert!(rebuild(&p, &mut run, &["c"]).records.is_empty());

        run.append(finished("c", "failed"));
        run.append(vec![dead("j", &["c2"])]);
        assert_eq!(
            dbg(&rebuild(&p, &mut run, &["c"]).records),
            dbg(&[revived_record("j", &["c2"], "c")])
        );
    }

    /// An explicit join inside the cycle is an ancestor of the re-run node: it
    /// keeps its finish, so its old write-off is not reported as revived.
    #[test]
    fn a_finished_join_outside_the_revival_records_nothing() {
        let p = Playbook::from_yaml(
            r#"
schema: 2
id: l
name: L
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: x, type: prompt, prompt: "x" }
  - { id: y, type: prompt, prompt: "y" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: x }
  - { from: a, to: y }
  - { from: x, to: j, join: all }
  - { from: y, to: j, join: all }
  - { from: j, to: a, max_traversals: 1 }
  - { from: j, to: done, fallback: true }
"#,
        )
        .unwrap();
        let mut e = Vec::new();
        e.extend(finished("start", "succeeded"));
        e.extend(finished("a", "succeeded"));
        e.push(hop("a", "x"));
        e.push(hop("a", "y"));
        e.extend(finished("y", "succeeded"));
        e.push(hop("y", "j"));
        e.extend(finished("x", "failed"));
        e.push(dead("j", &["x"]));
        e.extend(finished("j", "succeeded"));
        let mut run = Run::new(e);
        let r = rebuild(&p, &mut run, &["x"]);
        assert!(r.records.is_empty());
    }

    /// Review finding: a `join: any` that ran on `b` in the old pass is not
    /// run again at once on that old input; the revived branch re-runs it.
    #[test]
    fn a_finished_revived_any_join_is_not_offered_from_a_stale_hop() {
        let p = report(", join: any");
        let mut e = failed_pass();
        e.pop();
        e.extend(finished("j", "succeeded"));
        e.push(hop("j", "done"));
        let mut run = Run::new(e);
        let r = rebuild(&p, &mut run, &["c"]);
        assert!(r.frontier.is_empty(), "{:?}", r.frontier);
    }

    /// The same for a first-arrival merge inside a cycle (`m <-> n`): it is no
    /// barrier, so only the finish in the old pass keeps it off the frontier.
    #[test]
    fn a_finished_revived_merge_in_a_cycle_is_not_offered_from_a_stale_hop() {
        let p = Playbook::from_yaml(
            r#"
schema: 2
id: m
name: M
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: c, type: prompt, prompt: "c" }
  - { id: m, type: condition }
  - { id: n, type: prompt, prompt: "n" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: b }
  - { from: a, to: c }
  - { from: b, to: m }
  - { from: c, to: m }
  - { from: m, to: n, condition: { type: node_status, node: m, equals: failure } }
  - { from: n, to: m, max_traversals: 1 }
  - { from: m, to: done, condition: { type: node_status, node: m, equals: success } }
"#,
        )
        .unwrap();
        assert!(!parallel::is_join(&p, "m"));
        let mut e = Vec::new();
        e.extend(finished("start", "succeeded"));
        e.extend(finished("a", "succeeded"));
        e.push(hop("a", "b"));
        e.push(hop("a", "c"));
        e.extend(finished("b", "succeeded"));
        e.push(hop("b", "m"));
        e.extend(finished("m", "succeeded"));
        e.extend(finished("c", "failed"));
        let mut run = Run::new(e);
        let r = rebuild(&p, &mut run, &["c"]);
        assert!(r.frontier.is_empty(), "{:?}", r.frontier);
    }

    /// The revived join is offered once the re-run node fails again and its
    /// branch is dead: the fresh write-off is journaled and the join pushed.
    #[test]
    fn a_revived_join_is_reoffered_when_the_revived_branch_dies() {
        let p = report("");
        let mut run = Run::new(failed_pass());
        rebuild(&p, &mut run, &["c"]);
        let mut frontier = Vec::new();

        run.append(vec![started("c")]);
        assert!(sweep(&p, &mut run, "c", &mut frontier).is_empty());
        assert!(frontier.is_empty(), "still waiting while c runs");

        run.append(finished("c", "failed")[1..].to_vec());
        frontier.push("failed".into());
        let fresh = sweep(&p, &mut run, "c", &mut frontier);
        assert_eq!(frontier, names(&["failed", "j"]));
        assert_eq!(dbg(&fresh), dbg(&[dead("j", &["c2"])]));
    }

    /// Review hole 1: a revived join that finished in the old pass is not run
    /// again on its old inputs when the revived branch dies again.
    #[test]
    fn a_join_that_ran_before_is_not_reoffered_on_its_old_inputs() {
        let p = report("");
        let mut e = failed_pass();
        e.pop();
        e.push(dead("j", &["c2"]));
        e.extend(finished("j", "succeeded"));
        e.push(hop("j", "done"));
        let mut run = Run::new(e);
        let r = rebuild(&p, &mut run, &["c"]);
        assert!(r.frontier.is_empty());
        run.append(finished("c", "failed"));
        let mut frontier = names(&["failed"]);
        assert!(sweep(&p, &mut run, "c", &mut frontier).is_empty());
        assert_eq!(frontier, names(&["failed"]));
    }

    /// Round 2 hole 3: `G` (`join: any` over `b`, `c2`) ran on `b`, and so
    /// did `P`; the downstream join `J` (over `P` and `b`) had not run yet
    /// when the driver died. Resumed from `c`, which dies again through a
    /// non-finish step: `G` and `P` are revived and get no new input, so `J`
    /// must be offered on `b` alone instead of waiting forever.
    #[test]
    fn a_downstream_join_behind_a_revived_any_join_is_offered() {
        let p = Playbook::from_yaml(
            r#"
schema: 2
id: g
name: G
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: c, type: prompt, prompt: "c" }
  - { id: c2, type: prompt, prompt: "c2" }
  - { id: G, type: prompt, prompt: "G" }
  - { id: P, type: prompt, prompt: "P" }
  - { id: J, type: prompt, prompt: "J" }
  - { id: notify, type: prompt, prompt: "notify" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: b }
  - { from: a, to: c }
  - { from: b, to: G, join: any }
  - { from: c2, to: G, join: any }
  - { from: c, to: c2, condition: { type: node_status, node: c, equals: success } }
  - { from: c, to: notify, condition: { type: node_status, node: c, equals: failure } }
  - { from: G, to: P }
  - { from: P, to: J }
  - { from: b, to: J }
  - { from: J, to: done }
"#,
        )
        .unwrap();
        let mut e = Vec::new();
        e.extend(finished("start", "succeeded"));
        e.push(hop("start", "a"));
        e.extend(finished("a", "succeeded"));
        e.push(hop("a", "b"));
        e.push(hop("a", "c"));
        e.extend(finished("b", "succeeded"));
        e.push(hop("b", "G"));
        e.push(hop("b", "J"));
        e.extend(finished("G", "succeeded"));
        e.push(hop("G", "P"));
        e.extend(finished("P", "succeeded"));
        e.push(hop("P", "J"));
        e.extend(finished("c", "failed"));
        e.push(hop("c", "notify"));
        let mut run = Run::new(e);
        let r = rebuild(&p, &mut run, &["c"]);
        assert!(r.frontier.is_empty(), "{:?}", r.frontier);

        run.append(finished("c", "failed"));
        let mut frontier = names(&["notify"]);
        let fresh = sweep(&p, &mut run, "c", &mut frontier);
        assert_eq!(frontier, names(&["notify", "J"]));
        assert_eq!(dbg(&fresh), dbg(&[dead("J", &["P"])]));
    }

    /// Round 2 item 5: the old hop of a revived node does not survive its
    /// re-run. `c` delivered into `c2` in the old pass; re-run, it fails and
    /// routes to `failed`, and the fold no longer counts the old delivery.
    #[test]
    fn an_old_hop_of_a_revived_node_does_not_survive_its_rerun() {
        let p = report("");
        let mut e = Vec::new();
        e.extend(finished("start", "succeeded"));
        e.extend(finished("a", "succeeded"));
        e.extend(finished("c", "succeeded"));
        e.push(hop("c", "c2"));
        let mut run = Run::new(e);
        rebuild(&p, &mut run, &["c"]);
        run.append(finished("c", "failed"));
        run.append(vec![hop("c", "failed")]);
        let state = RunState::fold(&run.events());
        assert!(!state.journaled_hops.contains(&("c".into(), "c2".into())));
        assert!(
            state
                .journaled_hops
                .contains(&("c".into(), "failed".into()))
        );
        assert!(state.revived.contains("c2") && !state.revived.contains("c"));
    }

    /// Round 3 item 11: two sources written off, only one of them revived:
    /// the record names that one alone, the other stays written off.
    #[test]
    fn only_the_revived_one_of_two_written_off_sources_is_recorded() {
        let p = Playbook::from_yaml(
            r#"
schema: 2
id: t
name: T
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: c, type: prompt, prompt: "c" }
  - { id: c2, type: prompt, prompt: "c2" }
  - { id: d, type: prompt, prompt: "d" }
  - { id: d2, type: prompt, prompt: "d2" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: a }
  - { from: a, to: b }
  - { from: a, to: c }
  - { from: a, to: d }
  - { from: b, to: j }
  - { from: c, to: c2, condition: { type: node_status, node: c, equals: success } }
  - { from: c, to: failed, condition: { type: node_status, node: c, equals: failure } }
  - { from: d, to: d2, condition: { type: node_status, node: d, equals: success } }
  - { from: d, to: failed, condition: { type: node_status, node: d, equals: failure } }
  - { from: c2, to: j }
  - { from: d2, to: j }
"#,
        )
        .unwrap();
        let mut e = Vec::new();
        e.extend(finished("start", "succeeded"));
        e.push(hop("start", "a"));
        e.extend(finished("a", "succeeded"));
        e.push(hop("a", "b"));
        e.push(hop("a", "c"));
        e.push(hop("a", "d"));
        e.extend(finished("b", "succeeded"));
        e.push(hop("b", "j"));
        e.extend(finished("c", "failed"));
        e.push(hop("c", "failed"));
        e.extend(finished("d", "failed"));
        e.push(hop("d", "failed"));
        e.push(dead("j", &["c2", "d2"]));
        let mut run = Run::new(e);
        let r = rebuild(&p, &mut run, &["c"]);
        assert_eq!(dbg(&r.records), dbg(&[revived_record("j", &["c2"], "c")]));
    }

    /// A fork's `fail_fast` (`start` forks into `a` and `b`, `b` feeds the
    /// join `j`; `x` is a second join cancelled while it waited) cancelled
    /// `b` and `x` when `a` failed. Both lie outside `a`'s closure: re-running
    /// `a` revives them, `b` goes back on the frontier, `x` waits again.
    fn fail_fast_pass() -> (Playbook, Vec<EventPayload>) {
        let p = Playbook::from_yaml(
            r#"
schema: 2
id: f
name: F
version: 1.0.0
nodes:
  - { id: start, type: start, fork: { on_branch_failure: fail_fast, on_failure: rejected } }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: p, type: prompt, prompt: "p" }
  - { id: x, type: prompt, prompt: "x" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: rejected, type: finish, outcome: failure }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: start, to: p }
  - { from: p, to: x }
  - { from: b, to: x }
  - { from: a, to: j }
  - { from: x, to: j }
  - { from: j, to: done }
"#,
        )
        .unwrap();
        let mut e = Vec::new();
        e.extend(finished("start", "succeeded"));
        e.push(hop("start", "a"));
        e.push(hop("start", "b"));
        e.push(hop("start", "p"));
        e.extend(finished("p", "succeeded"));
        e.push(hop("p", "x"));
        e.extend(finished("a", "failed"));
        e.push(EventPayload::BranchFailed {
            fork: "start".into(),
            node: "a".into(),
            policy: "fail_fast".into(),
            target: Some("rejected".into()),
        });
        for n in ["b", "x"] {
            e.extend(finished(n, "cancelled"));
            e.push(EventPayload::BranchCancelled {
                fork: "start".into(),
                node: n.into(),
                failed_node: "a".into(),
            });
        }
        (p, e)
    }

    #[test]
    fn a_rerun_revives_the_siblings_its_failure_cancelled() {
        let (p, e) = fail_fast_pass();
        let mut run = Run::new(e);
        let r = rebuild(&p, &mut run, &["a"]);
        let from: Vec<&str> = r.revivals.iter().map(|(f, _)| f.as_str()).collect();
        assert_eq!(from, ["a", "b"], "the plain sibling is a start of its own");
        assert!(
            r.revivals[0].1.contains(&"x".to_string()),
            "the join is revived"
        );
        assert_eq!(r.frontier, names(&["b"]));
        let state = RunState::fold(&run.events());
        assert_eq!(state.current_status("x"), NodeStatus::Pending);
        assert!(!state.route_suppressed.contains("b"));
    }

    /// Round 4 item 3: a later revival that covers an earlier owed re-run
    /// drops it: `b` was owed, then the run moves to `a`, upstream of `b`.
    /// `b` leaves the frontier and the owed set; `a`'s re-run reaches it.
    #[test]
    fn a_later_revival_drops_the_owed_rerun_it_covers() {
        let p = report("");
        let mut e = failed_pass();
        e.push(EventPayload::NodesRevived {
            from_node: "b".into(),
            nodes: names(&["b", "done", "j"]),
        });
        let mut run = Run::new(e);
        let events = run.events();
        let mut frontier = names(&["b"]);
        revive(&mut run.log, &p, &events, &names(&["a"]), &mut frontier).unwrap();
        assert!(frontier.is_empty(), "{frontier:?}");
        let state = RunState::fold(&run.events());
        assert_eq!(
            state.revival_starts.iter().collect::<Vec<_>>(),
            [&"a".to_string()]
        );
    }

    /// Round 5 item 3: two owed re-runs, `c` (the drive's current) and `y`,
    /// which feeds the join `jj` beside `x` (finished, routed into `jj`).
    /// `y` counts as active before `jj` is weighed, so `jj` is not ready
    /// without it.
    #[test]
    fn an_owed_rerun_that_is_not_current_keeps_its_join_waiting() {
        let p = Playbook::from_yaml(
            r#"
schema: 2
id: o
name: O
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: x, type: prompt, prompt: "x" }
  - { id: y, type: prompt, prompt: "y" }
  - { id: c, type: prompt, prompt: "c" }
  - { id: jj, type: prompt, prompt: "jj" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: x }
  - { from: start, to: y }
  - { from: start, to: c }
  - { from: x, to: jj }
  - { from: y, to: jj }
  - { from: c, to: done }
"#,
        )
        .unwrap();
        let mut e = Vec::new();
        e.extend(finished("start", "succeeded"));
        e.push(hop("start", "x"));
        e.push(hop("start", "y"));
        e.push(hop("start", "c"));
        e.extend(finished("x", "succeeded"));
        e.push(hop("x", "jj"));
        e.extend(finished("y", "failed"));
        e.extend(finished("c", "failed"));
        for start in ["c", "y"] {
            e.push(EventPayload::NodesRevived {
                from_node: start.into(),
                nodes: names(&[start]),
            });
        }
        let run = Run::new(e);
        let state = RunState::fold(&run.events());
        let mut frontier = Vec::new();
        restore_frontier(&p, &state, "c", &mut frontier);
        assert_eq!(frontier, names(&["y"]), "{frontier:?}");
    }

    /// Round 5 item 5: a re-run asked inside the revival of a still-owed
    /// re-run (`a`, upstream of `c`) is refused; the owed node itself, or
    /// one upstream of it, is not.
    #[test]
    fn a_rerun_inside_an_owed_revival_is_refused() {
        let p = report("");
        let mut e = failed_pass();
        e.push(EventPayload::NodesRevived {
            from_node: "a".into(),
            nodes: names(&["a", "b", "c", "c2", "done", "failed", "j"]),
        });
        let state = RunState::fold(&Run::new(e).events());
        let err = refuse_inside_owed(&p, &state, "c", Asker::Operator).unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("`a`") && text.contains("--from-node"),
            "{text}"
        );
        let err = refuse_inside_owed(&p, &state, "c2", Asker::Supervisor).unwrap_err();
        assert!(
            err.to_string().contains("retry or continue from `a`"),
            "{err}"
        );
        assert!(refuse_inside_owed(&p, &state, "a", Asker::Operator).is_ok());
        assert!(refuse_inside_owed(&p, &state, "start", Asker::Operator).is_ok());
    }

    /// A join already on the frontier is not offered twice.
    #[test]
    fn a_revived_join_on_the_frontier_is_left_alone() {
        let p = report("");
        let mut run = Run::new(failed_pass());
        rebuild(&p, &mut run, &["c"]);
        run.append(finished("c", "failed"));
        let mut frontier = names(&["j"]);
        assert!(sweep(&p, &mut run, "c", &mut frontier).is_empty());
        assert_eq!(frontier, names(&["j"]));
    }
}
