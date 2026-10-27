//! The budget gate's decision before a gated node or a list batch: a pure
//! function of the budget in force, what was spent, what is reserved for
//! nodes still running, and the node's median spend.
//!
//! Per limited dimension, any pause wins:
//! 1. a person's one-time waiver admits the node;
//! 2. nothing left (spent at or over the limit) pauses, and so does less
//!    left than the node's median; what was spent counts its known part as
//!    a lower bound, so this holds even when the rest of it is unknown;
//! 3. no estimate for the node (no comparable sample, or the node's
//!    executor drifted in this run), or an unknown spent amount, leaves it
//!    to `on_no_estimate`: `continue` runs the node, `pause` and an
//!    unresolved `ask` pause;
//! 4. otherwise the node runs.

use super::fold::BudgetFold;
use super::kind::{BudgetAmount, GateBasis};
use super::spent::{Remaining, Spent, remaining};
use apb_core::budget::OnNoEstimate;

/// The node's median spend.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Need {
    Known(BudgetAmount),
    /// No comparable sample in any dimension.
    Unknown,
}

/// Everything the gate weighs.
#[derive(Debug, Clone, Copy)]
pub struct GateInput<'a> {
    pub fold: &'a BudgetFold,
    pub spent: &'a Spent,
    pub need: Need,
    /// The medians of the admitted nodes still running.
    pub reserved: BudgetAmount,
    /// A person let this node run once over the budget.
    pub waived: bool,
    /// The node fell back to another executor in this run: its history does
    /// not estimate it.
    pub executor_drift: bool,
}

/// The gate's decision.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    Admit,
    /// Admitted on a person's one-time waiver.
    AdmitWaived,
    /// Admitted although the gate could not decide (`on_no_estimate:
    /// continue`).
    Continue(GateBasis),
    Pause {
        basis: GateBasis,
        need: Option<BudgetAmount>,
        /// What was left per limited dimension, where known.
        remaining: BudgetAmount,
    },
}

enum Dim {
    Fits,
    /// Less is left than the node needs, or nothing at all.
    Over(GateBasis),
    NoEstimate,
    UnknownSpend,
}

/// One limited dimension. What was spent is a lower bound when part of it
/// is unknown, so an amount that is already over (or short of the need)
/// on its known part pauses whatever the policy; the unknown part only
/// matters while the known part still fits.
fn dim(rem: Remaining, need: Option<f64>) -> Option<Dim> {
    let (left, exact) = match rem {
        Remaining::Unlimited => return None,
        Remaining::Known(r) => (r, true),
        Remaining::Unknown { at_most } => (at_most, false),
    };
    Some(match need {
        _ if left <= 0.0 => Dim::Over(GateBasis::Exhausted),
        Some(n) if left < n => Dim::Over(GateBasis::Median),
        None => Dim::NoEstimate,
        Some(_) if !exact => Dim::UnknownSpend,
        Some(_) => Dim::Fits,
    })
}

/// What is left in one dimension, as a pause reports it: the exact amount,
/// or the upper bound of an unknown one when that bound decided the pause.
fn reported(r: Remaining, over: bool) -> Option<f64> {
    match r {
        Remaining::Known(x) => Some(x),
        Remaining::Unknown { at_most } if over => Some(at_most),
        _ => None,
    }
}

