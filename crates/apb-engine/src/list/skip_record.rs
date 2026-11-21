//! The records of a list skip (`list.skip`, incremental lists D2): the set
//! one list node's `list_skip_read` line holds and the view the run
//! surfaces show. A leaf the journal and the views read; the reading and
//! the writing live in [`super::skip`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One list node's skip set: every item id that passed in the source run
/// with the skip digest it passed with.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SkipSet {
    /// The run the set was read from; absent when there is none (no
    /// earlier success, a child, scenario or eval run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Item id -> the skip digest it passed with.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub items: BTreeMap<String, String>,
    /// More entries passed than `LIST_SKIP_MAX_ENTRIES` or
    /// `LIST_SKIP_MAX_BYTES` keep: the first in id order were kept, the
    /// rest run.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Item id -> the output its source settlement recorded (the compact
    /// JSON of its declared outputs, at most `LIST_SKIP_OUTPUT_MAX_BYTES`,
    /// scrubbed when it was journaled): a skipped item carries it, so later
    /// nodes read what the item produced. An entry of `items` absent here
    /// passed with no output object, and a skipped item carries none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, String>,
    /// Items that passed in the source with a stamp but are not in the set
    /// because their output cannot be carried: withheld (clipped at
    /// settlement or a redaction marker in it) or over the per-item cap.
    /// They are worked.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub not_inherited: u32,
    /// Set-time refusal, for two reasons: the run's type cannot be told
    /// after a crash (`skip_run_type_unknown`, written by
    /// [`ensure_recorded`](super::skip::ensure_recorded)), or the source run
    /// is no longer an earlier success on the read the sets come from
    /// (`skip_source_ineligible`); the node then skips nothing in this run.
    /// Every reason that needs the work is decided at the visit's start and
    /// recorded on its `list_started` (`skip_refused`), never here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// The skip of one list node as the run surfaces show it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkipView {
    /// Items of the visit a skip settled.
    pub skipped: u32,
    /// Entries the run's set holds.
    pub read_items: u32,
    /// The run the set was read from.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub source: Option<String>,
    /// The set kept fewer entries than passed.
    pub truncated: bool,
    /// Why the node skips nothing: the set's refusal, else the visit's.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub refused: Option<String>,
    /// Passes of the source the set left out because their output cannot
    /// be carried; they are worked.
    #[serde(default)]
    pub not_inherited: u32,
    /// Skipped items worked again because their skip stopped holding while
    /// the visit was open (the work changed, or a supervisor note came into
    /// force).
    #[serde(default)]
    pub reworked: u32,
    /// Why the visit stopped stamping part way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub stopped_stamping: Option<String>,
}
