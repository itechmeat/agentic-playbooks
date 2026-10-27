//! What a run spent, per budget dimension, derived from its journal: its
//! own attempts' reported usage plus its sub-playbook runs'. An attempt that
//! reported nothing in a dimension is unknown, never zero: it is estimated
//! with its node's median per attempt from the run's estimate, and when any
//! such attempt has no estimate the spent amount is unknown.

use std::path::Path;

use apb_core::budget::BudgetLimit;

use super::kind::{BudgetAmount, BudgetSpent, SpentDim};
use crate::event::{Event, EventPayload};
use crate::run_estimate::RunEstimate;
use crate::run_estimate::sample::{AttemptSpend, attempt_spend, child_attempts};

/// What the run spent in each dimension.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Spent {
    pub tokens: SpentDim,
    pub usd: SpentDim,
}

/// What is left in one dimension.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Remaining {
    /// The dimension has no limit.
    Unlimited,
    /// What was spent is unknown. The known part of it is a lower bound,
    /// so what is left is at most `at_most` (the limit minus the known
    /// spend and the reservations).
    Unknown {
        at_most: f64,
    },
    Known(f64),
}

fn add(dim: &mut SpentDim, value: Option<f64>, imputed: Option<f64>) {
    match value {
        Some(v) => dim.known += v,
        None => {
            dim.unknown_attempts += 1;
            dim.imputed = match (dim.imputed, imputed) {
                (Some(a), Some(b)) => Some(a + b),
                _ => None,
            };
        }
    }
}

impl Spent {
    /// The spent amounts of the run at `run_dir` over `events`, estimating
    /// unknown attempts with `est`.
    pub fn of(run_dir: &Path, events: &[Event], est: Option<&RunEstimate>) -> Spent {
        let mut s = Spent {
            tokens: SpentDim {
                imputed: Some(0.0),
                ..SpentDim::default()
            },
            usd: SpentDim {
                imputed: Some(0.0),
                ..SpentDim::default()
            },
        };
        let runs_dir = run_dir.parent().unwrap_or(run_dir);
        for e in events {
            let mut count = |node: &str, a: AttemptSpend| {
                let median = est.and_then(|x| x.node(node)).map(|n| n.per_attempt_p50);
                add(
                    &mut s.tokens,
                    a.tokens.map(|t| t as f64),
                    median.and_then(|m| m.tokens).map(|t| t as f64),
                );
                add(&mut s.usd, a.cost, median.and_then(|m| m.cost_usd));
            };
            if let Some((node, a)) = attempt_spend(&e.payload) {
                count(node, a);
            } else if let EventPayload::ChildRunStarted { node_id, run_id } = &e.payload {
                for a in child_attempts(runs_dir, run_id) {
                    count(node_id, a);
                }
            }
        }
        s
    }

    /// The spent amounts of the limited dimensions, as journaled.
    pub fn for_limit(&self, limit: &BudgetLimit) -> BudgetSpent {
        BudgetSpent {
            tokens: limit.tokens.map(|_| self.tokens),
            usd: limit.usd.map(|_| self.usd),
        }
    }
}

