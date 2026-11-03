//! Record packages: a result entry declared `format: records` holds one
//! package, UTF-8 JSON Lines whose first line is a header and every later
//! line one record.
//!
//! ```text
//! {"apb_records": 1, "complete": true, "source": "inbox of the support desk"}
//! {"id": "m-1", "kind": "email", "text": "...", "source": "imap", "version": "3", "at": "2026-11-03T10:00:00Z"}
//! ```
//!
//! - `apb_records` is the format version, `1` here;
//! - `complete` is the collector's own statement about its source, shown as
//!   such ("the collector says the package is complete"), never a promise
//!   about the outside world;
//! - a record `id` matches `[A-Za-z0-9._:-]{1,128}` and is unique in the
//!   package; `kind` is a slug (`[a-z0-9][a-z0-9_-]*`, at most 64); at most
//!   [`MAX_RECORDS`] records.
//!
//! A text that does not parse is no package: capture stores it as plain
//! text and records the first problem ([`parse`]). A problem names lines
//! and fields only, never the text it read.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// The header's format version this binary reads and writes.
pub const RECORDS_VERSION: u32 = 1;
/// The most records one package holds.
pub const MAX_RECORDS: usize = 10_000;
/// The longest record id.
pub const MAX_ID_CHARS: usize = 128;
/// The longest record kind.
pub const MAX_KIND_CHARS: usize = 64;

/// A package's first line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub apb_records: u32,
    /// The collector says its source is wholly in the package.
    pub complete: bool,
    /// What the collector read, in its own words.
    pub source: String,
}

/// One record of a package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub id: String,
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

/// A parsed package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub header: Header,
    pub records: Vec<Record>,
}

/// Whether `id` is a record id: `[A-Za-z0-9._:-]{1,128}`.
pub fn is_record_id(id: &str) -> bool {
    (1..=MAX_ID_CHARS).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// Whether `kind` is a record kind: a slug `[a-z0-9][a-z0-9_-]*` of at most
/// [`MAX_KIND_CHARS`] characters.
pub fn is_kind(kind: &str) -> bool {
    let b = kind.as_bytes();
    (1..=MAX_KIND_CHARS).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-'))
}

/// The kind of a decode error, without the text it met.
fn json_problem(e: &serde_json::Error) -> &'static str {
    use serde_json::error::Category;
    match e.classify() {
        Category::Data => "a field is missing, unknown or of the wrong type",
        Category::Syntax | Category::Eof => "not a JSON object",
        Category::Io => "unreadable",
    }
}

/// Parses `text` as a package: the package, or the first problem (lines
/// are 1-based; the problem names no text of the package).
pub fn parse(text: &str) -> Result<Package, String> {
    let body = text.strip_suffix('\n').unwrap_or(text);
    let mut lines = body.split('\n');
    let first = lines.next().unwrap_or_default();
    let header: Header =
        serde_json::from_str(first).map_err(|e| format!("line 1: header {}", json_problem(&e)))?;
    if header.apb_records != RECORDS_VERSION {
        return Err(format!(
            "line 1: apb_records is {}, this apb reads {RECORDS_VERSION}",
            header.apb_records
        ));
    }
    let mut records = Vec::new();
    let mut seen = HashSet::new();
    for (i, line) in lines.enumerate() {
        let n = i + 2;
        if records.len() == MAX_RECORDS {
            return Err(format!(
                "line {n}: more than {MAX_RECORDS} records in one package"
            ));
        }
        let r: Record = serde_json::from_str(line)
            .map_err(|e| format!("line {n}: record {}", json_problem(&e)))?;
        if !is_record_id(&r.id) {
            return Err(format!(
                "line {n}: the id is not [A-Za-z0-9._:-], 1 to {MAX_ID_CHARS} characters"
            ));
        }
        if !is_kind(&r.kind) {
            return Err(format!("line {n}: the kind is not a slug"));
        }
        if !seen.insert(r.id.clone()) {
            return Err(format!("line {n}: the id repeats an earlier record's"));
        }
        records.push(r);
    }
    Ok(Package { header, records })
}
