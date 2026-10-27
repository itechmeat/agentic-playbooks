//! The run's budget as every surface reports it: the limits, what was
//! spent and is left per dimension, and the node a gate pause stopped
//! before.

use std::path::Path;

use apb_core::budget::{BudgetLimit, BudgetSource, OnNoEstimate};
use serde::Serialize;

use super::fold::BudgetFold;
use super::kind::{BudgetAmount, GateBasis, SpentDim};
use super::spent::Spent;
use crate::event::Event;

/// One limited dimension.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DimStatus {
    /// What the attempts reported.
    pub spent: f64,
    /// Attempts that reported nothing in this dimension.
    pub unknown_attempts: u32,
    /// The estimate for those attempts; absent when one has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub imputed: Option<f64>,
    /// The limit minus the spent and imputed amounts; absent when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub remaining: Option<f64>,
}

impl DimStatus {
    fn of(limit: f64, d: &SpentDim) -> Self {
        DimStatus {
            spent: d.known,
            unknown_attempts: d.unknown_attempts,
            imputed: d.imputed.filter(|_| d.unknown_attempts > 0),
            remaining: d.total().map(|t| limit - t),
        }
    }
}

/// The node a budget pause holds the run before.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PausedAt {
    pub node: String,
    /// The list batch, for a list node.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub batch: Option<u32>,
    pub basis: GateBasis,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub need: Option<BudgetAmount>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub remaining: Option<BudgetAmount>,
}

/// A run's budget.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BudgetStatus {
    pub limit: BudgetLimit,
    pub on_no_estimate: OnNoEstimate,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub source: Option<BudgetSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub tokens: Option<DimStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub usd: Option<DimStatus>,
    /// How often the budget was set (at start and at resumes).
    pub sets: u32,
    /// The gate pause holding the run, while it does.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub paused: Option<PausedAt>,
    /// A node a person let run once over the budget, until it does.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub waiver: Option<String>,
}

/// The budget of the run at `run_dir`, `None` when it has none.
pub fn status(run_dir: &Path, events: &[Event]) -> Option<BudgetStatus> {
    let fold = BudgetFold::of(events)?;
    let est = crate::run_estimate::snapshot::read(run_dir);
    let spent = Spent::of(run_dir, events, est.as_ref());
    Some(BudgetStatus {
        limit: fold.limit,
        on_no_estimate: fold.on_no_estimate,
        source: fold.source,
        tokens: fold
            .limit
            .tokens
            .map(|l| DimStatus::of(l as f64, &spent.tokens)),
        usd: fold.limit.usd.map(|l| DimStatus::of(l, &spent.usd)),
        sets: fold.sets,
        paused: fold
            .last_pause
            .as_ref()
            .filter(|_| fold.pause_open)
            .map(|p| PausedAt {
                node: p.node.clone(),
                batch: p.batch,
                basis: p.basis,
                need: p.need,
                remaining: p.remaining,
            }),
        waiver: fold.waiver.clone(),
    })
}

impl crate::run_view::RunView {
    /// The run's budget: limits, spent and left per dimension, and the node
    /// a budget pause holds it before. `None` without a budget.
    pub fn budget(&self, run_dir: &Path) -> Option<BudgetStatus> {
        status(run_dir, &self.events)
    }
}
