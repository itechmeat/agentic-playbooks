//! The history of `current` (`current-history.jsonl`): one line per move
//! or aborted move, each naming the sha256 of the line before it, and the
//! audit that reads it back by value.

use std::io;
use std::path::Path;

use super::model::{EntryKind, History, HistoryEntry};
use crate::content::sha256_hex;

/// The history file next to `current`.
pub const HISTORY_FILE: &str = "current-history.jsonl";

fn read_text(playbook_dir: &Path) -> io::Result<String> {
    match std::fs::read_to_string(playbook_dir.join(HISTORY_FILE)) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e),
    }
}

fn lines(text: &str) -> Vec<&str> {
    text.lines().filter(|l| !l.trim().is_empty()).collect()
}

/// Appends `entry` with its `prev`, unless a line with its `change_id` is
/// already there. The file is rewritten whole (temp and rename), so no
/// line is ever left half written.
pub(super) fn append_once(playbook_dir: &Path, entry: &HistoryEntry) -> io::Result<()> {
    let text = read_text(playbook_dir)?;
    let existing = lines(&text);
    let recorded = existing.iter().any(|l| {
        serde_json::from_str::<HistoryEntry>(l).is_ok_and(|e| e.change_id == entry.change_id)
    });
    if recorded {
        return Ok(());
    }
    let mut line = entry.clone();
    line.prev = existing.last().map(|l| sha256_hex(l.as_bytes()));
    let mut out = String::with_capacity(text.len() + 256);
    for l in &existing {
        out.push_str(l);
        out.push('\n');
    }
    out.push_str(&serde_json::to_string(&line).map_err(io::Error::other)?);
    out.push('\n');
    crate::fsutil::atomic_write(&playbook_dir.join(HISTORY_FILE), out.as_bytes())
}

/// What the pointers say beside the history, for its audit.
pub(super) struct Pointers {
    /// The version `current` names now.
    pub current: Option<String>,
    /// An interrupted move waits.
    pub pending: bool,
    /// Its target, when its intent reads.
    pub pending_to: Option<String>,
}

/// The newest `limit` lines and the audit against `pointers`. Reads only.
pub(super) fn read(playbook_dir: &Path, limit: usize, pointers: Pointers) -> io::Result<History> {
    let text = read_text(playbook_dir)?;
    let mut entries = Vec::new();
    let mut incomplete = false;
    let mut prev: Option<String> = None;
    let mut last_to: Option<String> = None;
    for line in lines(&text) {
        match serde_json::from_str::<HistoryEntry>(line) {
            Ok(e) => {
                incomplete |= e.prev != prev;
                if e.kind == EntryKind::Move {
                    incomplete |= e.to.is_none()
                        || last_to.as_ref().is_some_and(|t| e.from.as_ref() != Some(t));
                    last_to = e.to.clone();
                }
                entries.push(e);
            }
            Err(_) => incomplete = true,
        }
        prev = Some(sha256_hex(line.as_bytes()));
    }
    if let Some(last) = last_to {
        // An interrupted move explains a `current` at its own target.
        let explained = pointers.pending_to.is_some() && pointers.current == pointers.pending_to;
        incomplete |= pointers.current.as_ref() != Some(&last) && !explained;
    }
    entries.reverse();
    entries.truncate(limit);
    Ok(History {
        entries,
        incomplete,
        pending: pointers.pending,
    })
}
