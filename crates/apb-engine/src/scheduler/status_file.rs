//! Per-attempt status-file protocol for `agent_task` nodes (subtask S2).
//!
//! Each attempt is handed an `APB_STATUS_FILE` env var pointing at a per-attempt
//! JSON file in the run directory. The agent MAY write its final verdict there
//! as `{"status": "success"|"failure", "outputs": { ... }}`. The engine reads
//! that file FIRST when deciding the attempt's status and outputs, and falls
//! back to parsing the agent's textual report when the file is absent,
//! unreadable, invalid, or carries an unrecognized status. The read stays total
//! and panic-free so a malformed file only means "fall back", never a crash.

use std::path::Path;

use serde::Deserialize;

use crate::state::NodeStatus;

/// The paragraph appended to an `agent_task` prompt when the node has a
/// `success_check`, describing the status-file contract to the agent. Kept as a
/// `const` so the exact wording is stable and directly unit-testable. A node
/// without a `success_check` keeps the historical report-only contract, so the
/// note would be noise there and is omitted.
pub(crate) const STATUS_FILE_NOTE: &str = concat!(
    "Status file: the path in the APB_STATUS_FILE environment variable points at a ",
    "JSON file for your final result. You may write it as ",
    "{\"status\": \"success\"|\"failure\", \"outputs\": {}} where outputs is an object ",
    "of the values this step should expose to later steps. The engine reads that file ",
    "first to decide this step's status and outputs, and falls back to your textual ",
    "report when the file is absent or invalid.",
);

/// The status-file prompt note for a node, or an empty string when the node has
/// no `success_check`.
pub(crate) fn status_file_note(has_success_check: bool) -> &'static str {
    if has_success_check {
        STATUS_FILE_NOTE
    } else {
        ""
    }
}

/// The engine's interpretation of a per-attempt status file: the reported
/// status plus, when the file carried a non-empty `outputs` object, its compact
/// JSON string to become the node's downstream output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusFileReport {
    pub status: NodeStatus,
    pub outputs: Option<String>,
}

/// The raw on-disk shape. `outputs` is optional and untyped so any JSON value
/// deserializes; only a non-empty object is honored downstream.
#[derive(Deserialize)]
struct RawStatusFile {
    status: String,
    #[serde(default)]
    outputs: Option<serde_json::Value>,
}

/// Reads an attempt's status file, returning `Some` only when the file exists,
/// is valid JSON, and carries a recognized `status` (`"success"`/`"failure"`).
/// Every other case - missing, unreadable, invalid JSON, or an unknown status
/// string - returns `None` so the caller falls back to the textual report.
/// A non-empty `outputs` object is serialized to compact JSON; anything else
/// (absent, empty, or a non-object value) yields `None` outputs, keeping the
/// agent's parsed output.
pub(crate) fn read_status_file(path: &Path) -> Option<StatusFileReport> {
    let raw = std::fs::read_to_string(path).ok()?;
    let parsed: RawStatusFile = serde_json::from_str(&raw).ok()?;
    let status = match parsed.status.as_str() {
        "success" => NodeStatus::Succeeded,
        "failure" => NodeStatus::Failed,
        _ => return None,
    };
    let outputs = parsed
        .outputs
        .as_ref()
        .and_then(|v| v.as_object())
        .filter(|o| !o.is_empty())
        .and_then(|o| serde_json::to_string(o).ok());
    Some(StatusFileReport { status, outputs })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, body: &str) -> std::path::PathBuf {
        let p = dir.join("status.json");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn reads_valid_success_with_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(
            dir.path(),
            r#"{"status":"success","outputs":{"key":"val"}}"#,
        );
        let sfr = read_status_file(&p).expect("valid success parses");
        assert_eq!(sfr.status, NodeStatus::Succeeded);
        assert_eq!(sfr.outputs.as_deref(), Some(r#"{"key":"val"}"#));
    }

    #[test]
    fn reads_valid_failure_without_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), r#"{"status":"failure"}"#);
        let sfr = read_status_file(&p).expect("valid failure parses");
        assert_eq!(sfr.status, NodeStatus::Failed);
        assert_eq!(sfr.outputs, None);
    }

    #[test]
    fn empty_outputs_object_yields_none_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), r#"{"status":"success","outputs":{}}"#);
        let sfr = read_status_file(&p).expect("valid success parses");
        assert_eq!(sfr.outputs, None);
    }

    #[test]
    fn missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_status_file(&dir.path().join("nope.json")).is_none());
    }

    #[test]
    fn invalid_json_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "not json {");
        assert!(read_status_file(&p).is_none());
    }

    #[test]
    fn unknown_status_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), r#"{"status":"maybe"}"#);
        assert!(read_status_file(&p).is_none());
    }

    #[test]
    fn note_gated_on_success_check() {
        assert!(status_file_note(true).contains("APB_STATUS_FILE"));
        assert_eq!(status_file_note(false), "");
    }
}
