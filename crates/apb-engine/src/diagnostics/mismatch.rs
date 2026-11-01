//! `definition_mismatch`: a resume found a `playbook.yaml` whose digest no
//! journaled definition boundary accounts for (a patch that crashed between
//! replacing the file and journaling `patch_applied`, an edit nothing
//! recorded), or the last boundary is a `patch_applied` an older apb wrote
//! without a digest. The resume still executes the current `playbook.yaml`;
//! this record only tells the read surfaces that the segment from that
//! boundary on is not established.
//!
//! A record is identified by its boundary (and the digest found), so a
//! repeated resume over the same mismatch writes it once, while two
//! mismatches separated by a successful patch are two records.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::writer::{Record, write_with};

/// The record kind.
pub const KIND: &str = "definition_mismatch";

/// A `definition_mismatch` record as the reader returns it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DefinitionMismatch {
    pub after_seq: u64,
    /// The seq of the journaled boundary the mismatch is against: the
    /// `run_started` of the start, a `run_resumed` or a `patch_applied`.
    pub boundary_seq: u64,
    /// The digest that boundary journaled, when it journaled one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journaled: Option<String>,
    /// The digest of the `playbook.yaml` the resume found.
    pub found: String,
    /// `digest_differs` or `patch_without_definition`.
    pub reason: String,
}

/// Writes `found` (see `crate::run_definitions::detect_mismatch`), once per
/// boundary and digest found. Never fails.
pub(crate) fn record(run_dir: &Path, found: DefinitionMismatch) {
    write_with(run_dir, |_| {
        let after_seq = found.after_seq;
        let key = format!("{}|{}", found.boundary_seq, found.found);
        let Value::Object(mut fields) = serde_json::to_value(&found).ok()? else {
            return None;
        };
        fields.remove("after_seq");
        Some(Record {
            kind: KIND,
            after_seq,
            key,
            fields: fields.into_iter().collect::<Map<String, Value>>(),
        })
    });
}
