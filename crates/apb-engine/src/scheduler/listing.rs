//! The run inventory: a compact summary per run directory, sorted newest
//! first. Shares the parent module's imports via `use super::*`.

use super::*;

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Serialize)]
pub struct RunSummary {
    pub run_id: String,
    pub playbook: String,
    pub status: String,
    pub started_ts: u128,
    /// Active milliseconds from `run_started` to the run's end (`run_finished`
    /// or `run_aborted`), paused time left out: the same duration `apb stats`
    /// counts. Absent while the run has not ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub duration_ms: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
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
    /// Events of a type this binary does not know, skipped when the journal
    /// was read: a newer apb wrote them. Zero (and absent) for a journal this
    /// binary reads in full.
    #[serde(default, skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "ts", ts(as = "Option<usize>", optional))]
    pub unknown_events: usize,
    /// The run was a trial of the playbook's candidate version (issue #192),
    /// with its verdict; absent for every other run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub candidate_trial: Option<crate::candidate::CandidateTrial>,
    /// The run's pause (issue #200): who paused it, why, since and until
    /// when, and `pausing` while a live driver drains toward a queued pause.
    /// Absent for a run that is neither paused nor pausing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub pause: Option<crate::pause::PauseInfo>,
    /// Why the run needs a person's look: a baseline regression, or
    /// findings or warnings its attempts reported. Absent when none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub needs_look: Option<crate::run_outcome::NeedsLook>,
}

/// Active time from the start to the last `run_finished` / `run_aborted`, or
/// `None` for a run that has not ended (or never recorded its start).
fn run_duration_ms(events: &[crate::event::Event], started_ts: u128) -> Option<u128> {
    if started_ts == 0 {
        return None;
    }
    let end = events.iter().rev().find_map(|e| match e.payload {
        EventPayload::RunFinished { .. } | EventPayload::RunAborted { .. } => Some(e.ts),
        _ => None,
    })?;
    Some(crate::pause::active_elapsed_ms(
        events, started_ts, end, end,
    ))
}

fn is_zero(n: &usize) -> bool {
    *n == 0
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
        // the entire listing - skip such a directory and show the rest. A
        // torn LAST line is not that: it is a line the driver is still
        // writing, and the run view reads through it.
        let Ok(view) = crate::run_view::RunView::load(&entry.path(), &run_id) else {
            continue;
        };
        if view.events.is_empty() {
            continue;
        }
        let (playbook, started_ts) = view
            .events
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::RunStarted { playbook, .. } => Some((playbook.clone(), e.ts)),
                _ => None,
            })
            .unwrap_or_else(|| (run_id.clone(), 0));
        let duration_ms = run_duration_ms(&view.events, started_ts);
        let cfg = crate::run_config::read_run_config(&entry.path()).ok();
        let parent_run = cfg.as_ref().and_then(|c| c.parent_run.clone());
        let continued_from = cfg.as_ref().and_then(|c| c.continued_from.clone());
        let superseded_by = cfg.as_ref().and_then(|c| c.superseded_by.clone());
        let driver_dead = matches!(view.driver_alive, Some(false));
        let unknown_events = view.unknown.len();
        let status = view.run_status.as_str().into();
        let candidate_trial = crate::candidate::trial_of(&entry.path(), &view.events);
        let pause = view.pause().cloned();
        let needs_look = crate::run_outcome::NeedsLook::for_run(&entry.path(), &view.events);
        let progress = view.progress;
        out.push(RunSummary {
            needs_look,
            pause,
            run_id,
            playbook,
            status,
            started_ts,
            duration_ms,
            progress,
            parent_run,
            continued_from,
            superseded_by,
            driver_dead,
            unknown_events,
            candidate_trial,
        });
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.started_ts));
    Ok(out)
}
