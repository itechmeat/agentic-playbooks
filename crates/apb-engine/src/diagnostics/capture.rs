//! `result_capture_failed`: a result snapshot was not stored (an I/O error,
//! a full store, a lock that did not come in time), or it was stored and its
//! transaction not closed. The node settled all the same; this record only
//! tells the read surfaces why a settlement carries no stamp. One record per
//! settlement (its node, execution, item and the journal seq it follows).
/// The kind of a record that a result snapshot was not stored.
pub const KIND: &str = "result_capture_failed";

/// A `result_capture_failed` record: the settlement's subject and why its
/// snapshot was not stored. The node settled without a stamp all the same.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CaptureFailed {
    pub after_seq: u64,
    pub node: String,
    pub execution: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    pub reason: String,
}

/// The longest reason a `result_capture_failed` record keeps.
const CAPTURE_REASON_MAX: usize = 300;

/// Writes a `result_capture_failed` record, once per settlement. Never fails.
pub(crate) fn record(
    run_dir: &std::path::Path,
    after_seq: u64,
    subject: &apb_core::result_store::SnapshotSubject,
    reason: &str,
) {
    super::writer::write_with(run_dir, |scrub| {
        let found = CaptureFailed {
            after_seq,
            node: subject.node.clone(),
            execution: subject.execution,
            item: subject.item.clone(),
            reason: scrub.clip(reason, CAPTURE_REASON_MAX),
        };
        let key = format!(
            "{}|{}|{}|{after_seq}",
            found.node,
            found.execution,
            found.item.as_deref().unwrap_or("")
        );
        let serde_json::Value::Object(mut fields) = serde_json::to_value(&found).ok()? else {
            return None;
        };
        fields.remove("after_seq");
        Some(super::writer::Record {
            kind: KIND,
            after_seq,
            key,
            fields,
        })
    });
}

/// Every `result_capture_failed` record of the sidecar at `run_dir`, in file
/// order, read as tolerantly as [`read`]: for the read-only surfaces.
pub fn capture_failures(run_dir: &std::path::Path) -> Vec<CaptureFailed> {
    super::index::read_raw(run_dir)
        .into_iter()
        .filter(|v| {
            v.get("v").and_then(serde_json::Value::as_u64) == Some(u64::from(super::VERSION))
        })
        .filter(|v| v.get("kind").and_then(serde_json::Value::as_str) == Some(KIND))
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect()
}
