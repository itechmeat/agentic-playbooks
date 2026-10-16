//! The frontier rebuild of a drive that is about to execute nodes again
//! (issue #203): a re-run resume and a supervisor retry or continue-from of
//! failed batch members.
//!
//! The rebuild journals the revival as `nodes_revived`, so the fold reads the
//! revived nodes as not yet run from then on ([`RunState::revive`]): every
//! readiness decision of the drive, not only this rebuild, sees the same view
//! until each node starts again. It also journals `join_input_revived` for
//! every earlier write-off that view makes obsolete. [`reoffer_revived_joins`]
//! covers the one gap no edge does: a revived join whose revived input dies.

use super::*;

/// [`restore_frontier`] for a drive that executes every node of `starts`
/// again, `starts[0]` being the drive's `current`: journals the revival of
/// each start, then rebuilds the heads against the revived fold.
pub(crate) fn restore_revived_frontier(
    log: &mut EventLog,
    playbook: &Playbook,
    events: &[Event],
    starts: &[String],
    frontier: &mut Vec<String>,
) -> Result<(), EngineError> {
    let Some((current, rest)) = starts.split_first() else {
        return Ok(());
    };
    let mut state = RunState::fold(events);
    let revival = parallel::Revival::new(playbook, starts);
    for (start, nodes) in revival.per_start() {
        if !nodes.iter().any(|n| carries_a_result(&state, n)) {
            continue;
        }
        log.append(EventPayload::NodesRevived {
            from_node: start.to_string(),
            nodes: nodes.iter().cloned().collect(),
        })?;
        // Exactly what the fold does with the record just written.
        state.revive(nodes);
    }
    let mut also: Vec<String> = rest.to_vec();
    also.extend(parallel::pending_heads(playbook, &state));
    let active = active_set(current, frontier, &also);
    for record in revived_inputs(playbook, events, &state, &revival, &active) {
        log.append(record)?;
    }
    restore_frontier(playbook, &state, current, frontier);
    Ok(())
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
        restore_revived_frontier(&mut run.log, p, &events, &names(starts), &mut frontier).unwrap();
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
