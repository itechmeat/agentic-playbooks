//! Bringing the defaults setup installed in line with a new binary, for the
//! people who never start the dashboard: `apb self-update` runs it in the
//! updated binary (`apb __reconcile-defaults`), and `apb mcp` runs it at
//! start. It runs only once setup was applied on this machine (the setup
//! record lists an applied plan), so a machine without setup pays one
//! missing-file read. A run step's server leaves the shared defaults to
//! the person's own processes. It never fails its caller: a problem is a
//! note.

use std::path::Path;

use apb_core::bundled::ReconcileNote;

use super::record;

/// Reconciles the default playbooks and skills when setup was applied.
pub fn reconcile_if_set_up() -> Vec<ReconcileNote> {
    if apb_core::step_env::StepCaller::from_env()
        .describe()
        .is_some()
    {
        return Vec::new();
    }
    match record::setup_dir() {
        Some(dir) => reconcile_if_set_up_in(&dir, apb_core::bundled::reconcile_defaults),
        None => Vec::new(),
    }
}

/// [`reconcile_if_set_up`] over the setup folder `dir`, running `reconcile`
/// only when its record lists an applied plan.
pub(crate) fn reconcile_if_set_up_in(
    dir: &Path,
    reconcile: impl FnOnce() -> Vec<ReconcileNote>,
) -> Vec<ReconcileNote> {
    match record::load(dir) {
        Ok(r) if r.applied.is_empty() => Vec::new(),
        Ok(_) => reconcile(),
        Err(e) => vec![ReconcileNote {
            failed: true,
            text: format!("default playbooks and skills not checked: {e}"),
        }],
    }
}
