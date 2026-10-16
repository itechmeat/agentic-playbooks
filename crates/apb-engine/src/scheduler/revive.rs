//! The frontier rebuild of a drive that is about to execute nodes again
//! (issue #203): a re-run resume and a supervisor retry of failed batch
//! members. Joins are weighed against the revived view
//! ([`parallel::Revival`]), every earlier `join_input_dead` write-off that view
//! makes obsolete is journaled as `join_input_revived`, and a join the rebuild
//! holds back is re-offered once its revived inputs have settled.

use super::*;

/// The joins a revived rebuild held back because a revived input could still
/// arrive. Nothing else re-offers such a join when that input then dies (the
/// re-run node fails again, or routes elsewhere): no edge delivers into it, so
/// [`advance_frontier`] never sees it. [`Withheld::reoffer`] does, after every
/// advance of the drive.
#[derive(Debug, Default)]
pub(crate) struct Withheld {
    entries: Vec<WithheldJoin>,
}

#[derive(Debug)]
struct WithheldJoin {
    join: String,
    /// The journal length at the rebuild: a `node_started` after it belongs
    /// to this pass.
    since: usize,
    /// The nodes this pass re-executes, as the rebuild revived them.
    revived: BTreeSet<String>,
}

impl Withheld {
    pub(crate) fn extend(&mut self, other: Withheld) {
        for e in other.entries {
            self.entries.retain(|w| w.join != e.join);
            self.entries.push(e);
        }
    }

    /// Re-offers every held join whose readiness has been decided, with the
    /// revived nodes that have not started again in this pass still read as
    /// not yet run. A join already on the frontier, or started in this pass,
    /// is no longer held. A ready join is offered exactly as
    /// [`advance_frontier`] offers one: its dead inputs journaled, then pushed.
    pub(crate) fn reoffer(
        &mut self,
        playbook: &Playbook,
        run_dir: &Path,
        log: &mut EventLog,
        node: &str,
        also_active: &[String],
        frontier: &mut Vec<String>,
    ) -> Result<(), EngineError> {
        if self.entries.is_empty() {
            return Ok(());
        }
        let events = read_all(run_dir)?;
        let real = RunState::fold(&events);
        let mut kept = Vec::new();
        for w in std::mem::take(&mut self.entries) {
            let started = started_since(&events, w.since);
            if frontier.contains(&w.join) || started.contains(&w.join) {
                continue;
            }
            let still: BTreeSet<String> = w.revived.difference(&started).cloned().collect();
            let state = parallel::revive_state(&real, &still);
            let active = active_set(node, frontier, also_active);
            if matches!(
                parallel::join_readiness(playbook, &w.join, &state, &active),
                JoinReadiness::NotReady
            ) {
                kept.push(w);
                continue;
            }
            journal_dead_inputs(log, playbook, &w.join, &state, &active)?;
            frontier.push(w.join);
        }
        self.entries = kept;
        Ok(())
    }
}

fn started_since(events: &[Event], since: usize) -> BTreeSet<String> {
    events
        .iter()
        .skip(since)
        .filter_map(|e| match &e.payload {
            EventPayload::NodeStarted { node, .. } => Some(node.clone()),
            _ => None,
        })
        .collect()
}

