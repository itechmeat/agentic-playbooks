//! Execution, list visit and attempt identity, derived from event order.
//!
//! No event carries an execution ordinal, and none gains one: every ordinal
//! here is a pure function of a journal, so every binary that reads the same
//! journal derives the same identities.
//!
//! - **Execution** `n` of a node is the work ended by its `n`-th
//!   `node_finished` (1-based). It begins at the first `node_started` after
//!   the previous `node_finished`, or at its own `node_finished` when none
//!   was written (a finish without a prompt). Interactive re-invocations,
//!   retries and attempts a pause interrupted belong to the open execution.
//!   A `join: any` race loser is an execution with status `cancelled`.
//! - **Attempt id**: the seq of the attempt's `attempt_started`.
//! - **List visit**: starts at a `list_started` that is not a reopen and may
//!   span several executions of its node ([`ListVisitRef`]).
//!
//! Two attribution rules are kept apart: [`ExecutionIndex::containing`] for
//! an event inside an execution (attempts, the judge node's decision,
//! questions and reviews), and [`ExecutionIndex::routed_by`] for the routing
//! of a source after its execution ended (hops, judge-edge decisions,
//! `route_evaluated` records).

use std::collections::BTreeMap;

use crate::event::{Event, EventPayload};

/// One execution of a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Execution {
    pub node: String,
    /// 1-based, in journal order.
    pub ordinal: u32,
    /// The seq of its `node_started`, or of its `node_finished` when it
    /// wrote none.
    pub begin_seq: u64,
    /// The seq of its `node_finished`; `None` while it is open (running,
    /// paused, or cut short by a crash).
    pub end_seq: Option<u64>,
    /// The status its `node_finished` recorded.
    pub status: Option<String>,
    /// A `join: any` race loser.
    pub cancelled: bool,
}

impl Execution {
    pub fn is_open(&self) -> bool {
        self.end_seq.is_none()
    }

    pub fn reference(&self) -> ExecutionRef {
        ExecutionRef {
            node: self.node.clone(),
            ordinal: self.ordinal,
            ended_seq: self.end_seq,
        }
    }
}

/// A reference to one execution of a node.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExecutionRef {
    pub node: String,
    pub ordinal: u32,
    pub ended_seq: Option<u64>,
}

/// A visit of a list node: its non-reopen `list_started`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ListVisitRef {
    pub node: String,
    pub started_seq: u64,
}

/// Every execution of every node of one journal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecutionIndex {
    by_node: BTreeMap<String, Vec<Execution>>,
}

/// The execution index of `events`.
pub fn index(events: &[Event]) -> ExecutionIndex {
    let mut by_node: BTreeMap<String, Vec<Execution>> = BTreeMap::new();
    let mut open: BTreeMap<&str, u64> = BTreeMap::new();
    for e in events {
        match &e.payload {
            EventPayload::NodeStarted { node, .. } => {
                open.entry(node.as_str()).or_insert(e.seq);
            }
            EventPayload::NodeFinished { node, status, .. } => {
                let begin_seq = open.remove(node.as_str()).unwrap_or(e.seq);
                let list = by_node.entry(node.clone()).or_default();
                list.push(Execution {
                    node: node.clone(),
                    ordinal: list.len() as u32 + 1,
                    begin_seq,
                    end_seq: Some(e.seq),
                    status: Some(status.clone()),
                    cancelled: status == "cancelled",
                });
            }
            _ => {}
        }
    }
    for (node, begin_seq) in open {
        let list = by_node.entry(node.to_string()).or_default();
        list.push(Execution {
            node: node.to_string(),
            ordinal: list.len() as u32 + 1,
            begin_seq,
            end_seq: None,
            status: None,
            cancelled: false,
        });
    }
    ExecutionIndex { by_node }
}

