//! Read-only lookups of an existing run by id: the playbook and the active
//! version it runs, and the origin it was started under.

use std::path::Path;

use apb_core::profile_store::PlaybookOrigin;
use apb_core::registry::is_safe_segment;

use crate::error::EngineError;
use crate::event::{EventPayload, read_all};

/// Returns (playbook id, run's active version). The active version is
/// the latest `RunMigrated.to_version`; if there were no migrations - `RunStarted.version`.
/// Used by the `playbook_patch` tool to choose the base of the patch version.
pub fn run_playbook_ref(root: &Path, run_id: &str) -> Result<(String, String), EngineError> {
    if !is_safe_segment(run_id) {
        return Err(EngineError::NotFound(run_id.to_string()));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(run_id.to_string()));
    }
    let events = read_all(&run_dir)?;
    let mut id: Option<String> = None;
    let mut version: Option<String> = None;
    for event in &events {
        match &event.payload {
            EventPayload::RunStarted {
                playbook,
                version: v,
            } => {
                id = Some(playbook.clone());
                version = Some(v.clone());
            }
            EventPayload::RunMigrated { to_version, .. } => {
                version = Some(to_version.clone());
            }
            _ => {}
        }
    }
    match (id, version) {
        (Some(id), Some(version)) => Ok((id, version)),
        _ => Err(EngineError::NotFound(format!(
            "run `{run_id}` has no RunStarted event"
        ))),
    }
}

/// The origin (project vs global) a run was started under, from its provenance.
/// Used by the MCP rebind gate to resolve and trust-check a new profile the same
/// way run start resolved the run's node profiles (issue #45 finding 5).
pub fn run_profile_origin(root: &Path, run_id: &str) -> Result<PlaybookOrigin, EngineError> {
    if !is_safe_segment(run_id) {
        return Err(EngineError::NotFound(run_id.to_string()));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(run_id.to_string()));
    }
    super::rebind::run_origin(&run_dir)
}
