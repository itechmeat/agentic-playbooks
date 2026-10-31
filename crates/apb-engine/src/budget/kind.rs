//! The budget's journal-facing types. A leaf module: the journal
//! (`event.rs`) names these types, so they depend on nothing else in the
//! crate.

use serde::{Deserialize, Serialize};

/// An amount per budget dimension. `None` leaves a dimension out.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetAmount {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub usd: Option<f64>,
}

/// A token amount as text: a whole number, never negative.
pub fn tokens_text(x: f64) -> String {
    format!("{}", x.max(0.0).round() as u64)
}

/// A dollar amount as text: `$1.25`, never negative.
pub fn usd_text(x: f64) -> String {
    format!("${:.2}", x.max(0.0))
}

impl BudgetAmount {
    /// `30000 tokens, $1.25`, or `unknown` when empty.
    pub fn text(&self) -> String {
        let parts: Vec<String> = [
            self.tokens.map(|t| format!("{} tokens", tokens_text(t))),
            self.usd.map(usd_text),
        ]
        .into_iter()
        .flatten()
        .collect();
        if parts.is_empty() {
            "unknown".into()
        } else {
            parts.join(", ")
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_none() && self.usd.is_none()
    }

    /// The sum per dimension; a dimension either side leaves out stays the
    /// other side's.
    pub fn plus(&self, other: &BudgetAmount) -> BudgetAmount {
        let add = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
        };
        BudgetAmount {
            tokens: add(self.tokens, other.tokens),
            usd: add(self.usd, other.usd),
        }
    }
}

/// What the run spent in one dimension: the amount the attempts reported,
/// how many attempts reported none, and the estimate for those.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SpentDim {
    /// The sum the attempts (and the sub-playbook runs) reported.
    #[serde(default)]
    pub known: f64,
    /// Attempts that reported nothing in this dimension: unknown, never
    /// zero.
    #[serde(default)]
    pub unknown_attempts: u32,
    /// The estimate for the unknown attempts (their node's median per
    /// attempt), `None` when any of them has no estimate: the spent amount
    /// is then unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub imputed: Option<f64>,
}

impl SpentDim {
    /// The best figure for the spent amount: the known sum plus the
    /// estimate for the unknown attempts; `None` when that is unknown.
    pub fn total(&self) -> Option<f64> {
        self.imputed.map(|i| self.known + i)
    }
}

/// What the run spent in each dimension, as a gate decision saw it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetSpent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub tokens: Option<SpentDim>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub usd: Option<SpentDim>,
}

/// What the gate decided before a node (or a list batch). A plain
/// admission journals nothing.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateDecision {
    /// The run pauses before the node.
    Paused,
    /// The node runs although the gate could not decide
    /// (`on_no_estimate: continue`).
    Continued,
    /// A person allowed the paused node to run once over the budget.
    Waived,
    /// The node ran on that one-time allowance.
    AdmittedWaived,
    /// A decision a newer apb wrote.
    Unknown,
}

/// Why the gate decided as it did.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateBasis {
    /// The node's median spend is more than what is left.
    Median,
    /// Nothing is left: the run spent its limit (its known spend alone),
    /// whatever the node needs.
    Exhausted,
    /// No comparable history estimates the node.
    NoEstimate,
    /// What the run spent is unknown (attempts without usage and no
    /// estimate for them).
    UnknownSpend,
    /// A person decided.
    Operator,
    /// A basis a newer apb wrote.
    Unknown,
}

impl GateBasis {
    pub fn as_str(self) -> &'static str {
        match self {
            GateBasis::Median => "median",
            GateBasis::Exhausted => "exhausted",
            GateBasis::NoEstimate => "no_estimate",
            GateBasis::UnknownSpend => "unknown_spend",
            GateBasis::Operator => "operator",
            GateBasis::Unknown => "unknown",
        }
    }
}

impl GateDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            GateDecision::Paused => "paused",
            GateDecision::Continued => "continued",
            GateDecision::Waived => "waived",
            GateDecision::AdmittedWaived => "admitted_waived",
            GateDecision::Unknown => "unknown",
        }
    }
}

/// Decodes a unit enum from its `as_str` name; a name this binary does not
/// know (a newer apb's) reads as the enum's `Unknown`.
macro_rules! lenient_names {
    ($t:ty, [$($v:ident),*]) => {
        impl<'de> Deserialize<'de> for $t {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Ok([$(<$t>::$v),*]
                    .into_iter()
                    .find(|v| v.as_str() == s)
                    .unwrap_or(<$t>::Unknown))
            }
        }
    };
}

lenient_names!(GateDecision, [Paused, Continued, Waived, AdmittedWaived]);
lenient_names!(
    GateBasis,
    [Median, Exhausted, NoEstimate, UnknownSpend, Operator]
);
