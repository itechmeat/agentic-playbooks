//! The bounded reader every reading session goes through: a run grader's
//! (`crate::grade`) and an evidence session's (`crate::evidence`).
//!
//! - `outline` lists the views (`n:<view>`, record counts, completeness);
//! - `children` lists a view's record references with a short snippet;
//! - `read` returns one record, whole or a character span of it;
//! - `page` returns consecutive records of a view, each whole;
//! - `search` returns the references whose text holds a phrase, with a
//!   snippet.
//!
//! Outline entries, child listings and search snippets route a reader and
//! never prove anything: only a record returned whole by `read` or `page`
//! counts as read in full and may be cited (and only when the view set
//! lets it be, [`ViewSet::citable`]). Every returned text goes through the
//! redactor, or was redacted before the views were built, and every
//! character returned counts against the session's budget. The
//! [`ReadState`] keeps what was read as references and counts, never the
//! text, so it can be stored.

use std::borrow::Cow;
use std::collections::BTreeSet;

use apb_core::quality::grade::ReadOp;
use serde::{Deserialize, Serialize};

use super::views::{OUTLINE_PREFIX, SourceView, ViewSet, is_outline_ref};
use crate::error::EngineError;
use crate::redact::Redactor;

/// The characters a host session may read when it names no budget.
pub const DEFAULT_BUDGET_CHARS: u64 = 200_000;
/// The longest snippet a child listing or a search hit shows.
pub const SNIPPET_CHARS: usize = 80;
/// The most records one `children` or `page` call returns.
pub const MAX_PAGE: u64 = 200;
/// The most hits one search returns.
pub const MAX_HITS: usize = 50;
/// The most trace operations a session keeps; beyond this the oldest is
/// dropped, so a repeating no-op read cannot grow the file without end.
pub const MAX_OPS: usize = 512;

pub use super::views::ReadState;

/// One request to the reader.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ReadRequest {
    Outline,
    Children {
        view: String,
        #[serde(default)]
        from: u64,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        count: Option<u64>,
    },
    Read {
        #[serde(rename = "ref")]
        r#ref: String,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        span: Option<[u64; 2]>,
    },
    Page {
        view: String,
        #[serde(default)]
        from: u64,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        count: Option<u64>,
    },
    Search {
        query: String,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        view: Option<String>,
    },
}

/// One item of an answer.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReadItem {
    #[serde(rename = "ref")]
    pub r#ref: String,
    /// The redacted text: a whole record, a span of one, a snippet, or an
    /// outline line.
    pub text: String,
    /// Whether the item may be cited (a record returned whole).
    pub citable: bool,
}

/// The reader's answer.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReadResponse {
    pub items: Vec<ReadItem>,
    pub read_chars: u64,
    pub budget_chars: u64,
    /// Set when the answer stopped short (the budget, the page size).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub note: Option<String>,
}

/// A reader over one set of views.
pub struct Reader<'a, K = apb_core::quality::criteria::ReadKind> {
    views: &'a ViewSet<K>,
    /// The redactor every shown text goes through; `None` when the record
    /// texts were redacted before the views were built.
    redactor: Option<&'a Redactor>,
    pub state: ReadState,
}

impl<'a, K: PartialEq> Reader<'a, K> {
    pub(crate) fn new(
        views: &'a ViewSet<K>,
        redactor: Option<&'a Redactor>,
        state: ReadState,
    ) -> Self {
        Reader {
            views,
            redactor,
            state,
        }
    }

