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

use super::quota::{self, PROBE_ERROR, PROBE_PASSED, PROBE_QUOTA, QUOTA_PROBE_ACTION};
use super::{PauseBy, holds};

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

/// The checks a resume makes before anything of the run executes, applied
/// to the wake-up probe as well, which runs an executor of the run's
/// manifest: a run directory this machine created (the run gate, the same
/// call the waker's resume makes) and an executor binary unchanged since the
/// run started (environment drift).
fn verify_before_probe(root: &Path, run_id: &str, run_dir: &Path) -> Result<(), EngineError> {
    crate::gate::check_resume(root, run_id, true)
        .map_err(|v| EngineError::Conflict(v.to_string()))?;
    crate::scheduler::check_environment_drift(run_dir, false)?;
    Ok(())
}

/// Bytes of a probe failure message kept at its start and at its end.
const MESSAGE_HEAD: usize = 400;
const MESSAGE_TAIL: usize = 200;

/// A probe failure message as the journal keeps it: secrets, tokens and
/// personal paths redacted the way a decision request is (the connector
/// secrets of the workspace at `root`), on one line, and clipped.
fn journal_message(root: &Path, msg: &str) -> String {
    let secrets = apb_core::connector::resolve::all_referenced_env_names(root)
        .iter()
        .filter_map(|n| apb_core::connector::secrets::resolve_var(root, n))
        .collect();
    let redacted = crate::decision::redact::Redactor::new(secrets, root).redact(msg);
    let one_line = redacted.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::decision::redact::clip(&one_line, MESSAGE_HEAD, MESSAGE_TAIL).replace('\n', " ")
}

/// Probes the executor of the quota pause at journal sequence `seq` of run
/// `run_id`, and journals the outcome (see [`WakeProbe`]). The run origin
/// and environment drift are verified first, so a run a resume would refuse
/// is never probed: that refusal is the `Err`, and so is an outcome that
/// could not be journaled ([`journal_outcome`]).
pub fn probe_before_wake(root: &Path, run_id: &str, seq: u64) -> Result<WakeProbe, EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
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
    verify_before_probe(root, run_id, &run_dir)?;
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
                    journal_message(root, msg)
                ),
                Some(until),
            )
        }
        ProbeRun::Failed(msg) => (
            format!(
                "{PROBE_ERROR}: `{agent}/{model}` failed: {}",
                journal_message(root, msg)
            ),
            None,
        ),
    };
    journal_outcome(&run_dir, seq, &node, &agent, &model, detail, repause)
}

/// How many times the probe's outcome is offered to the run's event lock
/// (each try waits out the lock's own timeout) before the waker gives up.
const JOURNAL_TRIES: u32 = 30;

/// Journals the outcome of a probe that ran: the marker and, for a quota
/// that still holds, the new pause. Serialized against `stop_run` and
/// `request_pause` under the run's event lock, and only while the pause at
/// `seq` still holds the run. The probe is never run again for this: a lock
/// that stays busy is retried up to [`JOURNAL_TRIES`] times, and any other
/// lock error, or a lock still busy after that, is a refusal (the run stays
/// paused for `apb resume` or `apb resume --all`), never a resume on an
/// outcome nobody recorded.
fn journal_outcome(
    run_dir: &Path,
    seq: u64,
    node: &str,
    agent: &str,
    model: &str,
    detail: String,
    repause: Option<u64>,
) -> Result<WakeProbe, EngineError> {
    let mut tries = 0;
    let _lock = loop {
        match apb_core::fsutil::lock_dir(run_dir, crate::stop::EVENT_LOCK) {
            Ok(lock) => break lock,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && tries + 1 < JOURNAL_TRIES => {
                tries += 1;
                if !holds(&read_all(run_dir)?, seq) {
                    return Ok(WakeProbe::Superseded);
                }
            }
            Err(e) => {
                return Err(EngineError::Conflict(format!(
                    "the outcome of the wake-up probe could not be journaled: {e}"
                )));
            }
        }
    };
    if !holds(&read_all(run_dir)?, seq) {
        return Ok(WakeProbe::Superseded);
    }
    let mut log = EventLog::open(run_dir)?;
    log.append(EventPayload::SupervisorAction {
        action: QUOTA_PROBE_ACTION.into(),
        node: Some(node.to_string()),
        detail,
    })?;
    Ok(match repause {
        Some(until) => {
            let seq = log
                .append(quota::quota_pause(agent, model, until).to_event())?
                .seq;
            WakeProbe::Repaused { seq, until }
        }
        None => WakeProbe::Passed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_failure_message_is_redacted_and_clipped() {
        let dir = tempfile::tempdir().unwrap();
        let token = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789";
        let msg = format!(
            "429 Too Many Requests\nAuthorization: Bearer {token}\n{}",
            "x".repeat(10_000)
        );
        let out = journal_message(dir.path(), &msg);
        assert!(!out.contains(token), "{out}");
        assert!(!out.contains("abcdefghijklmnop"), "{out}");
        assert!(out.starts_with("429 Too Many Requests"), "{out}");
        assert!(!out.contains('\n'), "{out}");
        assert!(
            out.len() < MESSAGE_HEAD + MESSAGE_TAIL + 64,
            "{}",
            out.len()
        );
    }
}
