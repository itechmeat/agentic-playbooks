//! Per-attempt status-file protocol for `agent_task` nodes (subtask S2).
//!
//! Each attempt is handed an `APB_STATUS_FILE` env var pointing at a per-attempt
//! JSON file in the run directory. The agent MAY write its final verdict there
//! as `{"status": "success"|"failure", "outputs": { ... }}`. The engine reads
//! that file FIRST when deciding the attempt's status and outputs, and falls
//! back to parsing the agent's textual report when the file is absent,
//! unreadable, invalid, or carries an unrecognized status. The read stays total
//! and panic-free so a malformed file only means "fall back", never a crash.
//! The same file may carry `findings` and `warnings` ([`NOTES_NOTE`]), read
//! independently of the verdict.

use std::path::Path;

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

/// The paragraph appended to every fresh `agent_task` attempt: how to report
/// a problem the attempt saw but did not fix, and what a person should
/// check. Appended at attempt time, outside the cache-keyed prompt, like
/// [`INTERRUPTION_NOTE`]. Notes are reported only: they never change the
/// step's result.
pub(crate) const NOTES_NOTE: &str = concat!(
    "Findings and warnings: if you notice a problem you did not fix (out of scope, not yours ",
    "to change, or too risky), you may list it in the JSON status file named by APB_STATUS_FILE ",
    "as \"findings\": [{\"kind\": \"bug\", \"location\": \"path or record\", ",
    "\"evidence\": \"what you saw\", \"why_not_fixed\": \"why you left it\"}], and anything ",
    "a person should check as \"warnings\": [{\"subject\": \"what\", \"reason\": \"why\"}]. ",
    "Both are optional, at most 20 each, and reported to the person only: they never change ",
    "this step's result and never replace the `status` field.",
);

/// Appended after [`NOTES_NOTE`] only for a playbook with a
/// `known_problems:` block, so every other prompt stays byte-stable: how to
/// report trouble with a tool so later runs of the playbook are told.
pub(crate) const TOOL_PROBLEM_NOTE: &str = concat!(
    " Tool problems: if a tool you used misbehaved, report it as a finding with ",
    "\"kind\": \"tool_problem\", a \"location\" naming the tool as connector:<name>.<function>, ",
    "mcp:<server>.<tool>, cli:<program>, skill:<name>, path:<repo-relative path> or node:<id>, ",
    "and, when you got around it, a \"workaround\" saying how. Later runs of this playbook ",
    "are shown problems that recur.",
);

/// The findings paragraph of a fresh attempt: [`NOTES_NOTE`], with
/// [`TOOL_PROBLEM_NOTE`] when the playbook collects known problems.
pub(crate) fn notes_note(known_problems: bool) -> String {
    if known_problems {
        format!("{NOTES_NOTE}{TOOL_PROBLEM_NOTE}")
    } else {
        NOTES_NOTE.to_string()
    }
}

/// The status-file prompt note for a node, empty when the node is told nothing
/// about the file. A `success_check` node has been told the contract since the
/// file was introduced; a `require_verdict` node is told the stronger form (the
/// verdict is mandatory, [`VERDICT_REQUIRED_NOTE`]). A plain node keeps the
/// historical report-only contract.
///
/// Declared output `fields` (issue #67 item 4) deliver the note too, followed
/// by the exact keys the `outputs` object must carry: they are how the node
/// publishes its named values, and the status file is the channel for them.
pub(crate) fn status_file_note(
    has_success_check: bool,
    require_verdict: bool,
    fields: &[String],
) -> String {
    let mut note = match (require_verdict, has_success_check || !fields.is_empty()) {
        (true, _) => format!("{STATUS_FILE_NOTE} {VERDICT_REQUIRED_NOTE}"),
        (false, true) => STATUS_FILE_NOTE.to_string(),
        (false, false) => return String::new(),
    };
    if !fields.is_empty() {
        let keys: Vec<String> = fields.iter().map(|f| format!("`{f}`")).collect();
        note.push_str(&format!(
            " This step publishes named outputs: write the status file with an outputs object \
             that carries these keys, each a string (or a number or boolean): {}. \
             Later steps read them by name.",
            keys.join(", ")
        ));
    }
    note
}

/// The engine's interpretation of a per-attempt status file: the reported
/// status plus, when the file carried a non-empty `outputs` object, its compact
/// JSON string to become the node's downstream output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusFileReport {
    pub status: NodeStatus,
    pub outputs: Option<String>,
}

/// One read of an attempt's status file: the verdict fields and the notes,
/// parsed once. The file is a JSON object; anything else reads as absent.
/// [`StatusFileDoc::verdict`] and [`StatusFileDoc::notes`] are independent,
/// so a file carrying only findings and warnings still delivers them while
/// the verdict falls back to the textual report.
#[derive(Debug, Clone, Default)]
pub(crate) struct StatusFileDoc {
    status: Option<String>,
    outputs: Option<serde_json::Value>,
    findings: serde_json::Value,
    warnings: serde_json::Value,
}

