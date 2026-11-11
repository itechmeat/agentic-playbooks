//! What every surface shows of a playbook's routine health (D2): the
//! view types, a leaf the run statistics read without the evaluation.

use serde::Serialize;

/// One playbook's health as every surface shows it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HealthView {
    /// The playbook id.
    pub playbook: String,
    /// `project` or `global`.
    pub scope: String,
    /// `held` when a hold is set (or the file does not verify), else
    /// `unwatched` when the playbook is not watched, else `tripped` when
    /// any rule is tripped, else `ok`.
    pub status: String,
    /// One entry per rule the block sets, in the order
    /// `consecutive_failures`, `failure_rate`, `overdue`; empty for an
    /// unwatched playbook and a file that does not verify.
    pub rules: Vec<HealthRuleView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub hold: Option<HoldView>,
    /// ISO UTC; absent when not muted.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub muted_until: Option<String>,
    /// The newest notice: `<rule> <kind> <state> <ISO UTC>`; absent when
    /// the ledger is empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub last_notice: Option<String>,
}

/// One rule of a [`HealthView`].
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthRuleView {
    /// `consecutive_failures`, `failure_rate` or `overdue`.
    pub rule: String,
    pub tripped: bool,
    /// ISO UTC of the trip; absent while not tripped.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub since: Option<String>,
    /// The verdict's detail; empty for a rule never evaluated.
    pub detail: String,
}

/// The hold of a [`HealthView`].
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HoldView {
    /// ISO UTC of the hold's start.
    pub since: String,
    /// The rule that set it; `unreadable` for a file that does not verify.
    pub rule: String,
    pub detail: String,
    /// The command that clears it: `apb health clear <ref>`.
    pub clear: String,
}
