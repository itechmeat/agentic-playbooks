//! The frontier rebuild of a drive that is about to execute nodes again
//! (issue #203): a re-run resume and a supervisor retry of failed batch
//! members. Joins are weighed against [`parallel::revived`], and every earlier
//! `join_input_dead` write-off that view makes obsolete is journaled as
//! `join_input_revived`.

use super::*;

/// [`restore_frontier`] for a drive that executes every node of `starts`
/// again, `starts[0]` being the drive's `current`: the revived nodes read as
/// not yet run, so a join fed by their branches waits for them instead of
/// starting together with the re-run node, and a stale hop of a node
/// downstream of them is not re-offered as a head.
pub(crate) fn restore_revived_frontier(
    log: &mut EventLog,
    playbook: &Playbook,
    events: &[Event],
    starts: &[String],
    frontier: &mut Vec<String>,
) -> Result<(), EngineError> {
    let Some(current) = starts.first() else {
        return Ok(());
    };
    let state = parallel::revived(playbook, &RunState::fold(events), starts);
    for record in revived_inputs(playbook, events, &state, starts, frontier) {
        log.append(record)?;
    }
    restore_frontier(playbook, &state, current, frontier);
    Ok(())
}

/// The `JoinInputRevived` records for `state` (the revived fold): one per join
/// that has not finished in it and whose latest `JoinInputDead` wrote off a
/// source that can arrive again, listing exactly those sources. The active set
/// is the one [`restore_frontier`] weighs readiness with, so the record and the
/// verdict agree.
fn revived_inputs(
    playbook: &Playbook,
    events: &[Event],
    state: &RunState,
    starts: &[String],
    frontier: &[String],
) -> Vec<EventPayload> {
    let mut written_off: BTreeMap<&str, &[String]> = BTreeMap::new();
    for e in events {
        if let EventPayload::JoinInputDead { node, sources } = &e.payload {
            written_off.insert(node.as_str(), sources.as_slice());
        }
    }
    let Some((current, rest)) = starts.split_first() else {
        return Vec::new();
    };
    if written_off.is_empty() {
        return Vec::new();
    }
    let mut also: Vec<String> = rest.to_vec();
    also.extend(parallel::pending_heads(playbook, state));
    let active = active_set(current, frontier, &also);
    let mut out = Vec::new();
    for (join, dead) in written_off {
        if state.nodes.get(join).is_some_and(|s| s.is_finished()) {
            continue;
        }
        let pending = parallel::pending_inputs(playbook, join, state, &active);
        let sources: Vec<String> = dead
            .iter()
            .filter(|s| pending.contains(s))
            .cloned()
            .collect();
        let Some(first) = sources.first() else {
            continue;
        };
        let from_node = starts
            .iter()
            .find(|s| parallel::revived_nodes(playbook, s).contains(first))
            .unwrap_or(current)
            .clone();
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
    /// failed into `failed`, so `c2` was written off.
    const REPORT: &str = r#"
schema: 2
id: r
name: R
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: c, type: prompt, prompt: "c" }
  - { id: c2, type: prompt, prompt: "c2" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: done, type: finish, outcome: success }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: a }
  - { from: a, to: b }
  - { from: a, to: c }
  - { from: b, to: j }
  - { from: c, to: c2, condition: { type: node_status, node: c, equals: success } }
  - { from: c, to: failed, condition: { type: node_status, node: c, equals: failure } }
  - { from: c2, to: j }
  - { from: j, to: done }
"#;

    fn ev(seq: u64, payload: EventPayload) -> Event {
        Event {
            seq,
            ts: u128::from(seq),
            payload,
        }
    }

    fn finished(seq: u64, node: &str, status: &str) -> Vec<Event> {
        vec![
            ev(
                seq,
                EventPayload::NodeStarted {
                    node: node.into(),
                    attempt: 1,
                },
            ),
            ev(
                seq + 1,
                EventPayload::NodeFinished {
                    node: node.into(),
                    status: status.into(),
                    attempt: 1,
                    output: String::new(),
                    artifacts: Vec::new(),
                },
            ),
        ]
    }

    fn hop(seq: u64, from: &str, to: &str) -> Event {
        ev(
            seq,
            EventPayload::EdgeTraversed {
                from: from.into(),
                to: to.into(),
                via_policy: false,
                uncounted: false,
            },
        )
    }

    /// The first, failed pass of the report.
    fn failed_pass() -> Vec<Event> {
        let mut e = Vec::new();
        e.extend(finished(1, "start", "succeeded"));
        e.push(hop(3, "start", "a"));
        e.extend(finished(4, "a", "succeeded"));
        e.push(hop(6, "a", "b"));
        e.push(hop(7, "a", "c"));
        e.extend(finished(8, "b", "succeeded"));
        e.push(hop(10, "b", "j"));
        e.extend(finished(11, "c", "failed"));
        e.push(hop(13, "c", "failed"));
        e.push(ev(
            14,
            EventPayload::JoinInputDead {
                node: "j".into(),
                sources: vec!["c2".into()],
            },
        ));
        e
    }

    fn records(p: &Playbook, events: &[Event], starts: &[&str]) -> Vec<EventPayload> {
        let starts: Vec<String> = starts.iter().map(|s| (*s).to_string()).collect();
        let state = parallel::revived(p, &RunState::fold(events), &starts);
        revived_inputs(p, events, &state, &starts, &[])
    }

    #[test]
    fn a_rerun_revives_the_written_off_source() {
        let p = Playbook::from_yaml(REPORT).unwrap();
        let got = records(&p, &failed_pass(), &["c"]);
        assert_eq!(got.len(), 1);
        match &got[0] {
            EventPayload::JoinInputRevived {
                node,
                sources,
                from_node,
            } => {
                assert_eq!(node, "j");
                assert_eq!(sources, &vec!["c2".to_string()]);
                assert_eq!(from_node, "c");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// The supervisor retry path hands several starts: the record names the
    /// one whose re-run revives the source, not just the drive's `current`.
    #[test]
    fn a_retry_of_several_members_names_the_reviving_one() {
        let p = Playbook::from_yaml(REPORT).unwrap();
        let got = records(&p, &failed_pass(), &["b", "c"]);
        match got.as_slice() {
            [EventPayload::JoinInputRevived { from_node, .. }] => assert_eq!(from_node, "c"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_rerun_elsewhere_revives_nothing() {
        let p = Playbook::from_yaml(REPORT).unwrap();
        assert!(records(&p, &failed_pass(), &["b"]).is_empty());
    }

    #[test]
    fn no_write_off_no_record() {
        let p = Playbook::from_yaml(REPORT).unwrap();
        let mut events = failed_pass();
        events.pop();
        assert!(records(&p, &events, &["c"]).is_empty());
    }
}
