//! The diagnostics sidecar of a run: `runs/<id>/diagnostics.jsonl`.
//!
//! Diagnostics explain a run (what each condition of a routing decision
//! evaluated to, a definition that no journaled boundary accounts for); they
//! never steer it. The rule this module exists for: **removing every
//! diagnostic leaves execution identical**. So:
//!
//! - nothing is written to `events.jsonl` (no event type is added for a
//!   diagnostic, so no released binary ever meets one in the journal);
//! - execution code (the drive, folds, routing, admission, resume, gates)
//!   never opens the file; it hands a finished value to [`writer`], which
//!   swallows its own errors (counted in [`writer::write_errors`]);
//! - [`reader`] is for the read-only surfaces, tolerant of a missing file,
//!   unknown kinds, unparsable lines and a torn last line, and it never
//!   creates, repairs or backfills anything;
//! - [`index`] keeps a record from being written twice after a resume and
//!   is visible to the writer and the read surfaces only.
//!
//! Each line is one JSON object `{ "v": 1, "kind": "...", "after_seq": <the
//! journal seq it follows>, ... }`, written by one `write` call under the
//! sidecar's own lock, in a file created 0600. Every text a record carries
//! is scrubbed whole by the run's redactor first and clipped afterwards
//! ([`writer::Scrub`]).

pub mod capture;
pub(crate) mod index;
pub mod mismatch;
pub mod reader;
pub mod route;
pub mod writer;

/// The sidecar's file name inside a run directory.
pub const FILE: &str = "diagnostics.jsonl";

/// The lock file the writer holds for one append.
pub const LOCK: &str = "diagnostics.lock";

/// The record format this apb writes.
pub const VERSION: u32 = 1;

/// The longest a sidecar may grow; past it nothing more is written.
pub const MAX_BYTES: u64 = 8 * 1024 * 1024;

pub use capture::capture_failures;
pub use reader::{Diagnostic, read};
