//! The comparable runs of a target: the newest [`ESTIMATE_WINDOW`] runs of
//! the same playbook id, version and execution mode, among the newest
//! [`ESTIMATE_SCAN`] run directories of the project (a long history is not
//! read in full, see [`crate::run_history`]).

use std::path::PathBuf;

use super::types::{ESTIMATE_SCAN, ESTIMATE_WINDOW, EstimateTarget};
use crate::run_stats::StatsRun;

/// The comparable runs, newest first: the newest [`ESTIMATE_WINDOW`] runs
/// of the target's playbook id, version and execution mode (the target's
/// own run left out; a run whose mode this apb does not know is left out
/// too).
pub(super) fn comparable(roots: &[PathBuf], target: &EstimateTarget) -> Vec<(PathBuf, StatsRun)> {
    crate::run_history::runs_of(
        roots,
        &target.playbook.id,
        Some(&target.version),
        ESTIMATE_SCAN,
    )
    .into_iter()
    .filter(|d| {
        target
            .exclude_run
            .as_deref()
            .is_none_or(|own| d.file_name().and_then(|n| n.to_str()) != Some(own))
    })
    .filter_map(|d| {
        let run = StatsRun::load(&d)?;
        (run.execution.as_deref() == Some(target.execution.as_str())).then_some((d, run))
    })
    .take(ESTIMATE_WINDOW)
    .collect()
}