    fn view(&self, id: &str) -> Result<&'a SourceView<K>, EngineError> {
        self.views
            .view(id)
            .ok_or_else(|| EngineError::Invalid(format!("no view `{id}` in this session")))
    }

    fn charge(&mut self, op: &str, r: &str, chars: u64) {
        self.state.read_chars = self.state.read_chars.saturating_add(chars);
        // The trace keeps at most MAX_OPS entries: a reader that repeats an
        // op that returns nothing (an empty listing, a search with no hit)
        // cannot grow the session file without end.
        if self.state.ops.len() == MAX_OPS {
            self.state.ops.drain(..1);
        }
        self.state.ops.push(ReadOp {
            op: op.to_string(),
            r#ref: r.to_string(),
            chars,
        });
    }

    fn left(&self) -> u64 {
        self.state
            .budget_chars
            .saturating_sub(self.state.read_chars)
    }

    fn respond(&self, items: Vec<ReadItem>, note: Option<String>) -> ReadResponse {
        ReadResponse {
            items,
            read_chars: self.state.read_chars,
            budget_chars: self.state.budget_chars,
            note,
        }
    }

    pub fn handle(&mut self, req: &ReadRequest) -> Result<ReadResponse, EngineError> {
        match req {
            ReadRequest::Outline => Ok(self.outline()),
            ReadRequest::Children { view, from, count } => self.children(view, *from, *count),
            ReadRequest::Read { r#ref, span } => self.read(r#ref, *span),
            ReadRequest::Page { view, from, count } => self.page(view, *from, *count),
            ReadRequest::Search { query, view } => self.search(query, view.as_deref()),
        }
    }

    /// The views: `n:<view>` with the record count, the characters and the
    /// completeness. No source text.
    pub fn outline(&mut self) -> ReadResponse {
        let items: Vec<ReadItem> = self
            .views
            .all()
            .iter()
            .map(|v| ReadItem {
                r#ref: format!("{OUTLINE_PREFIX}{}", v.id),
                text: format!(
                    "{} records, {} chars, {}",
                    v.records.len(),
                    v.chars(),
                    if v.complete { "complete" } else { "incomplete" }
                ),
                citable: false,
            })
            .collect();
        self.charge("outline", OUTLINE_PREFIX, 0);
        self.respond(items, None)
    }

    /// A record's text as the reader shows it: redacted.
    fn shown<'t>(&self, text: &'t str) -> Cow<'t, str> {
        match self.redactor {
            Some(r) => Cow::Owned(r.redact(text)),
            None => Cow::Borrowed(text),
        }
    }

    /// At most [`SNIPPET_CHARS`] of the shown text `shown`, starting a
    /// little before character `at`.
    fn snippet(shown: &str, at: usize) -> String {
        shown
            .chars()
            .skip(at.saturating_sub(SNIPPET_CHARS / 4))
            .take(SNIPPET_CHARS)
            .collect()
    }

    /// A routing answer's snippet when the budget still has room for it;
    /// `None` (and a note) when it does not.
    fn routed_snippet(
        &mut self,
        r: &str,
        text: String,
        chars: &mut u64,
        note: &mut Option<String>,
    ) -> Option<ReadItem> {
        let n = text.chars().count() as u64;
        if chars.saturating_add(n) > self.left() {
            *note = Some(format!(
                "stopped before `{r}`: the budget has {} chars left",
                self.left().saturating_sub(*chars)
            ));
            return None;
        }
        *chars += n;
        self.state.routed.insert(r.to_string());
        Some(ReadItem {
            r#ref: r.to_string(),
            text,
            citable: false,
        })
    }

    pub fn children(
        &mut self,
        view: &str,
        from: u64,
        count: Option<u64>,
    ) -> Result<ReadResponse, EngineError> {
        let v = self.view(view)?;
        let count = count.unwrap_or(MAX_PAGE).min(MAX_PAGE);
        let mut items = Vec::new();
        let (mut chars, mut note) = (0u64, None);
        for r in v.records.iter().skip(from as usize).take(count as usize) {
            let text = Self::snippet(&self.shown(&r.text), 0);
            match self.routed_snippet(&r.r#ref, text, &mut chars, &mut note) {
                Some(item) => items.push(item),
                None => break,
            }
        }
        self.charge("children", &format!("n:{view}"), chars);
        Ok(self.respond(items, note))
    }

    pub fn read(&mut self, r: &str, span: Option<[u64; 2]>) -> Result<ReadResponse, EngineError> {
        if is_outline_ref(r) {
            return Err(EngineError::Invalid(format!(
                "`{r}` is an outline entry: list its records with `children`"
            )));
        }
        let rec = self
            .views
            .record(r)
            .ok_or_else(|| EngineError::Invalid(format!("no record `{r}` in this session")))?;
        let text = self.shown(&rec.text).into_owned();
        let total = text.chars().count() as u64;
        let (from, to) = match span {
            Some([a, b]) if a < b => (a.min(total), b.min(total)),
            Some(_) => {
                return Err(EngineError::Invalid(
                    "a span is [from, to) with from < to".into(),
                ));
            }
            None => (0, total),
        };
        let part: String = text
            .chars()
            .skip(from as usize)
            .take((to - from) as usize)
            .collect();
        let chars = part.chars().count() as u64;
        if chars > self.left() {
            return Ok(self.respond(
                Vec::new(),
                Some(format!(
                    "`{r}` holds {chars} chars and {} are left in the budget: read a span of it",
                    self.left()
                )),
            ));
        }
        let whole = from == 0 && to == total;
        if whole {
            self.state.full.insert(r.to_string());
        }
        self.charge("read", r, chars);
        Ok(self.respond(
            vec![ReadItem {
                r#ref: r.to_string(),
                text: part,
                citable: whole && self.views.citable(r),
            }],
            None,
        ))
    }

    pub fn page(
        &mut self,
        view: &str,
        from: u64,
        count: Option<u64>,
    ) -> Result<ReadResponse, EngineError> {
        let v = self.view(view)?;
        let count = count.unwrap_or(MAX_PAGE).min(MAX_PAGE);
        let mut items = Vec::new();
        let mut note = None;
        for r in v.records.iter().skip(from as usize).take(count as usize) {
            let text = self.shown(&r.text).into_owned();
            let chars = text.chars().count() as u64;
            if chars > self.left() {
                note = Some(format!(
                    "stopped before `{}`: the budget has {} chars left",
                    r.r#ref,
                    self.left()
                ));
                break;
            }
            self.state.full.insert(r.r#ref.clone());
            self.charge("page", &r.r#ref, chars);
            items.push(ReadItem {
                citable: self.views.citable(&r.r#ref),
                r#ref: r.r#ref.clone(),
                text,
            });
        }
        Ok(self.respond(items, note))
    }

    pub fn search(&mut self, query: &str, view: Option<&str>) -> Result<ReadResponse, EngineError> {
        let needle: Vec<char> = query.trim().chars().flat_map(char::to_lowercase).collect();
        if needle.is_empty() {
            return Err(EngineError::Invalid("an empty search".into()));
        }
        let views: Vec<&SourceView<K>> = match view {
            Some(id) => vec![self.view(id)?],
            None => self.views.all().iter().collect(),
        };
        let mut items: Vec<ReadItem> = Vec::new();
        let (mut chars, mut note) = (0u64, None);
        let mut hit = BTreeSet::new();
        'views: for v in views {
            for r in &v.records {
                // A `j:` reference is in several views: one hit each.
                if hit.contains(&r.r#ref) {
                    continue;
                }
                let shown = self.shown(&r.text);
                let Some(at) = find_ignoring_case(&shown, &needle) else {
                    continue;
                };
                hit.insert(r.r#ref.clone());
                let text = Self::snippet(&shown, at);
                match self.routed_snippet(&r.r#ref, text, &mut chars, &mut note) {
                    Some(item) => items.push(item),
                    None => break 'views,
                }
                if items.len() >= MAX_HITS {
                    break 'views;
                }
            }
        }
        // The query is the reader's own text: the trace names only where
        // it searched.
        self.charge("search", &format!("n:{}", view.unwrap_or("*")), chars);
        Ok(self.respond(items, note))
    }

    /// The rules grader's read of a whole view: every record counts as read
    /// in full and its characters count, but no text leaves the process.
    pub(crate) fn scan(&mut self, view: &str) {
        let Some(v) = self.views.view(view) else {
            return;
        };
        let chars = v.chars();
        for r in &v.records {
            self.state.full.insert(r.r#ref.clone());
        }
        self.charge("scan", &format!("n:{view}"), chars);
    }
}

/// The character index in `text` where `needle` (lowercased) starts,
/// comparing case-insensitively per character, so the index counts the
/// characters of `text` itself (lowercasing may change how many there
/// are).
fn find_ignoring_case(text: &str, needle: &[char]) -> Option<usize> {
    // Each lowered char, with the index of the char of `text` it comes from.
    let lowered: Vec<(char, usize)> = text
        .chars()
        .enumerate()
        .flat_map(|(i, c)| c.to_lowercase().map(move |l| (l, i)))
        .collect();
    if needle.is_empty() || needle.len() > lowered.len() {
        return None;
    }
    (0..=lowered.len() - needle.len())
        .find(|&s| needle.iter().zip(&lowered[s..]).all(|(n, (l, _))| n == l))
        .map(|s| lowered[s].1)
}

#[cfg(test)]
mod tests {
    use super::find_ignoring_case;

    #[test]
    fn a_match_is_found_at_the_index_of_the_original_text() {
        let needle: Vec<char> = "token".chars().collect();
        // `İ` lowercases to two chars: the index still counts the original.
        assert_eq!(find_ignoring_case("İİ TOKEN here", &needle), Some(3));
        assert_eq!(find_ignoring_case("no match", &needle), None);
    }
}
