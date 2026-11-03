//! The views a citation may point into, and how a reader's trace proves a
//! record was read whole.
//!
//! A view is a list of numbered records of one kind (a grader's
//! [`ReadKind`], or an evidence session's view kind), each with the
//! reference a citation names it by; a reference may sit in more than one
//! view (a journal event is in the journal and in a filtered view). An
//! outline entry (`n:<view>`) names a view, never a record. The
//! [`ReadState`] a bounded reader keeps holds references and counts only,
//! never the text: a record is read whole when it is in
//! [`ReadState::full`], and a view is read to its end when every one of
//! its records is.

use std::collections::{BTreeSet, HashMap};

use apb_core::quality::criteria::ReadKind;
use apb_core::quality::grade::{ReadOp, ReadTrace, ViewRead};
use serde::{Deserialize, Serialize};

/// The prefix of an outline entry: `n:<view>` routes a reader to a view
/// and never names a record.
pub const OUTLINE_PREFIX: &str = "n:";

/// Whether `r` is an outline entry rather than a record reference.
pub fn is_outline_ref(r: &str) -> bool {
    r.starts_with(OUTLINE_PREFIX)
}

/// One record of a view: its citation and its text before redaction.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceRecord {
    pub r#ref: String,
    pub text: String,
}

/// One readable view.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceView<K = ReadKind> {
    pub id: String,
    pub kind: K,
    pub complete: bool,
    pub records: Vec<SourceRecord>,
}

impl<K> SourceView<K> {
    pub fn chars(&self) -> u64 {
        self.records
            .iter()
            .map(|r| r.text.chars().count() as u64)
            .sum()
    }
}

/// The views a citation may point into, with where each reference is.
#[derive(Debug, Clone)]
pub struct ViewSet<K = ReadKind> {
    views: Vec<SourceView<K>>,
    /// Where each reference is: `(view, record)` indexes, every view that
    /// holds it.
    index: HashMap<String, Vec<(usize, usize)>>,
    /// Records a reader may show but no citation may rest on (a record the
    /// redactor changed: a span of it would not be a range of stored
    /// bytes). Empty for a grader's views.
    uncitable: BTreeSet<String>,
}

impl<K: PartialEq> ViewSet<K> {
    pub fn new(views: Vec<SourceView<K>>) -> Self {
        let mut index: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
        for (vi, v) in views.iter().enumerate() {
            for (ri, r) in v.records.iter().enumerate() {
                index.entry(r.r#ref.clone()).or_default().push((vi, ri));
            }
        }
        ViewSet {
            views,
            index,
            uncitable: BTreeSet::new(),
        }
    }

    /// These views with the records `refs` shown but never citable.
    pub fn with_uncitable(mut self, refs: impl IntoIterator<Item = String>) -> Self {
        self.uncitable.extend(refs);
        self
    }

    /// Whether a citation may rest on the record `r`.
    pub fn citable(&self, r: &str) -> bool {
        !self.uncitable.contains(r)
    }

    /// Every view, in order.
    pub fn all(&self) -> &[SourceView<K>] {
        &self.views
    }

    pub fn view(&self, id: &str) -> Option<&SourceView<K>> {
        self.views.iter().find(|v| v.id == id)
    }

    /// Every view that holds the record `r`.
    pub fn holding(&self, r: &str) -> impl Iterator<Item = &SourceView<K>> {
        self.index
            .get(r)
            .into_iter()
            .flatten()
            .map(|(v, _)| &self.views[*v])
    }

    /// The views of the kinds a criterion reads.
    pub fn of_kinds(&self, kinds: &[K]) -> impl Iterator<Item = &SourceView<K>> {
        self.views.iter().filter(move |v| kinds.contains(&v.kind))
    }

    /// The record a citation names, wherever it is.
    pub fn record(&self, r: &str) -> Option<&SourceRecord> {
        let (v, i) = *self.index.get(r)?.first()?;
        Some(&self.views[v].records[i])
    }

    /// Replaces every record's text with `f` of it; the references, and so
    /// the index, stay as they are.
    pub(crate) fn map_texts(&mut self, mut f: impl FnMut(&str) -> String) {
        for v in &mut self.views {
            for r in &mut v.records {
                r.text = f(&r.text);
            }
        }
    }
}

/// What was read so far: the trace operations, the records read in full
/// and the references seen only through routing (outline, children,
/// search). References and counts only.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReadState {
    pub budget_chars: u64,
    pub read_chars: u64,
    #[serde(default)]
    pub ops: Vec<ReadOp>,
    /// Records read in full.
    #[serde(default)]
    pub full: BTreeSet<String>,
    /// References a routing answer named.
    #[serde(default)]
    pub routed: BTreeSet<String>,
}

impl ReadState {
    pub fn new(budget_chars: u64) -> Self {
        ReadState {
            budget_chars,
            ..ReadState::default()
        }
    }

    /// Whether every record of `view` was read in full.
    pub fn ended<K>(&self, view: &SourceView<K>) -> bool {
        view.records.iter().all(|r| self.full.contains(&r.r#ref))
    }

    /// The trace a grade stores: the operations and how far each view was
    /// read.
    pub fn trace<K: PartialEq>(&self, views: &ViewSet<K>) -> ReadTrace {
        let views = views
            .all()
            .iter()
            .map(|v| {
                let read = v
                    .records
                    .iter()
                    .filter(|r| self.full.contains(&r.r#ref))
                    .count();
                let share = if v.records.is_empty() {
                    1.0
                } else {
                    apb_core::stats::round4(read as f64 / v.records.len() as f64)
                };
                ViewRead {
                    view: v.id.clone(),
                    complete: v.complete,
                    ended: self.ended(v),
                    read_share: share,
                }
            })
            .collect();
        ReadTrace {
            budget_chars: self.budget_chars,
            read_chars: self.read_chars,
            ops: self.ops.clone(),
            views,
        }
    }
}