/// Drop counts in the file's shape. The subagent writes this file, so a
/// host's counts never travel through it (they ride the attempt's report):
/// these keys are never read, and a host merge removes any it finds.
const DROPPED_KEYS: [&str; 2] = ["notes_dropped", "notes_dropped_reasons"];

impl StatusFileDoc {
    /// Reads `path`; `None` when it is missing, unreadable or not a JSON
    /// object.
    pub(crate) fn read(path: &Path) -> Option<Self> {
        let raw = std::fs::read_to_string(path).ok()?;
        let serde_json::Value::Object(mut map) = serde_json::from_str(&raw).ok()? else {
            return None;
        };
        Some(StatusFileDoc {
            status: map
                .remove("status")
                .and_then(|v| v.as_str().map(str::to_string)),
            outputs: map.remove("outputs"),
            findings: map.remove("findings").unwrap_or_default(),
            warnings: map.remove("warnings").unwrap_or_default(),
        })
    }

    /// The verdict, `Some` only for a recognized `status`
    /// (`"success"`/`"failure"`); every other case falls back to the
    /// textual report. A non-empty `outputs` object is serialized to compact
    /// JSON; anything else (absent, empty, or a non-object value) yields
    /// `None` outputs, keeping the agent's parsed output.
    pub(crate) fn verdict(&self) -> Option<StatusFileReport> {
        let status = match self.status.as_deref()? {
            "success" => NodeStatus::Succeeded,
            "failure" => NodeStatus::Failed,
            _ => return None,
        };
        let outputs = self
            .outputs
            .as_ref()
            .and_then(|v| v.as_object())
            .filter(|o| !o.is_empty())
            .and_then(|o| serde_json::to_string(o).ok());
        Some(StatusFileReport { status, outputs })
    }

    /// The findings and warnings, sanitized.
    pub(crate) fn notes(&self) -> apb_core::run_notes::SanitizedNotes {
        apb_core::run_notes::sanitize(&self.findings, &self.warnings)
    }
}

/// What a host submitted for an attempt's notes: the well-formed entries.
/// What it dropped travels with the attempt's report, not the file.
pub(crate) struct HostNotes<'a> {
    pub findings: &'a [apb_core::run_notes::Finding],
    pub warnings: &'a [apb_core::run_notes::NoteWarning],
}

impl HostNotes<'_> {
    fn is_empty(&self) -> bool {
        self.findings.is_empty() && self.warnings.is_empty()
    }
}

