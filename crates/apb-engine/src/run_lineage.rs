//! Links a predecessor run to a successor that continues it (issue #42 finding 10).

use std::path::Path;

use apb_core::registry::is_safe_segment;

use crate::error::EngineError;
use crate::event::{EventLog, EventPayload, read_all};
use crate::legacy_snapshot::load_run_playbook;
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
        return Err(EngineError::Conflict(format!(
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

/// Resolves the playbook id a predecessor run belongs to (snapshot first, then
/// the `RunStarted` event).
fn predecessor_playbook_id(pred_dir: &Path) -> Result<String, EngineError> {
    if let Some(pb) = load_run_playbook(pred_dir) {
        return Ok(pb.id);
    }
    let events = read_all(pred_dir)?;
    events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunStarted { playbook, .. } => Some(playbook.clone()),
            _ => None,
        })
        .ok_or_else(|| {
            EngineError::Invalid(format!(
                "run `{}` has no playbook identity",
                pred_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("<unknown>")
            ))
        })
}

/// Refuses a `continued_from` target that is missing, already superseded, or
/// belongs to a different playbook than the run being started.
pub fn validate_continued_from(
    root: &Path,
    predecessor_id: &str,
    playbook_id: &str,
) -> Result<(), EngineError> {
    if !is_safe_segment(predecessor_id) {
        return Err(EngineError::NotFound(format!("run `{predecessor_id}`")));
    }
    let pred_dir = root.join(".apb/runs").join(predecessor_id);
    if !pred_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{predecessor_id}`")));
    }
    let pred_playbook = predecessor_playbook_id(&pred_dir)?;
    if pred_playbook != playbook_id {
        return Err(EngineError::Invalid(format!(
            "continued_from run `{predecessor_id}` belongs to playbook `{pred_playbook}`, not `{playbook_id}`"
        )));
    }
    let pred_cfg = read_run_config(&pred_dir)?;
    if let Some(ref by) = pred_cfg.superseded_by {
        return Err(EngineError::Conflict(format!(
            "run `{predecessor_id}` is already superseded by `{by}`"
        )));
    }
    Ok(())
}
