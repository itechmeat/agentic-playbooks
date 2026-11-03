//! `playbook_stats`: the cross-run metrics `apb stats --json` prints and
//! `GET /api/stats` answers, per playbook version and node, from the run
//! journals of one workspace only. Model-free and read-only; the report is
//! built by [`apb_engine::run_stats`].

use std::path::Path;

use serde_json::Value;

use super::ToolError;

/// The stats report of the workspace at `root`: every playbook, or only
/// `playbook`; runs started at or after `since` (a date `2026-09-20` or a
/// duration back from now `30d`); `compare`, a version of `playbook`
/// against the latest other version seen.
pub fn playbook_stats(
    root: &Path,
    playbook: Option<String>,
    since: Option<&str>,
    compare: Option<String>,
) -> Result<Value, ToolError> {
    let playbook = playbook.filter(|p| !p.is_empty());
    let compare = compare.filter(|c| !c.is_empty());
    if compare.is_some() && playbook.is_none() {
        return Err(ToolError::Conflict("compare needs playbook".into()));
    }
    let since_ms = match since.filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => Some(
            apb_engine::decision::report::parse_since(s, apb_core::clock::now_ms()).ok_or_else(
                || {
                    ToolError::Conflict(
                        "since takes a date (2026-09-20) or a duration (30d, 24h)".into(),
                    )
                },
            )?,
        ),
    };
    let filter = apb_engine::run_stats::StatsFilter {
        playbook,
        since_ms,
        compare,
    };
    let (report, runs) = apb_engine::run_stats::stats_with_runs(&[root.to_path_buf()], &filter);
    let mut value = serde_json::to_value(&report).map_err(|e| ToolError::Engine(e.to_string()))?;
    // Every contributing run's redactor, so a finding location, a baseline
    // observation or a `last_run` line that echoes a secret param value
    // leaves the way `runs_list` and `run_status` redact. A redactor that
    // cannot resolve withholds the answer.
    let redaction = apb_engine::run_redact::WorkspaceRedaction::new(root);
    for (run_id, _) in &runs {
        redaction.run(run_id).redact_json(&mut value);
    }
    Ok(value)
}
