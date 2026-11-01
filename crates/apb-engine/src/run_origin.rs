//! The definition origin a run was started from, read from its journal's
//! `run_provenance`: what a child's `scope: auto`, the run gate's tree walk
//! of a resume and the launch check of a run never authorized resolve
//! against. Reads only the journal, so the gate and the scheduler both use
//! it without depending on each other.

use std::path::Path;

use crate::event::{EventPayload, read_all};

/// The parent run's definition origin (from its RunProvenance event), used to
/// resolve a child's `scope: auto` the same way the policy gate does (parent
/// origin first, then global). Defaults to Project when the label is absent.
pub fn parent_run_origin(run_dir: &Path) -> apb_core::scope::Origin {
    use apb_core::scope::Origin;
    let events = read_all(run_dir).unwrap_or_default();
    for e in &events {
        if let EventPayload::RunProvenance {
            origin: Some(label),
            ..
        } = &e.payload
        {
            return if label == "global" {
                Origin::Global
            } else {
                Origin::Project { workspace_id: None }
            };
        }
    }
    Origin::Project { workspace_id: None }
}
