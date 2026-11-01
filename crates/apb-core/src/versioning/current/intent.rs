//! The intent of a move (`current.intent.json`): written before the first
//! change, deleted after the last, so a crash in between leaves what
//! recovery needs to finish or undo it.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::model::{HistoryEntry, MovedBy, TrialFinal};

/// The intent file next to `current`.
pub const INTENT_FILE: &str = "current.intent.json";

/// A move in progress.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct Intent {
    pub change_id: String,
    pub from: Option<String>,
    pub to: String,
    pub by: MovedBy,
    /// The candidate the move ends as superseded.
    pub supersede: Option<String>,
    /// Set when the target is the candidate: its pointer is cleared and
    /// its trial recorded as promoted after `current` moved.
    pub finish_candidate: Option<FinishCandidate>,
    /// The line the history gets, `prev` filled in when it is appended.
    pub history_line: HistoryEntry,
}

/// The candidate a move promotes, with its final trial record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct FinishCandidate {
    pub version: String,
    pub trial: TrialFinal,
}

pub(super) fn write(playbook_dir: &Path, intent: &Intent) -> io::Result<()> {
    let json = serde_json::to_vec(intent).map_err(io::Error::other)?;
    crate::fsutil::atomic_write_private(&playbook_dir.join(INTENT_FILE), &json)
}

/// The intent waiting in `playbook_dir`. One that does not read is an
/// error: it was written whole, so only a hand edit breaks it, and moving
/// on without it could leave a move half done.
pub(super) fn read(playbook_dir: &Path) -> io::Result<Option<Intent>> {
    let bytes = match std::fs::read(playbook_dir.join(INTENT_FILE)) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{INTENT_FILE} does not read ({e}): check `current` and remove it by hand"),
        )
    })
}

pub(super) fn remove(playbook_dir: &Path) -> io::Result<()> {
    crate::process_mode::guard()?;
    match std::fs::remove_file(playbook_dir.join(INTENT_FILE)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}
