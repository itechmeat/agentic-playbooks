//! Whether a check's result still describes the tree the run left (spec
//! 2026-11-03-evidence-freshness-design): the one reading of
//! `evidence_bound` and `evidence_observed` every outcome surface shows.
//!
//! - A script `success_check` with a declared scope is read from the
//!   binding its node's latest successful execution journaled, against the
//!   finish's fingerprint of the same root and scope: equal is `current`,
//!   different is `stale`.
//! - Without that comparison (a run under way, one that ended before a
//!   finish node, a fingerprint that could not be taken), for a check that
//!   declares no scope (a git fingerprint sees neither ignored files nor
//!   paths above the check's directory, so equal ones prove nothing), for a
//!   pass restored from the node cache, which bound nothing in this run,
//!   and for a list's script item check, which binds nothing: `unknown`
//!   once a node that can act (a `read_only` node aside) started after the
//!   check or was still running when it passed, else `current`.
//! - A success check with no binding and no cache hit (an older apb, a
//!   scenario run's scripted check) carries no freshness and reads as
//!   before.

use apb_core::schema::Playbook;

use super::claims::{Freshness, FreshnessState};
use crate::event::{Event, EventPayload};

/// The latest `node_finished { succeeded }` of `node` and the latest
/// `node_started` of it before that, as journal seqs.
fn latest_success(events: &[Event], node: &str) -> Option<(u64, u64)> {
    let finished = events.iter().rev().find(|e| {
        matches!(&e.payload, EventPayload::NodeFinished { node: n, status, .. }
            if n == node && status == "succeeded")
    })?;
    let started = events
        .iter()
        .rev()
        .filter(|e| e.seq < finished.seq)
        .find(|e| matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node))
        .map_or(0, |e| e.seq);
    Some((started, finished.seq))
}

/// The events of `node`'s latest successful execution, newest first.
fn success_window<'a>(events: &'a [Event], node: &str) -> impl Iterator<Item = &'a Event> {
    let (started, finished) = latest_success(events, node).unwrap_or((0, 0));
    events
        .iter()
        .rev()
        .filter(move |e| e.seq > started && e.seq < finished)
}

/// The binding of `node`'s latest successful execution: its seq, root,
/// scope and fingerprint. The one selection the outcome reads and the
/// finish observes.
pub(crate) fn latest_binding<'a>(
    events: &'a [Event],
    node: &str,
) -> Option<(u64, &'a String, &'a Vec<String>, Option<&'a str>)> {
    success_window(events, node).find_map(|e| match &e.payload {
        EventPayload::EvidenceBound {
            node: n,
            root,
            scope,
            fingerprint,
            ..
        } if n == node => Some((e.seq, root, scope, fingerprint.as_deref())),
        _ => None,
    })
}

/// The cache hit of `node`'s latest successful execution: its seq and the
/// run that produced the cached result.
fn cache_hit<'a>(events: &'a [Event], node: &str) -> Option<(u64, &'a str)> {
    success_window(events, node).find_map(|e| match &e.payload {
        EventPayload::NodeCacheHit {
            node: n,
            source_run,
            ..
        } if n == node => Some((e.seq, source_run.as_str())),
        _ => None,
    })
}

/// The run whose cached result `node`'s latest successful execution
/// restored, when it was restored rather than executed.
pub fn cache_source(events: &[Event], node: &str) -> Option<String> {
    cache_hit(events, node).map(|(_, run)| run.to_string())
}

/// Whether `node` can change the tree: not a `read_only` one; a node the
/// playbook snapshot does not name counts as one that can.
fn can_act(playbook: Option<&Playbook>, node: &str) -> bool {
    playbook
        .and_then(|p| p.node(node))
        .is_none_or(|n| apb_core::effects::node_acts(n) && !n.read_only)
}

