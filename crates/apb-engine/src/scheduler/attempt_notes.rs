//! Journals an attempt's findings and warnings (`attempt_notes`), read from
//! its status file once the attempt ended and before any outcome branch
//! journals its `attempt_finished`. Nothing else reads them during the run:
//! they never touch the node's status, its outputs, the edges, the context
//! or a completion check.

use super::journal::Journal;
use super::status_file::StatusFileDoc;
use crate::error::EngineError;
use crate::event::EventPayload;
use apb_core::run_notes::DroppedNotes;

/// Appends one `attempt_notes` when the status file carried any finding or
/// warning, or either it or a host submission (`host_dropped`, carried by
/// the attempt's report) dropped malformed ones; nothing otherwise.
pub(crate) fn journal(
    journal: &Journal<'_>,
    node: &str,
    attempt: u32,
    doc: Option<&StatusFileDoc>,
    host_dropped: Option<&DroppedNotes>,
) -> Result<(), EngineError> {
    let mut s = doc.map(StatusFileDoc::notes).unwrap_or_default();
    if let Some(d) = host_dropped {
        s.absorb(d);
    }
    if s.is_empty() {
        return Ok(());
    }
    journal.append(EventPayload::AttemptNotes {
        node: node.to_string(),
        attempt,
        findings: s.notes.findings,
        warnings: s.notes.warnings,
        dropped: s.dropped,
        dropped_reasons: s.reasons,
    })
}
