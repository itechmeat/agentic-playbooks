//! The two kinds of claim a run outcome makes: [`Evidence`] the engine
//! checked, and a [`Gap`] nobody checked, each with what produced it. Shared
//! by the outcome summary and the sections that feed it (lists, baseline).

use apb_core::baseline::Verdict;
use serde::Serialize;

/// What checked a piece of evidence.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    Goal,
    SuccessCheck,
    Baseline,
    /// The check a list node runs on each item.
    ItemCheck,
}

impl EvidenceSource {
    /// Every source, for the dashboard's label table (`consts.gen.ts`).
    pub const ALL: [EvidenceSource; 4] = [
        EvidenceSource::Goal,
        EvidenceSource::SuccessCheck,
        EvidenceSource::Baseline,
        EvidenceSource::ItemCheck,
    ];

    /// The source in words.
    pub fn label(self) -> &'static str {
        match self {
            EvidenceSource::Goal => "goal",
            EvidenceSource::SuccessCheck => "success check",
            EvidenceSource::Baseline => "baseline",
            EvidenceSource::ItemCheck => "item check",
        }
    }
}

/// One thing the engine checked, and what it found.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Evidence {
    pub source: EvidenceSource,
    pub subject: String,
    /// `passed`, `failed`, `error`, `met` or `not_met`.
    pub result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub detail: Option<String>,
}

impl Evidence {
    /// Whether the check came out in the run's favour.
    pub fn passed(&self) -> bool {
        self.result == "passed" || self.result == Verdict::Met.as_str()
    }
}

/// Why something is not verified.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    /// A goal criterion a person confirms.
    ManualCriterion,
    /// An agent step that succeeded on its own report only.
    SelfReportedNode,
    /// The run ended before its goal criteria were checked.
    GoalNotChecked,
    /// A connector write whose effect is not known.
    UnknownCall,
    /// A connector response that was cut short.
    IncompleteResponse,
    /// Unread inbox events lost to retention.
    InboxGap,
    /// An attempt's output was cut.
    OutputTruncated,
    /// A baseline observation that could not be compared.
    BaselineUnknown,
    /// A list item its check, its step or its send left for a person.
    ItemNeedsLook,
    /// A list whose items passed with no item check.
    SelfReportedItems,
}

impl GapKind {
    /// Every kind, for the dashboard's label table (`consts.gen.ts`).
    pub const ALL: [GapKind; 10] = [
        GapKind::ManualCriterion,
        GapKind::SelfReportedNode,
        GapKind::GoalNotChecked,
        GapKind::UnknownCall,
        GapKind::IncompleteResponse,
        GapKind::InboxGap,
        GapKind::OutputTruncated,
        GapKind::BaselineUnknown,
        GapKind::ItemNeedsLook,
        GapKind::SelfReportedItems,
    ];

    /// Why something is not verified, in words.
    pub fn label(self) -> &'static str {
        match self {
            GapKind::ManualCriterion => "to confirm by hand",
            GapKind::SelfReportedNode => "self-reported",
            GapKind::GoalNotChecked => "goal not checked",
            GapKind::UnknownCall => "call outcome unknown",
            GapKind::IncompleteResponse => "response cut short",
            GapKind::InboxGap => "inbox gap",
            GapKind::OutputTruncated => "output cut",
            GapKind::BaselineUnknown => "baseline unknown",
            GapKind::ItemNeedsLook => "item needs a look",
            GapKind::SelfReportedItems => "items self-reported",
        }
    }
}

/// One thing nobody checked.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Gap {
    pub kind: GapKind,
    pub subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub detail: Option<String>,
}
