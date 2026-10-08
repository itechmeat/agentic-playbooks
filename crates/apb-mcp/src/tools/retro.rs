//! `run_retro_context` (issue #192 part 3): the retrospective numbers of one
//! run for whoever improves the playbook. The report itself is built by
//! [`apb_engine::run_retro`]; this layer only resolves the run.

use std::path::Path;

use serde_json::Value;

use super::{ToolError, resolve_run_dir};

/// The retrospective of `run_id`, compared with the last `compare_last`
/// finished runs of the same version (default
/// [`apb_engine::run_retro::DEFAULT_COMPARE_LAST`], at most
/// [`apb_engine::run_retro::MAX_COMPARE_LAST`], `0` for no comparison).
pub fn run_retro_context(
    root: &Path,
    run_id: &str,
    compare_last: Option<usize>,
) -> Result<Value, ToolError> {
    let dir = resolve_run_dir(root, run_id)?;
    let n = compare_last
        .unwrap_or(apb_engine::run_retro::DEFAULT_COMPARE_LAST)
        .min(apb_engine::run_retro::MAX_COMPARE_LAST);
    let report = apb_engine::run_retro::retro(root, &dir, n)?;
    serde_json::to_value(report).map_err(|e| ToolError::Engine(e.to_string()))
}