impl ExecutionIndex {
    /// The executions of `node`, in order.
    pub fn executions(&self, node: &str) -> &[Execution] {
        self.by_node.get(node).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Every node with at least one execution, with its executions.
    pub fn nodes(&self) -> impl Iterator<Item = (&str, &[Execution])> {
        self.by_node.iter().map(|(n, v)| (n.as_str(), v.as_slice()))
    }

    /// The execution of `node` an event at `seq` happened inside.
    pub fn containing(&self, node: &str, seq: u64) -> Option<&Execution> {
        self.executions(node)
            .iter()
            .find(|x| x.begin_seq <= seq && x.end_seq.is_none_or(|end| seq <= end))
    }

    /// The execution of `node` whose routing an event at `seq` belongs to:
    /// its latest execution that ended before `seq`. A finish without a
    /// prompt qualifies, and a resume that advances again after the same
    /// `node_finished` attributes to that same execution.
    pub fn routed_by(&self, node: &str, seq: u64) -> Option<&Execution> {
        self.executions(node)
            .iter()
            .rev()
            .find(|x| x.end_seq.is_some_and(|end| end < seq))
    }

    /// The execution a judge-edge `decision_made` at `seq` answered, when
    /// its `attempt` (the source's finish count) agrees with
    /// [`ExecutionIndex::routed_by`]; `None` (unknown) otherwise.
    pub fn judge_edge_execution(&self, node: &str, seq: u64, attempt: u32) -> Option<&Execution> {
        self.routed_by(node, seq).filter(|x| x.ordinal == attempt)
    }
}

/// Every visit of every list node, in journal order.
pub fn list_visits(events: &[Event]) -> Vec<ListVisitRef> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListStarted {
                node,
                reopened: false,
                ..
            } => Some(ListVisitRef {
                node: node.clone(),
                started_seq: e.seq,
            }),
            _ => None,
        })
        .collect()
}

