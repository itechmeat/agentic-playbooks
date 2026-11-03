//! The bounded reader every grader reads a run through: the one reader of
//! [`crate::citation::reader`], over a run's [`RunSources`]. Outline
//! entries, child listings and search snippets route a reader and never
//! prove anything: only a record returned whole by `read` or `page` counts
//! as read in full and may be cited. Every returned text goes through the
//! workspace's redactor, and every character returned counts against the
//! session's budget. The [`ReadState`] keeps what was read as references
//! and counts, never the text, so it can be stored; it lives in
//! `crate::citation::views`, where a citation is checked against it.

use super::sources::RunSources;
use crate::redact::Redactor;

pub use crate::citation::reader::{
    DEFAULT_BUDGET_CHARS, MAX_HITS, MAX_PAGE, ReadItem, ReadRequest, ReadResponse, ReadState,
    Reader, SNIPPET_CHARS,
};

impl<'a> Reader<'a> {
    /// A reader over the sources of one run.
    pub(crate) fn new(sources: &'a RunSources, redactor: &'a Redactor, state: ReadState) -> Self {
        Reader::over(&sources.views, sources.is_redacted(), redactor, state)
    }
}
