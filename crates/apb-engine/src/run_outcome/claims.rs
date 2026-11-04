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

/// What decided a piece of evidence: a command apb ran, something apb
/// read itself, the agent's own words, or a decision model's judgment.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceBasis {
    /// A command or script apb ran, decided by its exit or its JSON.
    Executed,
    /// A connector response or the run's own journal, read by apb.
    Read,
    /// Text the agent wrote: a marker, an output field. Self-reported.
    AgentText,
    /// A decision model's judgment.
    Model,
}

impl EvidenceBasis {
    /// Every basis, for the dashboard's label table (`consts.gen.ts`).
    pub const ALL: [EvidenceBasis; 4] = [
        EvidenceBasis::Executed,
        EvidenceBasis::Read,
        EvidenceBasis::AgentText,
        EvidenceBasis::Model,
    ];

    /// The basis in words.
    pub fn label(self) -> &'static str {
        match self {
            EvidenceBasis::Executed => "executed",
            EvidenceBasis::Read => "read by apb",
            EvidenceBasis::AgentText => "agent's own text",
            EvidenceBasis::Model => "model judgment",
        }
    }
}

/// Whether a check's result still describes the tree the run left.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessState {
    /// Nothing the check reads changed after it ran.
    Current,
    /// A file in the check's declared scope changed after it ran.
    Stale,
    /// Something may have changed after it ran, and nothing tells whether
    /// the check reads it.
    Unknown,
}

impl FreshnessState {
    /// Every state, for the dashboard's label table (`consts.gen.ts`).
    pub const ALL: [FreshnessState; 3] = [
        FreshnessState::Current,
        FreshnessState::Stale,
        FreshnessState::Unknown,
    ];

    /// The state in words.
    pub fn label(self) -> &'static str {
        match self {
            FreshnessState::Current => "current",
            FreshnessState::Stale => "stale",
            FreshnessState::Unknown => "not known to be current",
        }
    }
}

/// A check's freshness and why (see `run_outcome::freshness`).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Freshness {
    pub state: FreshnessState,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub reason: Option<String>,
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
    /// What decided it.
    pub basis: EvidenceBasis,
    /// Whether it still describes the tree the run left; absent for
    /// evidence nothing later in the run can change (a goal criterion, a
    /// baseline comparison, an agent's text) and for a check an older apb
    /// bound to nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub freshness: Option<Freshness>,
}

impl Evidence {
    /// Whether the check came out in the run's favour and still describes
    /// the tree the run left: a stale or unknown pass is not a pass.
    pub fn passed(&self) -> bool {
        self.result_passes() && self.is_current()
    }

    /// Whether the check itself came out in the run's favour, current or
    /// not: only such a result can be "not current".
    pub fn result_passes(&self) -> bool {
        self.result == "passed" || self.result == Verdict::Met.as_str()
    }

    /// Whether nothing says the result is out of date.
    pub fn is_current(&self) -> bool {
        self.freshness
            .as_ref()
            .is_none_or(|f| f.state == FreshnessState::Current)
    }

    /// The bracketed tags after a line: the basis, then a freshness that
    /// is not current with its reason.
    pub fn tags(&self) -> String {
        let mut out = format!(" [{}]", self.basis.label());
        if let Some(f) = self
            .freshness
            .as_ref()
            .filter(|f| f.state != FreshnessState::Current)
        {
            out.push_str(&format!(" [{}", f.state.label()));
            if let Some(r) = &f.reason {
                out.push_str(&format!(": {r}"));
            }
            out.push(']');
        }
        out
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