/// What is left of `limit` after `spent` and the amounts `reserved` for
/// nodes already admitted and still running: tokens, then dollars.
pub fn remaining(
    limit: &BudgetLimit,
    spent: &Spent,
    reserved: &BudgetAmount,
) -> (Remaining, Remaining) {
    let one = |limit: Option<f64>, dim: &SpentDim, reserved: Option<f64>| match limit {
        None => Remaining::Unlimited,
        Some(l) => {
            let left = l - reserved.unwrap_or(0.0);
            match dim.total() {
                None => Remaining::Unknown {
                    at_most: left - dim.known,
                },
                Some(s) => Remaining::Known(left - s),
            }
        }
    };
    (
        one(
            limit.tokens.map(|t| t as f64),
            &spent.tokens,
            reserved.tokens,
        ),
        one(limit.usd, &spent.usd, reserved.usd),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_estimate::{AttemptMedian, NodeEstimate, NodeSpread};

    fn event(seq: u64, v: serde_json::Value) -> Event {
        Event {
            seq,
            ts: u128::from(seq),
            payload: serde_json::from_value(v).unwrap(),
        }
    }

    fn attempt(node: &str, usage: Option<(u64, f64)>) -> serde_json::Value {
        let mut v = serde_json::json!({"type": "attempt_finished", "node": node, "attempt": 1, "status": "succeeded"});
        if let Some((t, c)) = usage {
            v["usage"] = serde_json::json!({"input_tokens": t, "output_tokens": 0, "cost_usd": c, "source": "reported"});
        }
        v
    }

    fn estimate(medians: &[(&str, Option<u64>)]) -> RunEstimate {
        RunEstimate {
            playbook: "p".into(),
            version: "1.0.0".into(),
            execution: "cli".into(),
            comparable: Default::default(),
            time: crate::run_estimate::TimeEstimate {
                basis: crate::run_estimate::EstimateBasis::None,
                spread: None,
                human_wait_p50_ms: None,
            },
            tokens: None,
            cost_usd: None,
            nodes: medians
                .iter()
                .map(|(n, t)| NodeEstimate {
                    node: (*n).into(),
                    visits_p50: 1.0,
                    per_visit: NodeSpread::default(),
                    per_run: NodeSpread::default(),
                    per_attempt_p50: AttemptMedian {
                        tokens: *t,
                        cost_usd: None,
                    },
                    list: None,
                })
                .collect(),
            unestimated: Vec::new(),
            unestimated_tokens: Vec::new(),
            unestimated_cost: Vec::new(),
            note: None,
        }
    }

    #[test]
    fn unknown_attempts_are_imputed_with_the_attempt_median_never_zero() {
        let events = vec![
            event(1, attempt("a", Some((100, 0.5)))),
            event(2, attempt("a", None)),
            event(3, attempt("b", None)),
        ];
        let dir = tempfile::tempdir().unwrap();
        let est = estimate(&[("a", Some(40)), ("b", Some(7))]);
        let s = Spent::of(dir.path(), &events, Some(&est));
        assert_eq!(s.tokens.known, 100.0);
        assert_eq!(s.tokens.unknown_attempts, 2);
        assert_eq!(s.tokens.total(), Some(147.0));
        // No cost median: the dollars spent are unknown, not 0.5.
        assert_eq!(s.usd.known, 0.5);
        assert_eq!(s.usd.total(), None);
        // A node without a median leaves the tokens unknown too.
        let est = estimate(&[("a", Some(40))]);
        assert_eq!(
            Spent::of(dir.path(), &events, Some(&est)).tokens.total(),
            None
        );
        assert_eq!(Spent::of(dir.path(), &events, None).tokens.total(), None);
        let limit = BudgetLimit {
            tokens: Some(1000),
            usd: None,
        };
        let s = Spent::of(
            dir.path(),
            &events,
            Some(&estimate(&[("a", Some(40)), ("b", Some(7))])),
        );
        let (t, u) = remaining(
            &limit,
            &s,
            &BudgetAmount {
                tokens: Some(3.0),
                usd: None,
            },
        );
        assert_eq!(t, Remaining::Known(850.0));
        assert_eq!(u, Remaining::Unlimited);
    }

    #[test]
    fn known_spend_is_a_lower_bound_for_the_gate() {
        use crate::budget::fold::BudgetFold;
        use crate::budget::gate::{GateInput, Need, Verdict, decide};
        use crate::budget::kind::GateBasis;
        use apb_core::budget::OnNoEstimate;
        let mut interrupted = attempt("a", None);
        interrupted["status"] = "interrupted".into();
        let events = vec![
            event(1, interrupted),
            event(2, attempt("b", Some((50_000, 0.0)))),
        ];
        let dir = tempfile::tempdir().unwrap();
        let fold = |tokens: u64, on: OnNoEstimate| BudgetFold {
            limit: BudgetLimit {
                tokens: Some(tokens),
                usd: None,
            },
            on_no_estimate: on,
            source: None,
            sets: 1,
            last_pause: None,
            pause_open: false,
            pause_resolved: false,
            waiver: None,
        };
        let gate = |f: &BudgetFold, s: &Spent, need: Need| {
            decide(&GateInput {
                fold: f,
                spent: s,
                need,
                reserved: BudgetAmount::default(),
                waived: false,
                executor_drift: false,
            })
        };
        // `a` has no estimate: what was spent is unknown, but its known
        // 50000 alone are over the 10000 limit, so even `continue` pauses.
        let unknown = Spent::of(dir.path(), &events, None);
        assert_eq!(unknown.tokens.total(), None);
        let v = gate(
            &fold(10_000, OnNoEstimate::Continue),
            &unknown,
            Need::Unknown,
        );
        assert!(
            matches!(
                v,
                Verdict::Pause {
                    basis: GateBasis::Exhausted,
                    ..
                }
            ),
            "{v:?}"
        );
        // Under `pause`, with a median for `a`, the spend is known: the
        // limit pauses the run and a raise lets the next node run.
        let est = estimate(&[("a", Some(400)), ("c", Some(300))]);
        let known = Spent::of(dir.path(), &events, Some(&est));
        assert_eq!(known.tokens.total(), Some(50_400.0));
        let need = Need::Known(BudgetAmount {
            tokens: Some(300.0),
            usd: None,
        });
        let v = gate(&fold(10_000, OnNoEstimate::Pause), &known, need);
        assert!(matches!(v, Verdict::Pause { .. }), "{v:?}");
        assert_eq!(
            gate(&fold(110_000, OnNoEstimate::Pause), &known, need),
            Verdict::Admit
        );
    }

    #[test]
    fn child_runs_count() {
        let dir = tempfile::tempdir().unwrap();
        let (parent, child) = (dir.path().join("parent"), dir.path().join("child"));
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::create_dir_all(&child).unwrap();
        let line = |v: serde_json::Value, seq: u64| {
            let mut v = v;
            v["seq"] = seq.into();
            v["ts"] = seq.into();
            v.to_string() + "\n"
        };
        std::fs::write(
            child.join("events.jsonl"),
            line(
                serde_json::json!({"type": "run_started", "playbook": "c", "version": "1.0.0"}),
                0,
            ) + &line(attempt("inner", Some((70, 0.25))), 1),
        )
        .unwrap();
        let events = vec![
            event(1, attempt("a", Some((30, 0.25)))),
            event(
                2,
                serde_json::json!({"type": "child_run_started", "node_id": "sub", "run_id": "child"}),
            ),
        ];
        let s = Spent::of(&parent, &events, None);
        assert_eq!(s.tokens.total(), Some(100.0));
        assert_eq!(s.usd.total(), Some(0.5));
    }
}
