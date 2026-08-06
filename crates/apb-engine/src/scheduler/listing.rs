//! The run inventory: a compact summary per run directory, sorted newest
//! first. Shares the parent module's imports via `use super::*`.

use super::*;

#[derive(Debug, Serialize)]
pub struct RunSummary {
    pub run_id: String,
    pub playbook: String,
    pub status: String,
    pub started_ts: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<crate::progress::ProgressSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continued_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// The run has a drive claim and that claim's process is provably gone: the
    /// journal still reads `running` because the only thing that writes a
    /// terminal event is the drive loop that no longer exists. The single-run
    /// surfaces (`run_status`, `apb doctor --run`) have answered this for a
    /// while; a listing could not, so a killed driver looked healthy in a table.
    /// `false` also covers "no drive claim at all", which is not a problem.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub driver_dead: bool,
}

pub fn list_runs(root: &Path) -> Result<Vec<RunSummary>, EngineError> {
    let runs_dir = root.join(".apb/runs");
    let mut out = Vec::new();
    if !runs_dir.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(&runs_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let run_id = entry.file_name().to_string_lossy().to_string();
        // One corrupted/legacy run (for example, events.jsonl with old schema,
        // where `ts` was a string or a truncated record) should not crash
        // the entire listing - skip such a directory and show the rest.
        let events = match read_all(&entry.path()) {
            Ok(events) => events,
            Err(_) => continue,
        };
        if events.is_empty() {
            continue;
        }
        let (playbook, started_ts) = events
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::RunStarted { playbook, .. } => Some((playbook.clone(), e.ts)),
                _ => None,
            })
            .unwrap_or_else(|| (run_id.clone(), 0));
        let progress = crate::progress::from_run_dir(&entry.path(), &events);
        let cfg = crate::run_config::read_run_config(&entry.path()).ok();
        let parent_run = cfg.as_ref().and_then(|c| c.parent_run.clone());
        let continued_from = cfg.as_ref().and_then(|c| c.continued_from.clone());
        let superseded_by = cfg.as_ref().and_then(|c| c.superseded_by.clone());
        let driver_dead = matches!(
            crate::liveness::driver_alive(&entry.path(), &run_id),
            Some(false)
        );
        out.push(RunSummary {
            run_id,
            playbook,
            status: crate::liveness::reported_run_status(&events)
                .as_str()
                .into(),
            started_ts,
            progress,
            parent_run,
            continued_from,
            superseded_by,
            driver_dead,
        });
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.started_ts));
    Ok(out)
}