/// [`restore_frontier`] for a drive that executes every node of `starts`
/// again, `starts[0]` being the drive's `current`: the revived nodes read as
/// not yet run, so a join fed by their branches waits for them instead of
/// starting together with the re-run node, and a stale hop into a revived node
/// is not re-offered.
///
/// A rebuilt head that is itself revived (not a start) is handled apart from
/// the plain rule, because the hop that makes it a head comes from a node
/// outside the revival, recorded in the old pass:
/// - a join still waiting for a revived input is held back ([`Withheld`]);
/// - one that finished in the old pass, and would be ready now (`join: any`,
///   a first-arrival merge inside a cycle), is not offered at all: running it
///   again on the old input would run it twice. This pass re-runs it when a
///   revived branch delivers into it;
/// - one that never finished is offered as before.
pub(crate) fn restore_revived_frontier(
    log: &mut EventLog,
    playbook: &Playbook,
    events: &[Event],
    starts: &[String],
    frontier: &mut Vec<String>,
) -> Result<Withheld, EngineError> {
    let mut withheld = Withheld::default();
    let Some((current, rest)) = starts.split_first() else {
        return Ok(withheld);
    };
    let real = RunState::fold(events);
    let revival = parallel::Revival::new(playbook, starts);
    let state = parallel::revive_state(&real, revival.nodes());
    let heads = parallel::pending_heads(playbook, &state);
    let mut also: Vec<String> = rest.to_vec();
    also.extend(heads.iter().cloned());
    let active = active_set(current, frontier, &also);
    for record in revived_inputs(playbook, events, &state, &revival, &active) {
        log.append(record)?;
    }
    for head in &heads {
        if head == current || frontier.contains(head) {
            continue;
        }
        let revived = revival.contains(head) && !starts.contains(head);
        if parallel::is_join(playbook, head)
            && matches!(
                parallel::join_readiness(playbook, head, &state, &active),
                JoinReadiness::NotReady
            )
        {
            if revived {
                withheld.entries.push(WithheldJoin {
                    join: head.clone(),
                    since: events.len(),
                    revived: revival.nodes().clone(),
                });
            }
            continue;
        }
        if revived && real.nodes.get(head).is_some_and(|s| s.is_finished()) {
            continue;
        }
        frontier.push(head.clone());
    }
    Ok(withheld)
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
        if state.nodes.get(join).is_some_and(|s| s.is_finished()) {
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

    /// What one revived rebuild did: the records it journaled, the frontier
    /// it built, the joins it held back.
    struct Rebuilt {
        records: Vec<EventPayload>,
        frontier: Vec<String>,
        withheld: Withheld,
    }

    impl Rebuilt {
        fn held(&self) -> Vec<String> {
            self.withheld
                .entries
                .iter()
                .map(|w| w.join.clone())
                .collect()
        }
    }

    fn rebuild(p: &Playbook, run: &mut Run, starts: &[&str]) -> Rebuilt {
        let events = run.events();
        let mut frontier = Vec::new();
        let withheld =
            restore_revived_frontier(&mut run.log, p, &events, &names(starts), &mut frontier)
                .unwrap();
        let records = run.events()[events.len()..]
            .iter()
            .map(|e| e.payload.clone())
            .collect();
        Rebuilt {
            records,
            frontier,
            withheld,
        }
    }

    #[test]
    fn a_rerun_revives_the_written_off_source_and_holds_the_join() {
        let p = report("");
        let mut run = Run::new(failed_pass());
        let r = rebuild(&p, &mut run, &["c"]);
        assert_eq!(dbg(&r.records), dbg(&[revived_record("j", &["c2"], "c")]));
        // `c` is the drive's current; the join waits, the failure route is gone.
        assert!(r.frontier.is_empty());
        assert_eq!(r.held(), names(&["j"]));
    }

    /// The supervisor retry path hands several starts: the record names the
    /// one whose re-run revives the source, not just the drive's `current`.
    #[test]
    fn a_retry_of_several_members_names_the_reviving_one() {
        let p = report("");
        let mut run = Run::new(failed_pass());
        let r = rebuild(&p, &mut run, &["b", "c"]);
        assert_eq!(dbg(&r.records), dbg(&[revived_record("j", &["c2"], "c")]));
        assert_eq!(r.frontier, names(&["c"]));
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
        assert!(r.held().is_empty());
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
        assert!(r.held().is_empty());
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

    /// The held join is re-offered once the re-run node fails again and its
    /// branch is dead: the fresh write-off is journaled and the join pushed.
    #[test]
    fn a_held_join_is_reoffered_when_the_revived_branch_dies() {
        let p = report("");
        let mut run = Run::new(failed_pass());
        let mut r = rebuild(&p, &mut run, &["c"]);
        let mut frontier = Vec::new();

        run.append(vec![started("c")]);
        r.withheld
            .reoffer(&p, run.dir.path(), &mut run.log, "c", &[], &mut frontier)
            .unwrap();
        assert!(frontier.is_empty(), "still waiting while c runs");

        run.append(finished("c", "failed")[1..].to_vec());
        run.append(vec![hop("c", "failed")]);
        let before = run.events().len();
        frontier.push("failed".into());
        r.withheld
            .reoffer(&p, run.dir.path(), &mut run.log, "c", &[], &mut frontier)
            .unwrap();
        assert_eq!(frontier, names(&["failed", "j"]));
        let fresh: Vec<EventPayload> = run.events()[before..]
            .iter()
            .map(|e| e.payload.clone())
            .collect();
        assert_eq!(dbg(&fresh), dbg(&[dead("j", &["c2"])]));
        assert!(r.held().is_empty());
    }

    /// Once the revived branch delivers, the normal advance offers the join;
    /// the hold then drops it without offering it twice.
    #[test]
    fn a_held_join_is_released_once_it_is_on_the_frontier() {
        let p = report("");
        let mut run = Run::new(failed_pass());
        let mut r = rebuild(&p, &mut run, &["c"]);
        let mut frontier = names(&["j"]);
        r.withheld
            .reoffer(&p, run.dir.path(), &mut run.log, "c2", &[], &mut frontier)
            .unwrap();
        assert_eq!(frontier, names(&["j"]));
        assert!(r.held().is_empty());
    }
}
