//! Record packages: a result entry declared `format: records`.
//!
//! A package is UTF-8 JSON Lines. The first line is the header
//! `{"apb_records": 1, "complete": true|false, "source": "<text>"}`, every
//! later non-empty line one record
//! `{"id", "kind", "text", "source"?, "version"?, "at"?}`. A record id
//! matches `[A-Za-z0-9._:-]{1,128}` and is unique in its package; a kind is
//! a slug (`[a-z0-9][a-z0-9_-]*`, at most 64 characters); a package holds
//! at most [`MAX_RECORDS`] records.
//!
//! `complete` is the collector's own statement about its source, shown as
//! such; the engine adds its own (a package from an incomplete snapshot is
//! incomplete) and never promises completeness relative to the outside
//! world. A text that breaks any rule is not a package: [`parse`] names
//! the first problem, and capture stores the entry as plain text with
//! that problem (`SnapshotEntry::records_invalid`).

use std::collections::BTreeSet;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use serde::{Deserialize, Serialize};

/// The package format this binary reads.
pub const FORMAT_VERSION: u64 = 1;
/// The most records one package holds.
pub const MAX_RECORDS: usize = 10_000;
/// The longest record id.
pub const MAX_ID_LEN: usize = 128;
/// The longest kind.
pub const MAX_KIND_LEN: usize = 64;

/// The header line of a package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub apb_records: u64,
    /// The collector's own statement that its source was read whole.
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

/// A package that parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub header: Header,
    pub records: Vec<Record>,
}

/// Whether `id` is a record id: `[A-Za-z0-9._:-]{1,128}`.
pub fn is_record_id(id: &str) -> bool {
    (1..=MAX_ID_LEN).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// Whether `kind` is a slug: `[a-z0-9][a-z0-9_-]*`, at most 64 characters.
pub fn is_kind(kind: &str) -> bool {
    let mut bytes = kind.bytes();
    kind.len() <= MAX_KIND_LEN
        && bytes
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Every byte except the RFC 3986 unreserved characters: a record id's `:`
/// is encoded, so `@<id>` never meets another separator of a reference.
const RECORD: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// A record id as a reference carries it, percent-encoded.
pub fn encode_id(id: &str) -> String {
    utf8_percent_encode(id, RECORD).to_string()
}

/// The record id a reference carries, or `None` when it does not decode to
/// a record id.
pub fn decode_id(encoded: &str) -> Option<String> {
    let id = percent_decode_str(encoded).decode_utf8().ok()?.into_owned();
    is_record_id(&id).then_some(id)
}

/// Parses `text` as a package; the first problem when it is not one. A
/// problem names line numbers and rule names only, never record text.
pub fn parse(text: &str) -> Result<Package, String> {
    let mut lines = text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty());
    let (_, first) = lines
        .next()
        .ok_or_else(|| "the package is empty: line 1 must be the header".to_string())?;
    let header: Header = serde_json::from_str(first)
        .map_err(|_| "line 1 is not the header {\"apb_records\": 1, \"complete\": true|false, \"source\": \"...\"}".to_string())?;
    if header.apb_records != FORMAT_VERSION {
        return Err(format!(
            "the header names format {}; this apb reads format {FORMAT_VERSION}",
            header.apb_records
        ));
    }
    let mut records = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, line) in lines {
        let n = i + 1;
        if records.len() == MAX_RECORDS {
            return Err(format!("more than {MAX_RECORDS} records"));
        }
        let record: Record = serde_json::from_str(line).map_err(|_| {
            format!("line {n} is not a record {{\"id\", \"kind\", \"text\", \"source\"?, \"version\"?, \"at\"?}}")
        })?;
        if !is_record_id(&record.id) {
            return Err(format!(
                "line {n}: the id is not [A-Za-z0-9._:-], 1 to {MAX_ID_LEN} characters"
            ));
        }
        if !is_kind(&record.kind) {
            return Err(format!("line {n}: the kind is not a slug"));
        }
        if !seen.insert(record.id.clone()) {
            return Err(format!("line {n}: the id repeats an earlier record"));
        }
        records.push(record);
    }
    Ok(Package { header, records })
}
