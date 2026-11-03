//! The bounded reader every grader reads a run through: the shared reader
//! of `crate::citation::reader`, over a run's [`super::sources::RunSources`]
//! views. Outline entries, child listings and search snippets route a
//! reader and never prove anything: only a record returned whole by `read`
//! or `page` counts as read in full and may be cited.

pub use crate::citation::reader::{
    DEFAULT_BUDGET_CHARS, MAX_HITS, MAX_PAGE, ReadItem, ReadRequest, ReadResponse, ReadState,
    Reader, SNIPPET_CHARS,
};