/// A node other than `own` that can change the tree and either started
/// after `seq` or was still running at it (started before, finished after
/// or never), with how.
fn acted_after<'a>(
    playbook: Option<&Playbook>,
    events: &'a [Event],
    seq: u64,
    own: &str,
) -> Option<(&'a str, &'static str)> {
    let mut running: Vec<&str> = Vec::new();
    for e in events {
        match &e.payload {
            EventPayload::NodeStarted { node, .. } if node != own => {
                if !can_act(playbook, node) {
                    continue;
                }
                if e.seq > seq {
                    return Some((node.as_str(), "ran after the check"));
                }
                running.push(node.as_str());
            }
            EventPayload::NodeFinished { node, .. } if e.seq < seq => {
                running.retain(|n| n != node);
            }
            _ => {}
        }
    }
    running
        .first()
        .map(|n| (*n, "was still running when the check passed"))
}

/// Rule 3: `unknown` once a node that can act started after `seq` or was
/// running at it.
fn by_later_nodes(
    playbook: Option<&Playbook>,
    events: &[Event],
    seq: u64,
    own: &str,
    why: &str,
) -> Freshness {
    match acted_after(playbook, events, seq, own) {
        Some((node, how)) => Freshness {
            state: FreshnessState::Unknown,
            reason: Some(format!(
                "node {node} {how} and nothing checked again; {why}"
            )),
        },
        None => Freshness {
            state: FreshnessState::Current,
            reason: None,
        },
    }
}

/// The freshness of `node`'s passing script `success_check`; `None` when
/// its latest successful execution neither bound it nor restored it from
/// the cache.
pub fn success_check(
    playbook: Option<&Playbook>,
    events: &[Event],
    node: &str,
) -> Option<Freshness> {
    let Some((seq, root, scope, bound)) = latest_binding(events, node) else {
        let (seq, source) = cache_hit(events, node)?;
        return Some(by_later_nodes(
            playbook,
            events,
            seq,
            node,
            &format!("the pass was restored from the cache of run {source}"),
        ));
    };
    if scope.is_empty() {
        return Some(by_later_nodes(
            playbook,
            events,
            seq,
            node,
            "the check declares no scope, so a change it reads could go unseen",
        ));
    }
    let Some(bound) = bound else {
        return Some(by_later_nodes(
            playbook,
            events,
            seq,
            node,
            "the check's scope could not be fingerprinted",
        ));
    };
    let observed = events
        .iter()
        .rev()
        .filter(|e| e.seq > seq)
        .find_map(|e| match &e.payload {
            EventPayload::EvidenceObserved { observations } => observations
                .iter()
                .find(|o| &o.root == root && &o.scope == scope)
                .map(|o| o.fingerprint.as_deref()),
            _ => None,
        });
    Some(match observed {
        None => by_later_nodes(
            playbook,
            events,
            seq,
            node,
            "the run did not reach a finish node",
        ),
        Some(None) => Freshness {
            state: FreshnessState::Unknown,
            reason: Some("the check's scope could not be fingerprinted at the finish".into()),
        },
        Some(Some(now)) if now == bound => Freshness {
            state: FreshnessState::Current,
            reason: None,
        },
        Some(Some(_)) => Freshness {
            state: FreshnessState::Stale,
            reason: Some(format!(
                "a file in its scope ({}) changed after the check",
                scope.join(", ")
            )),
        },
    })
}

