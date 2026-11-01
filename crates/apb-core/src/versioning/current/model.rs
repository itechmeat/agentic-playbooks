//! The values of a move of `current` and of its history: who moved it,
//! the final trial record of a promoted candidate, and the history lines.
//! A leaf: it depends on nothing else of the move.

use serde::{Deserialize, Serialize};

use crate::quality::QualityRecord;

/// The surface a person moved `current` from.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    Cli,
    Dashboard,
}

/// Who moved `current`, as the history keeps it: bounded values only (a
/// run id, a basis such as `successes:3`, `quality`, `person`), never text
/// taken from a run.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MovedBy {
    /// A person chose the version (`apb version use`, the dashboard).
    Person { surface: Surface },
    /// The candidate on trial was promoted: by its trial run (`run_id`),
    /// by a grade stored later, or by a person's decision on it.
    CandidatePromotion {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        run_id: Option<String>,
        basis: String,
    },
    /// An in-run patch the run proved, by the promotion policy.
    PatchPromotion { run_id: String },
    /// A new playbook, or a new version saved as current.
    Created,
    /// A schema migration moved the playbook onto its migrated version.
    Migration,
    /// The playbook was copied in from another scope.
    ScopeMove { from_scope: String },
}

impl std::fmt::Display for MovedBy {
    /// Who moved it, in a few plain words.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MovedBy::Person { surface } => {
                let at = match surface {
                    Surface::Cli => "the CLI",
                    Surface::Dashboard => "the dashboard",
                };
                write!(f, "a person on {at}")
            }
            MovedBy::CandidatePromotion { run_id, basis } => {
                write!(f, "candidate promotion ({basis}")?;
                match run_id {
                    Some(run) => write!(f, ", run {run})"),
                    None => write!(f, ")"),
                }
            }
            MovedBy::PatchPromotion { run_id } => write!(f, "patch promotion (run {run_id})"),
            MovedBy::Created => write!(f, "a new version"),
            MovedBy::Migration => write!(f, "schema migration"),
            MovedBy::ScopeMove { from_scope } => write!(f, "copied from the {from_scope} scope"),
        }
    }
}

impl MovedBy {
    /// Whether a frozen playbook refuses this move. A migration and a copy
    /// into another scope carry the definition as it is, so freezing does
    /// not stop them.
    pub(super) fn respects_frozen(&self) -> bool {
        !matches!(self, MovedBy::Migration | MovedBy::ScopeMove { .. })
    }
}

/// The final trial record of a candidate that becomes `current`, written
/// by the operation itself so no gap remains between the move and it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrialFinal {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<QualityRecord>,
}

/// What a history line records.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// `current` moved.
    #[default]
    Move,
    /// A move that stopped before `current` changed.
    Aborted,
}

impl EntryKind {
    fn is_move(&self) -> bool {
        *self == EntryKind::Move
    }
}

/// One line of the history.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Absent on a move.
    #[serde(default, skip_serializing_if = "EntryKind::is_move")]
    #[cfg_attr(feature = "ts", ts(as = "Option<EntryKind>", optional))]
    pub kind: EntryKind,
    pub change_id: String,
    pub at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub from: Option<String>,
    /// The version `current` moved to; absent on an aborted move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub by: Option<MovedBy>,
    /// A person's reason, scrubbed and cut.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub superseded_candidate: Option<String>,
    /// The sha256 of the line before; absent on the first line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub prev: Option<String>,
}

/// The fields of a move line.
pub(super) struct MoveLine {
    pub change_id: String,
    pub from: Option<String>,
    pub to: String,
    pub by: MovedBy,
    pub reason: Option<String>,
    pub superseded_candidate: Option<String>,
}

impl HistoryEntry {
    pub(super) fn moved(line: MoveLine) -> Self {
        HistoryEntry {
            kind: EntryKind::Move,
            change_id: line.change_id,
            at_ms: crate::clock::now_ms_u64(),
            from: line.from,
            to: Some(line.to),
            by: Some(line.by),
            reason: line.reason,
            superseded_candidate: line.superseded_candidate,
            prev: None,
        }
    }

    pub(super) fn aborted(change_id: &str, superseded_candidate: Option<String>) -> Self {
        HistoryEntry {
            kind: EntryKind::Aborted,
            change_id: change_id.to_string(),
            at_ms: crate::clock::now_ms_u64(),
            from: None,
            to: None,
            by: None,
            reason: None,
            superseded_candidate,
            prev: None,
        }
    }
}

/// The history as the surfaces show it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct History {
    /// Newest first.
    pub entries: Vec<HistoryEntry>,
    /// The record has a gap: a line lost or edited (the `prev` chain
    /// breaks), a move that does not start where the previous one ended,
    /// or a `current` other than the last recorded target (moved by an
    /// older apb or by hand).
    pub incomplete: bool,
    /// A move was interrupted and waits for recovery.
    pub pending: bool,
}