/// The decision for `input`.
pub fn decide(input: &GateInput<'_>) -> Verdict {
    if input.waived {
        return Verdict::AdmitWaived;
    }
    let need = match input.need {
        Need::Known(n) if !input.executor_drift => Some(n),
        _ => None,
    };
    let (rt, ru) = remaining(&input.fold.limit, input.spent, &input.reserved);
    let (dt, du) = (
        dim(rt, need.and_then(|n| n.tokens)),
        dim(ru, need.and_then(|n| n.usd)),
    );
    let over = |d: &Option<Dim>| match d {
        Some(Dim::Over(b)) => Some(*b),
        _ => None,
    };
    let pause = |basis| Verdict::Pause {
        basis,
        need,
        remaining: BudgetAmount {
            tokens: reported(rt, over(&dt).is_some()),
            usd: reported(ru, over(&du).is_some()),
        },
    };
    // A short median wins over an exhausted dimension: it names the need.
    let overs: Vec<GateBasis> = [over(&dt), over(&du)].into_iter().flatten().collect();
    if let Some(b) = overs
        .iter()
        .find(|b| **b == GateBasis::Median)
        .or(overs.first())
    {
        return pause(*b);
    }
    let dims: Vec<&Dim> = [&dt, &du].into_iter().flatten().collect();
    let basis = if dims.iter().any(|d| matches!(d, Dim::NoEstimate)) {
        GateBasis::NoEstimate
    } else if dims.iter().any(|d| matches!(d, Dim::UnknownSpend)) {
        GateBasis::UnknownSpend
    } else {
        return Verdict::Admit;
    };
    match input.fold.on_no_estimate {
        OnNoEstimate::Continue => Verdict::Continue(basis),
        OnNoEstimate::Pause | OnNoEstimate::Ask => pause(basis),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::kind::SpentDim;
    use apb_core::budget::BudgetLimit;

    fn fold(tokens: Option<u64>, usd: Option<f64>, policy: OnNoEstimate) -> BudgetFold {
        BudgetFold {
            limit: BudgetLimit { tokens, usd },
            on_no_estimate: policy,
            source: None,
            sets: 1,
            last_pause: None,
            pause_open: false,
            pause_resolved: false,
            waiver: None,
        }
    }

    fn spent(tokens: Option<f64>, usd: Option<f64>) -> Spent {
        let d = |v: Option<f64>| SpentDim {
            known: v.unwrap_or(0.0),
            unknown_attempts: u32::from(v.is_none()),
            imputed: v.map(|_| 0.0),
        };
        Spent {
            tokens: d(tokens),
            usd: d(usd),
        }
    }

    /// `known` tokens reported, and one attempt with no usage and no
    /// estimate.
    fn partly(known: f64) -> Spent {
        Spent {
            tokens: SpentDim {
                known,
                unknown_attempts: 1,
                imputed: None,
            },
            usd: SpentDim::default(),
        }
    }

    fn tokens(t: f64) -> Need {
        Need::Known(BudgetAmount {
            tokens: Some(t),
            usd: None,
        })
    }

    struct Case {
        name: &'static str,
        fold: BudgetFold,
        spent: Spent,
        need: Need,
        reserved: f64,
        waived: bool,
        drift: bool,
        want: fn(&Verdict) -> bool,
    }

    fn case(name: &'static str, fold: BudgetFold, need: Need, want: fn(&Verdict) -> bool) -> Case {
        Case {
            name,
            fold,
            spent: spent(Some(600.0), Some(0.0)),
            need,
            reserved: 0.0,
            waived: false,
            drift: false,
            want,
        }
    }

    #[test]
    fn decide_table() {
        use OnNoEstimate::*;
        let t = |p| fold(Some(1000), None, p);
        let cases = vec![
            case(
                "remaining covers the median",
                t(Pause),
                tokens(300.0),
                |v| *v == Verdict::Admit,
            ),
            case(
                "remaining less than need pauses",
                t(Continue),
                tokens(500.0),
                |v| matches!(v, Verdict::Pause { basis: GateBasis::Median, remaining, .. } if remaining.tokens == Some(400.0)),
            ),
            Case {
                reserved: 200.0,
                ..case(
                    "reserved spend is subtracted",
                    t(Continue),
                    tokens(300.0),
                    |v| {
                        matches!(
                            v,
                            Verdict::Pause {
                                basis: GateBasis::Median,
                                ..
                            }
                        )
                    },
                )
            },
            case(
                "unknown need under continue",
                t(Continue),
                Need::Unknown,
                |v| *v == Verdict::Continue(GateBasis::NoEstimate),
            ),
            case("unknown need under pause", t(Pause), Need::Unknown, |v| {
                matches!(
                    v,
                    Verdict::Pause {
                        basis: GateBasis::NoEstimate,
                        ..
                    }
                )
            }),
            case(
                "unknown need under an unresolved ask",
                t(Ask),
                Need::Unknown,
                |v| {
                    matches!(
                        v,
                        Verdict::Pause {
                            basis: GateBasis::NoEstimate,
                            ..
                        }
                    )
                },
            ),
            Case {
                spent: spent(None, Some(0.0)),
                ..case(
                    "unknown spend with no imputation",
                    t(Pause),
                    tokens(10.0),
                    |v| matches!(v, Verdict::Pause { basis: GateBasis::UnknownSpend, remaining, .. } if remaining.tokens.is_none()),
                )
            },
            case(
                "a usd dimension with no cost history",
                fold(None, Some(5.0), Continue),
                tokens(10.0),
                |v| *v == Verdict::Continue(GateBasis::NoEstimate),
            ),
            Case {
                drift: true,
                ..case(
                    "executor drift is unestimated",
                    t(Pause),
                    tokens(1.0),
                    |v| {
                        matches!(
                            v,
                            Verdict::Pause {
                                basis: GateBasis::NoEstimate,
                                ..
                            }
                        )
                    },
                )
            },
            Case {
                waived: true,
                ..case("a waiver admits once", t(Pause), tokens(5000.0), |v| {
                    *v == Verdict::AdmitWaived
                })
            },
            Case {
                spent: spent(Some(1000.0), Some(0.0)),
                ..case(
                    "nothing left pauses even without an estimate",
                    t(Continue),
                    Need::Unknown,
                    |v| {
                        matches!(
                            v,
                            Verdict::Pause {
                                basis: GateBasis::Exhausted,
                                ..
                            }
                        )
                    },
                )
            },
            Case {
                spent: partly(1200.0),
                ..case(
                    "a known part over the limit pauses although the rest is unknown",
                    t(Continue),
                    Need::Unknown,
                    |v| matches!(v, Verdict::Pause { basis: GateBasis::Exhausted, remaining, .. } if remaining.tokens == Some(-200.0)),
                )
            },
            Case {
                spent: partly(600.0),
                ..case(
                    "a known part leaving less than the need pauses although the rest is unknown",
                    t(Continue),
                    tokens(500.0),
                    |v| {
                        matches!(
                            v,
                            Verdict::Pause {
                                basis: GateBasis::Median,
                                ..
                            }
                        )
                    },
                )
            },
            Case {
                spent: partly(600.0),
                ..case(
                    "an unknown part matters only while the known part fits",
                    t(Continue),
                    tokens(300.0),
                    |v| *v == Verdict::Continue(GateBasis::UnknownSpend),
                )
            },
            case(
                "an over dimension wins over a continue",
                fold(Some(1000), Some(5.0), Continue),
                tokens(900.0),
                |v| {
                    matches!(
                        v,
                        Verdict::Pause {
                            basis: GateBasis::Median,
                            ..
                        }
                    )
                },
            ),
        ];
        for c in cases {
            let v = decide(&GateInput {
                fold: &c.fold,
                spent: &c.spent,
                need: c.need,
                reserved: BudgetAmount {
                    tokens: Some(c.reserved),
                    usd: None,
                },
                waived: c.waived,
                executor_drift: c.drift,
            });
            assert!((c.want)(&v), "case `{}` decided {v:?}", c.name);
        }
    }
}
