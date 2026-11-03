//! A run's result snapshots on every surface (MCP `run_results` and
//! `run_result_read`, `apb results`, the HTTP results routes), over
//! [`apb_engine::result_view`], so they agree on bytes, digests and
//! verdicts. Reading writes nothing; only [`run_results_clean`] removes
//! what no stamp and no open transaction names.
//!
//! A run is looked up only under `<root>/.apb/runs`, by a safe id. A
//! snapshot is named by its id (64 lowercase hex characters) and an entry
//! by the core grammar ([`apb_core::result_store::Citation`]); no path is
//! ever taken from a caller. Every answer passes the run redactor.

use std::path::Path;

use apb_core::result_store::{Citation, EntryId, is_snapshot_id};
use apb_engine::result_view::{self, EntryContent, LiveComparison, ResultReadError, ResultsView};
use serde_json::{Value, json};

use super::{ToolError, redacted_answer, resolve_run_dir};

/// Every stamped snapshot of `run_id` (only `node`'s, when given) and the
/// artifact refs recorded without stored bytes, redacted.
pub fn run_results(root: &Path, run_id: &str, node: Option<&str>) -> Result<Value, ToolError> {
    let dir = resolve_run_dir(root, run_id)?;
    let mut view = ResultsView::load(&dir).map_err(|e| ToolError::Engine(e.to_string()))?;
    if let Some(node) = node {
        view.rows.retain(|r| r.execution.node == node);
        view.legacy.retain(|r| r.execution.node == node);
        view.not_stored.retain(|r| r.node == node);
    }
    let answer = json!({
        "run_id": run_id,
        "rows": view.rows,
        "legacy": view.legacy,
        "not_stored": view.not_stored,
        "evidence": super::evidence::sessions_value(&dir, node),
    });
    Ok(redacted_answer(root, &dir, answer))
}

/// A citation as a caller passes it: `r:<snapshot_id>/<entry>` with an
/// optional range `#[a,b)`.
/// A package record (`...@<record id>`) is not an entry: it is read through
/// an evidence session, where its range counts in the record's text.
pub fn parse_ref(reference: &str) -> Result<Citation, ToolError> {
    let c = reference
        .parse::<Citation>()
        .map_err(|e| ToolError::Engine(format!("invalid result reference `{reference}`: {e}")))?;
    if c.record.is_some() {
        return Err(ToolError::Engine(
            "a package record reference (`@<record id>`) is read through an evidence session (run_evidence_read, apb evidence read); read the whole entry without `@<record id>`".into(),
        ));
    }
    Ok(c)
}

/// A snapshot id and an entry as two arguments (the HTTP routes).
pub fn parse_entry(snapshot_id: &str, entry: &str) -> Result<(String, EntryId), ToolError> {
    if !is_snapshot_id(snapshot_id) {
        return Err(ToolError::NotFound(format!("snapshot `{snapshot_id}`")));
    }
    let entry = entry
        .parse::<EntryId>()
        .map_err(|e| ToolError::Engine(format!("invalid entry `{entry}`: {e}")))?;
    Ok((snapshot_id.to_string(), entry))
}

fn read_error(e: ResultReadError) -> ToolError {
    match e {
        ResultReadError::NotFound => ToolError::NotFound(e.to_string()),
        ResultReadError::Corrupt | ResultReadError::Withheld => ToolError::Conflict(e.to_string()),
        ResultReadError::BadRange | ResultReadError::Io(_) => ToolError::Engine(e.to_string()),
    }
}

/// One entry of a stamped snapshot of `run_id`, re-verified on this read
/// and redacted. A range in the reference wins; else `offset` and `limit`
/// select a window of at most [`result_view::DISPLAY_CAP`] bytes. With
/// `compare_live`, the answer adds the comparison word of a workspace file
/// with its live copy (never live content).
pub fn run_result_read(
    root: &Path,
    run_id: &str,
    reference: &str,
    offset: Option<u64>,
    limit: Option<u64>,
    compare_live: bool,
) -> Result<Value, ToolError> {
    let dir = resolve_run_dir(root, run_id)?;
    let c = parse_ref(reference)?;
    let content = match c.range {
        Some((a, b)) if b.saturating_sub(a) <= result_view::DISPLAY_CAP => {
            result_view::read_entry(&dir, &c.snapshot_id, &c.entry, Some((a, b)))
        }
        Some(_) => Err(ResultReadError::BadRange),
        None => {
            result_view::read_window(&dir, &c.snapshot_id, &c.entry, offset.unwrap_or(0), limit)
        }
    }
    .map_err(read_error)?;
    let mut answer = json!({
        "run_id": run_id,
        "ref": Citation { range: None, ..c.clone() }.to_string(),
        "content": content,
    });
    if compare_live {
        let live = result_view::compare_live(&dir, &c.snapshot_id, &c.entry).map_err(read_error)?;
        answer["live"] = json!(live);
    }
    Ok(redacted_answer(root, &dir, answer))
}

/// The stored text of one entry, whole or the byte `range` a citation
/// names (the CLI's `results show`, the raw HTTP route), re-verified and
/// redacted. No display cap applies.
pub fn run_result_raw(
    root: &Path,
    run_id: &str,
    snapshot_id: &str,
    entry: &EntryId,
    range: Option<(u64, u64)>,
) -> Result<EntryContent, ToolError> {
    let dir = resolve_run_dir(root, run_id)?;
    result_view::read_entry(&dir, snapshot_id, entry, range).map_err(read_error)
}

/// How a stored workspace file compares with its live copy now: a word.
pub fn run_result_live(
    root: &Path,
    run_id: &str,
    snapshot_id: &str,
    entry: &EntryId,
) -> Result<LiveComparison, ToolError> {
    let dir = resolve_run_dir(root, run_id)?;
    result_view::compare_live(&dir, snapshot_id, entry).map_err(read_error)
}

/// Cleans the run's result store (`apb results clean`); with `dry_run`,
/// what it would remove, writing nothing. Answered as plain values.
pub fn run_results_clean(root: &Path, run_id: &str, dry_run: bool) -> Result<Value, ToolError> {
    let dir = resolve_run_dir(root, run_id)?;
    let r = apb_engine::result_clean::clean(&dir, dry_run)
        .map_err(|e| ToolError::Engine(format!("results clean: {e}")))?;
    let dead: Vec<Value> = r
        .dead
        .iter()
        .map(|d| {
            json!({
                "token": d.token,
                "outcome": match d.outcome {
                    apb_engine::result_clean::DeadOutcome::Completed => "completed",
                    apb_engine::result_clean::DeadOutcome::RolledBack => "rolled_back",
                },
            })
        })
        .collect();
    Ok(json!({
        "run_id": run_id,
        "dry_run": r.dry_run,
        "dead": dead,
        "open": r.open,
        "kept_snapshots": r.kept_snapshots,
        "removed_snapshots": r
            .removed_snapshots
            .iter()
            .map(|n| n.strip_suffix(".json").unwrap_or(n))
            .collect::<Vec<_>>(),
        "removed_objects": r.removed_objects,
        "removed_bytes": r.removed_bytes,
        "objects_unverified": r.objects_unverified,
    }))
}
