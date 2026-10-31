//! Settling a trial by successes (`on_success`, `always`,
//! `after_n_successes`, `manual`): a failed trial or a goal criterion that
//! did not hold rejects the candidate; a success counts, and enough of them
//! promote it.

use apb_core::candidate::PromotePolicy;

use super::line::Trial;
use crate::error::EngineError;
use crate::event::{EventLog, EventPayload};

/// The basis a promotion by successes journals.
pub const BASIS_SUCCESSES: &str = "successes";

pub(super) fn by_successes(
    log: &mut EventLog,
    trial: &Trial,
    policy: PromotePolicy,
    succeeded: bool,
) -> Result<(), EngineError> {
    let line = &trial.line;
    let goal_failed = trial.goal_failed();
    if !succeeded || goal_failed.is_some() {
        let reason = match goal_failed {
            Some(d) if succeeded => format!("goal criterion did not hold: {d}"),
            _ => "the trial run failed".to_string(),
        };
        line.reject(Some(&trial.run_id), &reason, None)?;
        log.append(EventPayload::CandidateRejected {
            version: line.candidate.clone(),
            run_id: trial.run_id.clone(),
            reason,
        })?;
        return Ok(());
    }
    // A trial that migrated onto an in-run patch did not run the candidate
    // as it is: its success proves nothing about it (its failure does).
    if trial.migrated() {
        return Ok(());
    }
    let record = apb_core::candidate::update_trial(&line.root, &line.id, &line.candidate, |t| {
        t.successes = t.successes.saturating_add(1);
    })?;
    if !policy
        .required_successes()
        .is_some_and(|n| record.successes >= n)
    {
        return Ok(());
    }
    line.promote(Some(&trial.run_id), None)?;
    log.append(EventPayload::CandidatePromoted {
        version: line.candidate.clone(),
        run_id: trial.run_id.clone(),
        successes: record.successes,
        basis: Some(BASIS_SUCCESSES.to_string()),
    })?;
    Ok(())
}