/// Records a host submission in an attempt's status file (host mode): the
/// submission's `verdict` unless the subagent wrote a valid status of its
/// own (which may carry named outputs and then wins; without one, its
/// leftover `outputs` are dropped so the submission's output stands), and
/// the submitted findings and warnings appended to the file's own. A value under `findings` or
/// `warnings` that is not a list is kept as an entry, so it still counts as
/// dropped. A file that exists but is not a JSON object is the subagent's:
/// it is left alone, and an error says submitted notes could not join it.
pub(crate) fn record_host_submission(
    path: &Path,
    verdict: &str,
    notes: &HostNotes<'_>,
) -> std::io::Result<()> {
    let mut map = match std::fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(serde_json::Value::Object(map)) => map,
            _ if notes.is_empty() => return Ok(()),
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "the subagent's status file is not a JSON object, so the submitted findings and warnings were not recorded",
                ));
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(e) => return Err(e),
    };
    let has_verdict = matches!(
        map.get("status").and_then(|v| v.as_str()),
        Some("success" | "failure")
    );
    if has_verdict && notes.is_empty() {
        return Ok(());
    }
    if !has_verdict {
        map.remove("outputs");
        map.insert("status".into(), verdict.into());
    }
    let mut append = |key: &str, items: Vec<serde_json::Value>| {
        if items.is_empty() {
            return;
        }
        match map.remove(key) {
            Some(serde_json::Value::Array(mut have)) => {
                have.extend(items);
                map.insert(key.into(), serde_json::Value::Array(have));
            }
            Some(serde_json::Value::Null) | None => {
                map.insert(key.into(), serde_json::Value::Array(items));
            }
            Some(other) => {
                let mut all = vec![other];
                all.extend(items);
                map.insert(key.into(), serde_json::Value::Array(all));
            }
        }
    };
    append(
        "findings",
        notes
            .findings
            .iter()
            .filter_map(|f| serde_json::to_value(f).ok())
            .collect(),
    );
    append(
        "warnings",
        notes
            .warnings
            .iter()
            .filter_map(|w| serde_json::to_value(w).ok())
            .collect(),
    );
    for key in DROPPED_KEYS {
        map.remove(key);
    }
    let bytes =
        serde_json::to_vec(&serde_json::Value::Object(map)).map_err(std::io::Error::other)?;
    apb_core::fsutil::atomic_write_private(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_status_file(path: &Path) -> Option<StatusFileReport> {
        StatusFileDoc::read(path)?.verdict()
    }

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
    fn notes_are_read_independently_of_the_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(
            dir.path(),
            r#"{"findings":[{"kind":"bug","location":"a.rs:1","evidence":"e","why_not_fixed":"w"}]}"#,
        );
        let doc = StatusFileDoc::read(&p).expect("an object parses");
        assert!(doc.verdict().is_none());
        assert_eq!(doc.notes().notes.findings.len(), 1);
        let p = write(
            dir.path(),
            r#"{"status":"success","warnings":[{"subject":"s","reason":"r"}, 7]}"#,
        );
        let doc = StatusFileDoc::read(&p).unwrap();
        assert_eq!(doc.verdict().unwrap().status, NodeStatus::Succeeded);
        let notes = doc.notes();
        assert_eq!((notes.notes.warnings.len(), notes.dropped), (1, 1));
    }

    fn host_finding() -> Vec<apb_core::run_notes::Finding> {
        vec![apb_core::run_notes::Finding {
            kind: "bug".into(),
            location: "a.rs".into(),
            evidence: "e".into(),
            why_not_fixed: "w".into(),
            workaround: None,
        }]
    }

    #[test]
    fn a_subagents_malformed_list_still_counts_as_dropped_after_a_host_merge() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(
            dir.path(),
            r#"{"findings":{"kind":"bug"},"outputs":{"x":"leftover"}}"#,
        );
        let findings = host_finding();
        let notes = HostNotes {
            findings: &findings,
            warnings: &[],
        };
        record_host_submission(&p, "success", &notes).unwrap();
        let doc = StatusFileDoc::read(&p).unwrap();
        let verdict = doc.verdict().unwrap();
        assert_eq!(verdict.status, NodeStatus::Succeeded);
        // Without a verdict of its own, the subagent's outputs do not win.
        assert_eq!(verdict.outputs, None);
        let n = doc.notes();
        // The host's finding is kept; the subagent's malformed object counts.
        assert_eq!((n.notes.findings.len(), n.dropped), (1, 1));
    }

    #[test]
    fn drop_counts_a_subagent_writes_into_the_file_are_never_read() {
        let dir = tempfile::tempdir().unwrap();
        let planted =
            r#"{"status":"success","notes_dropped":7,"notes_dropped_reasons":["forged"]}"#;
        let p = write(dir.path(), planted);
        let n = StatusFileDoc::read(&p).unwrap().notes();
        assert_eq!((n.dropped, n.reasons.len()), (0, 0));
        // A host merge removes them from the file.
        let findings = host_finding();
        let notes = HostNotes {
            findings: &findings,
            warnings: &[],
        };
        record_host_submission(&p, "success", &notes).unwrap();
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(!raw.contains("notes_dropped"), "{raw}");
    }

    #[test]
    fn notes_that_cannot_join_a_non_object_file_are_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "[1, 2]");
        let findings = host_finding();
        let notes = HostNotes {
            findings: &findings,
            warnings: &[],
        };
        assert!(record_host_submission(&p, "success", &notes).is_err());
        let empty = HostNotes {
            findings: &[],
            warnings: &[],
        };
        assert!(record_host_submission(&p, "success", &empty).is_ok());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "[1, 2]");
    }

    #[test]
    fn the_notes_note_never_offers_a_file_without_a_status_as_a_verdict() {
        // A require_verdict node fails an attempt whose file has no status,
        // so the notes paragraph must not suggest one.
        assert!(!NOTES_NOTE.contains("textual report"));
        assert!(NOTES_NOTE.contains("never replace the `status` field"));
        assert_eq!(notes_note(false), NOTES_NOTE);
        let with = notes_note(true);
        assert!(with.starts_with(NOTES_NOTE) && with.contains("\"kind\": \"tool_problem\""));
    }

    #[test]
    fn note_gated_on_success_check() {
        assert!(status_file_note(true, false, &[]).contains("APB_STATUS_FILE"));
        assert_eq!(status_file_note(false, false, &[]), "");
    }

    // Spec 2026-08-05 section 2.2: `require_verdict` alone (no success_check)
    // must still deliver the contract, and in its stronger form.
    #[test]
    fn require_verdict_delivers_the_stronger_note_without_a_success_check() {
        let note = status_file_note(false, true, &[]);
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
        assert_eq!(note, status_file_note(true, true, &[]));
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