/// The attempt id of an `attempt_started` event: its seq.
pub fn attempt_id(event: &Event) -> Option<u64> {
    matches!(event.payload, EventPayload::AttemptStarted { .. }).then_some(event.seq)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, payload: EventPayload) -> Event {
        Event {
            seq,
            ts: 0,
            payload,
        }
    }

    fn json(seq: u64, body: &str) -> Event {
        let mut v: serde_json::Value = serde_json::from_str(body).unwrap();
        v["seq"] = seq.into();
        v["ts"] = 0.into();
        serde_json::from_value(v).unwrap()
    }

    fn started(node: &str) -> EventPayload {
        EventPayload::NodeStarted {
            node: node.into(),
            attempt: 1,
        }
    }

    fn finished(node: &str, status: &str) -> EventPayload {
        EventPayload::NodeFinished {
            node: node.into(),
            status: status.into(),
            attempt: 1,
            output: String::new(),
            artifacts: Vec::new(),
        }
    }

    fn hop(from: &str, to: &str) -> EventPayload {
        EventPayload::hop(from, to, false, true, Vec::new())
    }

    #[test]
    fn a_finish_without_a_prompt_is_an_execution_and_routes() {
        let events = vec![
            ev(1, started("a")),
            ev(2, finished("a", "succeeded")),
            ev(3, hop("a", "fin")),
            ev(4, finished("fin", "succeeded")),
        ];
        let idx = index(&events);
        let fin = idx.executions("fin");
        assert_eq!(fin.len(), 1);
        assert_eq!((fin[0].begin_seq, fin[0].end_seq), (4, Some(4)));
        assert_eq!(idx.containing("fin", 4).unwrap().ordinal, 1);
        assert_eq!(idx.routed_by("a", 3).unwrap().ordinal, 1);
        assert!(idx.routed_by("fin", 4).is_none());
        assert_eq!(idx.routed_by("fin", 5).unwrap().ordinal, 1);
    }

    #[test]
    fn a_quota_paused_execution_stays_open_and_owns_its_attempts() {
        let events = vec![
            ev(1, started("a")),
            json(
                2,
                r#"{"type":"attempt_started","node":"a","attempt":1,"agent":"x"}"#,
            ),
            json(3, r#"{"type":"run_paused","reason":"quota"}"#),
        ];
        let idx = index(&events);
        let a = idx.executions("a");
        assert_eq!(a.len(), 1);
        assert!(a[0].is_open());
        assert_eq!(idx.containing("a", 2).unwrap().ordinal, 1);
        assert!(idx.routed_by("a", 4).is_none());
        assert_eq!(attempt_id(&events[1]), Some(2));
    }

    #[test]
    fn an_interactive_reinvocation_stays_in_one_execution() {
        // The node starts, its attempt asks a question, the node is invoked
        // again with the answer (a second start and attempt), then one
        // finish: one execution containing both attempts.
        let events = vec![
            ev(1, started("a")),
            json(
                2,
                r#"{"type":"attempt_started","node":"a","attempt":1,"agent":"x"}"#,
            ),
            json(3, r#"{"type":"question_asked","node":"a","question":"q"}"#),
            ev(4, started("a")),
            json(
                5,
                r#"{"type":"attempt_started","node":"a","attempt":2,"agent":"x"}"#,
            ),
            ev(6, finished("a", "succeeded")),
        ];
        let idx = index(&events);
        let a = idx.executions("a");
        assert_eq!(a.len(), 1);
        assert_eq!((a[0].begin_seq, a[0].end_seq), (1, Some(6)));
        assert_eq!(idx.containing("a", 2).unwrap().ordinal, 1);
        assert_eq!(idx.containing("a", 5).unwrap().ordinal, 1);
        assert_eq!(attempt_id(&events[4]), Some(5));
    }

    #[test]
    fn a_quota_pause_and_its_resume_stay_in_one_execution() {
        let events = vec![
            ev(1, started("a")),
            json(
                2,
                r#"{"type":"attempt_started","node":"a","attempt":1,"agent":"x"}"#,
            ),
            json(
                3,
                r#"{"type":"attempt_finished","node":"a","attempt":1,"status":"interrupted","duration_ms":1}"#,
            ),
            json(4, r#"{"type":"run_paused","reason":"quota"}"#),
            json(5, r#"{"type":"run_resumed","from_node":"a"}"#),
            ev(6, started("a")),
            json(
                7,
                r#"{"type":"attempt_started","node":"a","attempt":1,"agent":"x"}"#,
            ),
            ev(8, finished("a", "succeeded")),
            ev(9, hop("a", "b")),
        ];
        let idx = index(&events);
        let a = idx.executions("a");
        assert_eq!(a.len(), 1);
        assert_eq!((a[0].begin_seq, a[0].end_seq), (1, Some(8)));
        assert_eq!(idx.containing("a", 2).unwrap().ordinal, 1);
        assert_eq!(idx.containing("a", 7).unwrap().ordinal, 1);
        assert_eq!(idx.routed_by("a", 9).unwrap().ordinal, 1);
    }

    #[test]
    fn a_race_loser_is_a_cancelled_execution() {
        let events = vec![ev(1, started("b")), ev(2, finished("b", "cancelled"))];
        let idx = index(&events);
        assert!(idx.executions("b")[0].cancelled);
    }

    #[test]
    fn a_resume_that_advances_again_attributes_to_the_same_execution() {
        let events = vec![
            ev(1, started("a")),
            ev(2, finished("a", "succeeded")),
            ev(3, hop("a", "b")),
            json(
                4,
                r#"{"type":"run_resumed","from_node":"b","mode":"after"}"#,
            ),
            ev(5, hop("a", "b")),
        ];
        let idx = index(&events);
        assert_eq!(idx.routed_by("a", 3), idx.routed_by("a", 5));
        assert_eq!(idx.routed_by("a", 5).unwrap().ordinal, 1);
    }

    #[test]
    fn a_judge_edge_decision_after_the_finish_is_checked_against_its_attempt() {
        let events = vec![
            ev(1, started("a")),
            ev(2, finished("a", "succeeded")),
            ev(4, started("a")),
            ev(5, finished("a", "succeeded")),
        ];
        let idx = index(&events);
        assert_eq!(idx.judge_edge_execution("a", 6, 2).unwrap().ordinal, 2);
        assert!(idx.judge_edge_execution("a", 6, 1).is_none());
        assert_eq!(idx.judge_edge_execution("a", 3, 1).unwrap().ordinal, 1);
    }
}
