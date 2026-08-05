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
    "report when the file is absent or invalid. ",
    // Issue #70 item 2: only the FINAL result is captured. If a turn ends on an
    // interim progress note (for example while waiting on a background poll),
    // that note is what later steps receive. The status-file outputs channel is
    // the durable fix.
    "Only your FINAL result is stored: whatever you leave in the status-file outputs ",
    "(or, as a fallback, your last textual reply) is exactly what later steps receive, ",
    "so it must be the completed result and never a mid-work progress note. If you resume ",
    "after a background wake or long wait, overwrite the status file with the final result ",
    "before your turn ends.",
);

/// The sentence appended to [`STATUS_FILE_NOTE`] for a `require_verdict` node:
/// there the verdict is not optional, and an exit without one is classified as
/// an interruption (spec 2026-08-05 section 2.2).
///
/// The recovery route is deliberately stated as "retry or fallback" rather than
/// a promised retry: an interrupted attempt consumes a retry on the ordinary
/// exit shapes, while a deadline kill or a transport failure advances the
/// fallback chain instead (spec 2.2 addendum).
pub(crate) const VERDICT_REQUIRED_NOTE: &str = concat!(
    "This step REQUIRES the verdict: if your process ends without a valid status file, ",
    "the attempt is recorded as interrupted and the engine recovers by retrying this step ",
    "or falling back to another executor, so write the file before your turn ends even when ",
    "the result is a failure.",
);

/// The note handed to a fresh attempt after a previous one ended without
/// recording a verdict (spec 2026-08-05 section 2.2, issue #71 items 3 and 5):
/// work may be partly done already, so the new attempt is pointed at the places
/// to look before redoing anything.
///
/// The wording hedges on purpose. The note also rides attempts on the NEXT
/// executor of the fallback chain, where the previous attempt may have produced
/// nothing at all (a transport failure can end an attempt that never really
/// started), so it must not assert that work exists.
pub(crate) const INTERRUPTION_NOTE: &str = concat!(
    "Interruption note: a previous attempt at this step ended without recording a verdict, ",
    "so it was probably cut off mid-work. Part of the work may already be done, or none of it. ",
    "Check for work already done - commits, branches, worktrees, written files, running ",
    "background jobs - before redoing any of it, then continue from there and record your ",
    "final verdict in the status file.",
);

/// The status-file prompt note for a node, empty when the node is told nothing
/// about the file. A `success_check` node has been told the contract since the
/// file was introduced; a `require_verdict` node is told the stronger form (the
/// verdict is mandatory, [`VERDICT_REQUIRED_NOTE`]). A plain node keeps the
/// historical report-only contract.
pub(crate) fn status_file_note(has_success_check: bool, require_verdict: bool) -> String {
    match (require_verdict, has_success_check) {
        (true, _) => format!("{STATUS_FILE_NOTE} {VERDICT_REQUIRED_NOTE}"),
        (false, true) => STATUS_FILE_NOTE.to_string(),
        (false, false) => String::new(),
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
        assert!(status_file_note(true, false).contains("APB_STATUS_FILE"));
        assert_eq!(status_file_note(false, false), "");
    }

    // Spec 2026-08-05 section 2.2: `require_verdict` alone (no success_check)
    // must still deliver the contract, and in its stronger form.
    #[test]
    fn require_verdict_delivers_the_stronger_note_without_a_success_check() {
        let note = status_file_note(false, true);
        assert!(note.contains("APB_STATUS_FILE"), "got: {note}");
        assert!(note.contains("REQUIRES the verdict"), "got: {note}");
        // M2 of the Task 5 review: the note must not promise a same-executor
        // retry for every shape - a Timeout or Transport kill is recovered by
        // advancing the fallback chain instead (spec 2.2 addendum).
        assert!(
            note.contains("recorded as interrupted"),
            "the note must state the consequence of ending without a verdict: {note}"
        );
        assert!(
            !note.contains("interrupted and retried"),
            "the note must not promise a retry for every shape: {note}"
        );
        assert!(
            note.contains("retrying this step or falling back"),
            "the note must name both recovery routes: {note}"
        );
        // The stronger note is additive: the base contract is still in there.
        assert!(note.starts_with(STATUS_FILE_NOTE), "got: {note}");
        // A success_check does not change what a require_verdict node is told.
        assert_eq!(note, status_file_note(true, true));
    }

    // The fresh attempt after an interruption is told to look for existing work
    // before redoing it (issue #71 items 3 and 5).
    #[test]
    fn interruption_note_points_at_work_already_done() {
        let note = INTERRUPTION_NOTE;
        assert!(note.contains("cut off mid-work"), "got: {note}");
        assert!(
            note.contains("commits") && note.contains("worktrees") && note.contains("files"),
            "the note must name the places to check for existing work: {note}"
        );
        // M6 of the Task 5 review: the note also rides attempts on the NEXT
        // executor of the chain, where the previous attempt may never have
        // produced anything. It must therefore hedge rather than assert that
        // work was done.
        assert!(
            note.contains("may already be done, or none of it"),
            "the note must not assert that work exists: {note}"
        );
        assert!(
            !note.contains("was cut off mid-work before it recorded"),
            "the old unconditional wording must be gone: {note}"
        );
    }

    // Issue #70 item 2: the contract text must make explicit that only the FINAL
    // result is stored, so an agent cannot end a turn on a mid-work progress note
    // and have that placeholder become the node's durable output.
    #[test]
    fn note_states_final_output_contract() {
        let note = STATUS_FILE_NOTE;
        assert!(
            note.contains("FINAL result is stored"),
            "the note must state only the final result is stored: {note}"
        );
        assert!(
            note.contains("never a mid-work progress note"),
            "the note must warn against storing an interim progress note: {note}"
        );
        assert!(
            note.contains("overwrite the status file with the final result"),
            "the note must tell the agent to overwrite with the final result after a wake: {note}"
        );
    }
}
