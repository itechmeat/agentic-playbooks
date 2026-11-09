//! Whether a run is flagged, read from its run directory alone: a leaf the
//! readers of earlier runs (the decisions report among them) can call
//! without depending on the rest of [`super`].

use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::event::{Event, EventPayload};
use crate::manifest::RunOrigin;

/// How many leading journal lines [`is_flagged`] reads for `run_started`.
const HEAD_LINES: usize = 8;

/// Whether the run's journal names the flag on its `run_started`.
pub(super) fn journal_flag(run_dir: &Path) -> bool {
    let Ok(file) = std::fs::File::open(run_dir.join("events.jsonl")) else {
        return false;
    };
    BufReader::new(file)
        .lines()
        .take(HEAD_LINES)
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Event>(&l).ok())
        .find_map(|e| match e.payload {
            EventPayload::RunStarted { external_data, .. } => Some(external_data),
            _ => None,
        })
        .unwrap_or(false)
}

/// Whether a manifest marks its run: the boolean, the origin or the mode
/// suffix.
pub(super) fn manifest_flag(m: &crate::manifest::RunExecutionManifest) -> bool {
    m.external_data || m.origin == Some(RunOrigin::ExternalData) || m.is_external()
}

/// Whether the run in `run_dir` is an external data run: any source says
/// so. A manifest that does not read leaves the journal to tell; such a
/// run never [`super::learns_from`] either way.
pub fn is_flagged(run_dir: &Path) -> bool {
    let manifest = crate::manifest::read(run_dir)
        .ok()
        .flatten()
        .is_some_and(|m| manifest_flag(&m));
    manifest || journal_flag(run_dir)
}
