//! What a quota pause does when its `until` passes (issue #200, design
//! 6.6): before the run resumes, one cheap probe of the executor that ran
//! out of quota. A probe that hits the quota again re-pauses the run at the
//! next schedule step without touching the node; any other outcome lets the
//! resume go ahead. The probe's outcome is journaled as a
//! `supervisor_action` marker ([`super::quota::QUOTA_PROBE_ACTION`]), an
//! event type every binary knows.
//!
//! No probe runs for a pause that is not a quota pause, for a host-mode run
//! (a host task is no provider call), for a node that has had its
//! [`super::quota::QUOTA_PAUSE_CAP`] quota pauses already (its next quota
//! failure fails it), or for a script node (scripts are never classified,
//! so they never quota-pause).

use std::path::Path;

use crate::error::EngineError;
use crate::event::{Event, EventLog, EventPayload, read_all};
use crate::failure_class::FailureKind;
use crate::scheduler::ProbeRun;

use super::PauseBy;
use super::quota::{self, PROBE_ERROR, PROBE_PASSED, PROBE_QUOTA, QUOTA_PROBE_ACTION};

/// What [`probe_before_wake`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeProbe {
    /// The pause is not a quota pause: resume without a probe.
    NotQuota,
    /// A quota pause, but no probe applies (see the module docs); the
    /// reason. Resume.
    Skipped(String),
    /// The probe answered, or failed for a reason other than quota. Resume.
    Passed,
    /// The probe hit the quota again: the run was paused anew by the
    /// `run_paused` at journal sequence `seq`, until `until`.
    Repaused { seq: u64, until: u64 },
    /// The pause at the given sequence no longer holds the run.
    Superseded,
}

/// The `(agent, model)` of the run's pause at journal sequence `seq` when it
/// is a quota pause the engine took.
fn quota_executor(events: &[Event], seq: u64) -> Option<(String, String)> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::RunPaused {
            reason,
            by: Some(PauseBy::Engine),
            ..
        } if e.seq == seq => quota::parse_quota_reason(reason),
        _ => None,
    })
}

/// Whether the run is paused by the `run_paused` at journal sequence `seq`.
fn holds(events: &[Event], seq: u64) -> bool {
    crate::state::RunState::fold(events).run_status == crate::state::RunStatus::Paused
        && events
            .iter()
            .rev()
            .find(|e| matches!(e.payload, EventPayload::RunPaused { .. }))
            .map(|e| e.seq)
            == Some(seq)
}

/// Probes the executor of the quota pause at journal sequence `seq` of run
/// `run_id`, and journals the outcome (see [`WakeProbe`]).
pub fn probe_before_wake(root: &Path, run_id: &str, seq: u64) -> Result<WakeProbe, EngineError> {
    let run_dir = root.join(".apb/runs").join(run_id);
    let events = read_all(&run_dir)?;
    if !holds(&events, seq) {
        return Ok(WakeProbe::Superseded);
    }
    let Some((agent, model)) = quota_executor(&events, seq) else {
        return Ok(WakeProbe::NotQuota);
    };
    let Some(node) = quota::quota_node(&events) else {
        return Ok(WakeProbe::Skipped("no quota-paused node".into()));
    };
    let prior = quota::quota_pauses(&events, &node);
    if prior >= quota::QUOTA_PAUSE_CAP {
        return Ok(WakeProbe::Skipped(format!(
            "node `{node}` had {prior} quota pauses already"
        )));
    }
    let run = crate::scheduler::run_quota_probe(&run_dir, &node, &agent, &model);
    let (detail, repause) = match &run {
        ProbeRun::Unavailable(why) => return Ok(WakeProbe::Skipped(why.clone())),
        ProbeRun::Answered => (format!("{PROBE_PASSED}: `{agent}/{model}` answered"), None),
        ProbeRun::Failed(msg) if crate::failure_class::classify(msg) == FailureKind::Quota => {
            let now = apb_core::clock::now_ms();
            let until = quota::quota_until(crate::failure_class::retry_after(msg, now), prior, now);
            (
                format!(
                    "{PROBE_QUOTA}: `{agent}/{model}` is still out of quota: {}",
                    msg.trim()
                ),
                Some(until),
            )
        }
        ProbeRun::Failed(msg) => (
            format!("{PROBE_ERROR}: `{agent}/{model}` failed: {}", msg.trim()),
            None,
        ),
    };
    // Serialized against `stop_run` and `request_pause`; the pause must
    // still be the one this probe was for.
    let _lock = apb_core::fsutil::lock_dir(&run_dir, crate::stop::EVENT_LOCK).ok();
    if !holds(&read_all(&run_dir)?, seq) {
        return Ok(WakeProbe::Superseded);
    }
    let mut log = EventLog::open(&run_dir)?;
    log.append(EventPayload::SupervisorAction {
        action: QUOTA_PROBE_ACTION.into(),
        node: Some(node),
        detail,
    })?;
    Ok(match repause {
        Some(until) => {
            let seq = log
                .append(quota::quota_pause(&agent, &model, until).to_event())?
                .seq;
            WakeProbe::Repaused { seq, until }
        }
        None => WakeProbe::Passed,
    })
}
