//! The shapes of a fit report.

use serde::Serialize;

/// How much an item matters for running here.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FitSeverity {
    /// The run cannot work until it is resolved.
    Blocking,
    /// Worth knowing; the run can still work.
    Note,
}

/// One compatibility or setup item.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FitItem {
    pub code: String,
    /// What it is about: a profile, a param, a connector, a node.
    pub subject: Option<String>,
    pub detail: String,
    pub severity: FitSeverity,
}

/// One part of the tree and whether it is approved.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustItem {
    /// `playbook`, `imported_scripts`, `profile_bundle`, `connector`,
    /// `connector_account` or `sub_playbook`.
    pub kind: String,
    pub subject: String,
    pub digest: Option<String>,
    pub approved: bool,
    /// Whether an acknowledge at start stands in for the approval.
    pub acknowledge_covers: bool,
}

/// What a start must be agreed to.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ConsentView {
    /// The consent sources (`playbook`, `node <id>`, a connector function,
    /// `sub-playbook node <id>`); empty when no consent is needed.
    pub irreversible: Vec<String>,
    /// The effects of the whole tree.
    pub effects: Vec<String>,
}

/// The fit of one playbook in one project.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FitReport {
    pub id: String,
    /// `project` or `global`.
    pub scope: String,
    pub version: String,
    /// No blocking compatibility or setup item. Not the same as a start
    /// going through unasked: see `trusted` and `needs_consent`.
    pub ready: bool,
    /// Every trust item is approved.
    pub trusted: bool,
    pub needs_consent: bool,
    pub compatibility: Vec<FitItem>,
    pub setup: Vec<FitItem>,
    pub trust: Vec<TrustItem>,
    pub consent: ConsentView,
}

/// The short form of a report, for a list.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FitSummary {
    pub ready: bool,
    /// No blocking compatibility item.
    pub compatible: bool,
    /// How many setup items there are.
    pub setup: usize,
    pub trusted: bool,
    pub needs_consent: bool,
}

impl FitReport {
    pub fn summary(&self) -> FitSummary {
        FitSummary {
            ready: self.ready,
            compatible: !blocks(&self.compatibility),
            setup: self.setup.len(),
            trusted: self.trusted,
            needs_consent: self.needs_consent,
        }
    }
}

/// Whether any of `items` blocks a run.
pub(super) fn blocks(items: &[FitItem]) -> bool {
    items.iter().any(|i| i.severity == FitSeverity::Blocking)
}