/// The freshness of a list node's script item checks, read from the list's
/// latest settlement: they bind nothing.
pub fn item_check(playbook: Option<&Playbook>, events: &[Event], node: &str) -> Option<Freshness> {
    let seq = events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::NodeFinished { node: n, .. } if n == node => Some(e.seq),
        EventPayload::ListItemSettled { node: n, .. } if n == node => Some(e.seq),
        _ => None,
    })?;
    Some(by_later_nodes(
        playbook,
        events,
        seq,
        node,
        "item checks are not bound to a fingerprint",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAYBOOK: &str = "schema: 2\nid: f\nname: f\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: agent_task, prompt: p, success_check: { script: scripts/c.sh, scope: [\"src/**\"] } }\n  - { id: look, type: agent_task, prompt: p, read_only: true }\n  - { id: rows, type: agent_task, prompt: p, list: { items: { inline: [ { id: a } ] }, check: { script: scripts/c.sh } } }\n  - { id: edit, type: agent_task, prompt: p }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: look }\n  - { from: look, to: edit }\n  - { from: edit, to: done }\n";

    /// Events from one JSON payload per line, numbered from 1.
    fn journal(lines: &[&str]) -> Vec<Event> {
        lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
                v["seq"] = (i as u64 + 1).into();
                v["ts"] = 0.into();
                serde_json::from_value(v).unwrap()
            })
            .collect()
    }

    const START_W: &str = r#"{"type":"node_started","node":"w","attempt":1}"#;
    const BOUND_A: &str = r#"{"type":"evidence_bound","node":"w","attempt":1,"check":"success_check","root":"/t","scope":["src/**"],"fingerprint":"a"}"#;
    const DONE_W: &str =
        r#"{"type":"node_finished","node":"w","status":"succeeded","attempt":1,"output":""}"#;

    fn observed(fp: &str) -> String {
        format!(
            r#"{{"type":"evidence_observed","observations":[{{"root":"/t","scope":["src/**"],"fingerprint":"{fp}"}}]}}"#
        )
    }

    fn state(events: &[Event]) -> Option<FreshnessState> {
        let pb = Playbook::from_yaml(PLAYBOOK).unwrap();
        success_check(Some(&pb), events, "w").map(|f| f.state)
    }

    #[test]
    fn the_finish_fingerprint_decides_against_the_latest_binding() {
        let b = observed("b");
        assert_eq!(
            state(&journal(&[START_W, BOUND_A, DONE_W, &observed("a")])),
            Some(FreshnessState::Current)
        );
        assert_eq!(
            state(&journal(&[START_W, BOUND_A, DONE_W, &b])),
            Some(FreshnessState::Stale)
        );
        // A recheck: the node ran again after the edit and bound B.
        let bound_b = BOUND_A.replace(r#""fingerprint":"a""#, r#""fingerprint":"b""#);
        assert_eq!(
            state(&journal(&[
                START_W, BOUND_A, DONE_W, START_W, &bound_b, DONE_W, &b
            ])),
            Some(FreshnessState::Current)
        );
    }

    #[test]
    fn without_a_finish_only_a_node_that_can_act_makes_it_unknown() {
        let look = r#"{"type":"node_started","node":"look","attempt":1}"#;
        let edit = r#"{"type":"node_started","node":"edit","attempt":1}"#;
        assert_eq!(
            state(&journal(&[START_W, BOUND_A, DONE_W])),
            Some(FreshnessState::Current)
        );
        assert_eq!(
            state(&journal(&[START_W, BOUND_A, DONE_W, look])),
            Some(FreshnessState::Current),
            "a read_only node cannot change the tree"
        );
        assert_eq!(
            state(&journal(&[START_W, BOUND_A, DONE_W, look, edit])),
            Some(FreshnessState::Unknown)
        );
    }

    #[test]
    fn a_success_with_no_binding_carries_no_freshness() {
        assert_eq!(state(&journal(&[START_W, DONE_W])), None);
        // A binding of an earlier execution does not speak for a later one.
        assert_eq!(
            state(&journal(&[START_W, BOUND_A, DONE_W, START_W, DONE_W])),
            None
        );
    }

    const START_EDIT: &str = r#"{"type":"node_started","node":"edit","attempt":1}"#;
    const DONE_EDIT: &str =
        r#"{"type":"node_finished","node":"edit","status":"succeeded","attempt":1,"output":""}"#;

    #[test]
    fn a_node_in_flight_when_the_check_passed_makes_it_unknown() {
        // `edit` started before the check bound and finished after it: it
        // may have written after the check read the tree.
        let f = success_check(
            Some(&Playbook::from_yaml(PLAYBOOK).unwrap()),
            &journal(&[START_W, START_EDIT, BOUND_A, DONE_W, DONE_EDIT]),
            "w",
        )
        .unwrap();
        assert_eq!(f.state, FreshnessState::Unknown, "{f:?}");
        assert!(f.reason.unwrap().contains("node edit"));
        // Still running when the journal ends: the same.
        assert_eq!(
            state(&journal(&[START_W, START_EDIT, BOUND_A, DONE_W])),
            Some(FreshnessState::Unknown)
        );
        // Finished before the check read the tree: the check saw its work.
        assert_eq!(
            state(&journal(&[START_EDIT, DONE_EDIT, START_W, BOUND_A, DONE_W])),
            Some(FreshnessState::Current)
        );
    }

    #[test]
    fn a_pass_restored_from_the_cache_is_read_from_the_restore() {
        let hit = r#"{"type":"node_cache_hit","node":"w","key":"k","source_run":"r0"}"#;
        let pb = Playbook::from_yaml(PLAYBOOK).unwrap();
        assert_eq!(
            state(&journal(&[START_W, hit, DONE_W])),
            Some(FreshnessState::Current)
        );
        let f = success_check(
            Some(&pb),
            &journal(&[START_W, hit, DONE_W, START_EDIT]),
            "w",
        )
        .unwrap();
        assert_eq!(f.state, FreshnessState::Unknown, "{f:?}");
        let reason = f.reason.unwrap();
        assert!(reason.contains("node edit ran after"), "{reason}");
        assert!(reason.contains("cache of run r0"), "{reason}");
        assert_eq!(
            cache_source(&journal(&[START_W, hit, DONE_W]), "w").as_deref(),
            Some("r0")
        );
        assert_eq!(
            cache_source(&journal(&[START_W, BOUND_A, DONE_W]), "w"),
            None
        );
    }

    #[test]
    fn an_unscoped_pass_is_read_by_the_later_nodes_alone() {
        let bound = r#"{"type":"evidence_bound","node":"w","attempt":1,"check":"success_check","root":"/t","scope":[]}"#;
        assert_eq!(
            state(&journal(&[START_W, bound, DONE_W])),
            Some(FreshnessState::Current)
        );
        // Equal git fingerprints say nothing of ignored files or of paths
        // above the root, so a node that can act still makes it unknown.
        let same = r#"{"type":"evidence_observed","observations":[{"root":"/t","scope":[],"fingerprint":"a"}]}"#;
        let pb = Playbook::from_yaml(PLAYBOOK).unwrap();
        let f = success_check(
            Some(&pb),
            &journal(&[START_W, bound, DONE_W, START_EDIT, DONE_EDIT, same]),
            "w",
        )
        .unwrap();
        assert_eq!(f.state, FreshnessState::Unknown, "{f:?}");
        assert!(f.reason.unwrap().contains("declares no scope"));
    }

    #[test]
    fn the_binding_read_is_the_one_of_the_latest_success() {
        let bound_r2 = BOUND_A.replace(r#""root":"/t""#, r#""root":"/r2""#);
        let failed =
            r#"{"type":"node_finished","node":"w","status":"failed","attempt":2,"output":""}"#;
        let events = journal(&[START_W, BOUND_A, DONE_W, START_W, &bound_r2, failed]);
        let (_, root, scope, _) = latest_binding(&events, "w").unwrap();
        assert_eq!((root.as_str(), scope.len()), ("/t", 1));
    }

    #[test]
    fn a_list_item_check_is_unknown_once_a_node_that_can_act_ran_after_it() {
        let pb = Playbook::from_yaml(PLAYBOOK).unwrap();
        let settled = r#"{"type":"list_item_settled","node":"rows","item":"a","state":"passed"}"#;
        let events = journal(&[settled]);
        assert_eq!(
            item_check(Some(&pb), &events, "rows").map(|f| f.state),
            Some(FreshnessState::Current)
        );
        let events = journal(&[
            settled,
            r#"{"type":"node_started","node":"edit","attempt":1}"#,
        ]);
        let f = item_check(Some(&pb), &events, "rows").unwrap();
        assert_eq!(f.state, FreshnessState::Unknown);
        assert!(f.reason.unwrap().contains("not bound"));
    }
}
