//! The budget as the journal leaves it: the limit and policy in force, how
//! often they were set, the latest gate pause, and a pending one-time
//! waiver.

use apb_core::budget::{BudgetLimit, BudgetSource, OnNoEstimate};

use super::kind::{BudgetAmount, GateBasis, GateDecision};
use crate::event::{Event, EventPayload};

/// The latest `budget_gate` that paused the run.
#[derive(Debug, Clone, PartialEq)]
pub struct GatePause {
    pub node: String,
    pub batch: Option<u32>,
    pub items: Option<u32>,
    pub basis: GateBasis,
    pub need: Option<BudgetAmount>,
    pub remaining: Option<BudgetAmount>,
    pub seq: u64,
}

/// The fold of a run's `budget_set` and `budget_gate` events.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetFold {
    pub limit: BudgetLimit,
    pub on_no_estimate: OnNoEstimate,
    pub source: Option<BudgetSource>,
    /// `budget_set` events so far.
    pub sets: u32,
    /// The latest gate pause.
    pub last_pause: Option<GatePause>,
    /// The latest gate pause still holds the run (no resume, stop or end
    /// came after it).
    pub pause_open: bool,
    /// A person set the budget or waived the node after the latest gate
    /// pause, so a plain resume may continue it.
    pub pause_resolved: bool,
    /// The node a person let run once over the budget, until it does.
    pub waiver: Option<String>,
}

impl BudgetFold {
    /// The fold, `None` when the run has no budget.
    pub fn of(events: &[Event]) -> Option<Self> {
        let mut fold: Option<BudgetFold> = None;
        for e in events {
            match &e.payload {
                EventPayload::BudgetSet {
                    tokens,
                    usd,
                    on_no_estimate,
                    source,
                    ..
                } => {
                    let f = fold.get_or_insert_with(|| BudgetFold {
                        limit: BudgetLimit::default(),
                        on_no_estimate: *on_no_estimate,
                        source: *source,
                        sets: 0,
                        last_pause: None,
                        pause_open: false,
                        pause_resolved: false,
                        waiver: None,
                    });
                    f.limit = BudgetLimit {
                        tokens: *tokens,
                        usd: *usd,
                    };
                    f.on_no_estimate = *on_no_estimate;
                    f.source = *source;
                    f.sets += 1;
                    f.pause_resolved = true;
                }
                EventPayload::BudgetGate {
                    node,
                    batch,
                    items,
                    decision,
                    basis,
                    need,
                    remaining,
                    ..
                } => {
                    let Some(f) = fold.as_mut() else { continue };
                    match decision {
                        GateDecision::Paused => {
                            f.last_pause = Some(GatePause {
                                node: node.clone(),
                                batch: *batch,
                                items: *items,
                                basis: *basis,
                                need: *need,
                                remaining: *remaining,
                                seq: e.seq,
                            });
                            f.pause_open = true;
                            f.pause_resolved = false;
                            // A waiver covers only the pause it resolved.
                            f.waiver = None;
                        }
                        GateDecision::Waived => {
                            f.waiver = Some(node.clone());
                            f.pause_resolved = true;
                        }
                        GateDecision::AdmittedWaived if f.waiver.as_ref() == Some(node) => {
                            f.waiver = None;
                        }
                        _ => {}
                    }
                }
                EventPayload::RunResumed {
                    from_node,
                    explicit,
                    ..
                } => {
                    if let Some(f) = fold.as_mut() {
                        f.pause_open = false;
                        // A resume from another node leaves the waived one
                        // behind: the waiver does not travel with the run.
                        if *explicit && f.waiver.as_ref().is_some_and(|w| w != from_node) {
                            f.waiver = None;
                        }
                    }
                }
                EventPayload::RunAborted { .. } | EventPayload::RunFinished { .. } => {
                    if let Some(f) = fold.as_mut() {
                        f.pause_open = false;
                    }
                }
                _ => {}
            }
        }
        fold.filter(|f| f.limit.tokens.is_some() || f.limit.usd.is_some())
    }

    /// The open gate pause a plain resume may not continue.
    pub fn unresolved_pause(&self) -> Option<&GatePause> {
        self.last_pause
            .as_ref()
            .filter(|_| self.pause_open && !self.pause_resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(lines: &[serde_json::Value]) -> Vec<Event> {
        lines
            .iter()
            .enumerate()
            .map(|(i, v)| Event {
                seq: i as u64,
                ts: i as u128,
                payload: serde_json::from_value(v.clone()).unwrap(),
            })
            .collect()
    }

    fn gate(node: &str, decision: &str) -> serde_json::Value {
        serde_json::json!({"type": "budget_gate", "node": node, "decision": decision, "basis": "median"})
    }

    fn resumed(from: &str, explicit: bool) -> serde_json::Value {
        serde_json::json!({"type": "run_resumed", "from_node": from, "explicit": explicit})
    }

    #[test]
    fn a_waiver_covers_only_the_pause_it_resolved() {
        let set =
            serde_json::json!({"type": "budget_set", "tokens": 100, "on_no_estimate": "pause"});
        let waived = [set.clone(), gate("b", "paused"), gate("b", "waived")];
        let fold = |extra: &[serde_json::Value]| {
            let mut all = waived.to_vec();
            all.extend_from_slice(extra);
            BudgetFold::of(&events(&all)).unwrap().waiver
        };
        assert_eq!(fold(&[resumed("b", false)]), Some("b".into()));
        // The run took another path and paused before another node.
        assert_eq!(fold(&[resumed("b", false), gate("c", "paused")]), None);
        // A resume from another node leaves the waived one behind.
        assert_eq!(fold(&[resumed("x", true)]), None);
        assert_eq!(fold(&[resumed("b", true)]), Some("b".into()));
    }
}
