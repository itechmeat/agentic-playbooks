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

/// The launch policy a start would meet, for an agent: the effective
/// policy over the tree with its sources, and the playbook's own launch
/// floor.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LaunchView {
    pub policy: apb_core::launch::LaunchPolicy,
    pub sources: Vec<String>,
    /// The strictest declaration any saved version had, kept outside the
    /// definition; absent when no version declared one.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub floor: Option<apb_core::launch::LaunchPolicy>,
    /// Why the policy could not be read (a damaged settings or floor file,
    /// an unresolved tree); an agent's launch is refused then.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub problem: Option<String>,
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
    /// What a start needs first, from the items above
    /// ([`crate::next_action::start::from_fit`]); the consent itself comes
    /// with the gate's refusal, which carries its nonce.
    pub start_actions: Vec<crate::next_action::start_action::StartAction>,
    /// The launch policy for an agent (see [`LaunchView`]).
    pub launch: LaunchView,
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
