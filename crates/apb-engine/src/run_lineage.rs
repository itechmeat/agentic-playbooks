//! Links a predecessor run to a successor that continues it (issue #42 finding 10).

use std::path::Path;

use apb_core::registry::is_safe_segment;

use crate::error::EngineError;
use crate::event::{EventLog, EventPayload};
use crate::run_config::{read_run_config, write_run_config};

/// Writes `superseded_by` on the predecessor, appends lineage events on both
/// runs. The successor's `run.yaml` must already carry `continued_from`.
pub fn establish_run_lineage(
    root: &Path,
    predecessor_id: &str,
    successor_id: &str,
) -> Result<(), EngineError> {
    if !is_safe_segment(predecessor_id) || !is_safe_segment(successor_id) {
        return Err(EngineError::Invalid(
            "invalid run id in lineage link".into(),
        ));
    }
    if predecessor_id == successor_id {
        return Err(EngineError::Invalid(
            "a run cannot continue from itself".into(),
        ));
    }
    let runs = root.join(".apb/runs");
    let pred_dir = runs.join(predecessor_id);
    let succ_dir = runs.join(successor_id);
    if !pred_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{predecessor_id}`")));
    }
    if !succ_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{successor_id}`")));
    }

    let mut pred_cfg = read_run_config(&pred_dir)?;
    if let Some(ref existing) = pred_cfg.superseded_by {
        if existing == successor_id {
            return Ok(());
        }
        return Err(EngineError::Invalid(format!(
            "run `{predecessor_id}` is already superseded by `{existing}`"
        )));
    }

    let succ_cfg = read_run_config(&succ_dir)?;
    match &succ_cfg.continued_from {
        Some(from) if from == predecessor_id => {}
        Some(from) => {
            return Err(EngineError::Invalid(format!(
                "successor `{successor_id}` continues from `{from}`, not `{predecessor_id}`"
            )));
        }
        None => {
            return Err(EngineError::Invalid(format!(
                "successor `{successor_id}` has no continued_from set"
            )));
        }
    }

    pred_cfg.superseded_by = Some(successor_id.to_string());
    write_run_config(&pred_dir, &pred_cfg)?;

    let mut pred_log = EventLog::open(&pred_dir)?;
    pred_log.append(EventPayload::RunSupersededBy {
        by: successor_id.to_string(),
    })?;

    let mut succ_log = EventLog::open(&succ_dir)?;
    succ_log.append(EventPayload::RunContinuedFrom {
        from: predecessor_id.to_string(),
    })?;

    Ok(())
}

/// Refuses a `continued_from` target that is missing or already superseded.
pub fn validate_continued_from(root: &Path, predecessor_id: &str) -> Result<(), EngineError> {
    if !is_safe_segment(predecessor_id) {
        return Err(EngineError::NotFound(format!("run `{predecessor_id}`")));
    }
    let pred_dir = root.join(".apb/runs").join(predecessor_id);
    if !pred_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{predecessor_id}`")));
    }
    let pred_cfg = read_run_config(&pred_dir)?;
    if let Some(ref by) = pred_cfg.superseded_by {
        return Err(EngineError::Invalid(format!(
            "run `{predecessor_id}` is already superseded by `{by}`"
        )));
    }
    Ok(())
}
