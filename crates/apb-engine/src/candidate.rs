//! Candidate trials (issue #192): the run side of a forward patch.
//!
//! - At start, the gate picks the candidate for a start without an explicit
//!   version per `supervisor.policy.trial_candidates` ([`choose_at_gate`]),
//!   and preparation marks a run that starts on the candidate a trial
//!   (`candidate_trial: true` in the manifest, [`is_trial_start`]).
//! - At the end, a trial's outcome decides the candidate ([`settle`]): a
//!   failure (or a goal criterion that did not hold) drops the pointer and
//!   journals `candidate_rejected`; a success counts towards
//!   `promote_supervisor_patches` and, once it is met, moves `current` and
//!   journals `candidate_promoted`. Both land right before `run_finished`.
//! - The run surfaces read the verdict back with [`trial_of`].

use std::path::Path;

use apb_core::candidate::{
    OUTCOME_PROMOTED, OUTCOME_REJECTED, choose_for_start, clear_candidate_if, lineage_reaches,
    random_draw, read_candidate, update_trial,
};
use apb_core::registry::{LoadedPlaybook, Registry};
use apb_core::versioning::{
    PromotePolicy, promote_policy, promote_version, supersede_candidate, supersede_candidate_locked,
};
use serde::Serialize;

use crate::error::EngineError;
use crate::event::{Event, EventLog, EventPayload, read_all};
use crate::state::{RunState, RunStatus};

/// The gate's choice for a start without an explicit version: the candidate
/// and its definition when the policy admits it, else `None` (the start
/// runs `current`). A candidate that does not load leaves the start on
/// `current`. A candidate whose lineage no longer reaches `current` (a
/// person or an in-run promotion moved `current` past its base) is dropped
/// on the way, marked `superseded`.
pub(crate) fn choose_at_gate(
    reg: &Registry,
    playbook_dir: &Path,
    id: &str,
    current: &LoadedPlaybook,
) -> Option<(String, LoadedPlaybook)> {
    if let Some(c) = read_candidate(playbook_dir)
        && c != current.version
        && !lineage_reaches(playbook_dir, &c, &current.version)
        && let Err(e) = supersede_candidate(
            playbook_dir,
            None,
            &format!("`current` moved to {} past its base", current.version),
        )
    {
        eprintln!("apb: could not drop the stale candidate {c}: {e}");
    }
    let version = choose_for_start(
        playbook_dir,
        &current.playbook,
        &current.version,
        random_draw(),
    )?;
    reg.load(id, Some(&version)).ok().map(|l| (version, l))
}

/// Whether a top-level run starting on `version` of the playbook in
/// `playbook_dir` is a candidate trial: the pointer names that version and
/// `current` does not. A start that asked for the candidate by version is a
/// trial too: its outcome is the same evidence.
pub(crate) fn is_trial_start(playbook_dir: &Path, version: &str) -> bool {
    let current = std::fs::read_to_string(playbook_dir.join("current"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    read_candidate(playbook_dir).as_deref() == Some(version) && current != version
}

/// Decides the candidate after a trial run, right before its `run_finished`
/// (`succeeded`: the outcome that event will carry). Best effort: a fault
/// here is reported on stderr and never changes the run's outcome.
pub(crate) fn settle(log: &mut EventLog, run_dir: &Path, succeeded: bool) {
    if let Err(e) = settle_inner(log, run_dir, succeeded) {
        eprintln!("apb: could not settle the candidate trial: {e}");
    }
}

fn settle_inner(log: &mut EventLog, run_dir: &Path, succeeded: bool) -> Result<(), EngineError> {
    let trial = crate::manifest::read(run_dir)
        .ok()
        .flatten()
        .is_some_and(|m| m.candidate_trial);
    if !trial {
        return Ok(());
    }
    let events = read_all(run_dir)?;
    if events.iter().any(|e| {
        matches!(
            e.payload,
            EventPayload::CandidatePromoted { .. } | EventPayload::CandidateRejected { .. }
        )
    }) {
        return Ok(());
    }
    let Some((id, version)) = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunStarted { playbook, version } => Some((playbook.clone(), version.clone())),
        _ => None,
    }) else {
        return Ok(());
    };
    // Runs live in `<root>/.apb/runs/<id>`, next to `<root>/.apb/playbooks`.
    let (Some(apb_dir), Some(run_id)) = (
        run_dir.parent().and_then(Path::parent),
        run_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned()),
    ) else {
        return Ok(());
    };
    let Some(root) = apb_dir.parent() else {
        return Ok(());
    };
    let playbook_dir = apb_dir.join("playbooks").join(&id);
    let lock = apb_core::candidate::lock(&playbook_dir)?;
    // Replaced by a newer forward patch or already decided: not ours to judge.
    if read_candidate(&playbook_dir).as_deref() != Some(version.as_str()) {
        return Ok(());
    }
    // `current` moved past the candidate's base while the trial ran: the
    // trial judged a line a person left, so it neither promotes nor
    // rejects; the candidate ends as superseded.
    let current = std::fs::read_to_string(playbook_dir.join("current"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if !lineage_reaches(&playbook_dir, &version, &current) {
        supersede_candidate_locked(
            &playbook_dir,
            None,
            &format!("`current` moved to {current} during the trial"),
            &lock,
        )?;
        return Ok(());
    }
    let goal_failed = events.iter().find_map(|e| match &e.payload {
        EventPayload::GoalChecked {
            status,
            description,
            ..
        } if status == "failed" => Some(description.clone()),
        _ => None,
    });
    if !succeeded || goal_failed.is_some() {
        let reason = match goal_failed {
            Some(d) if succeeded => format!("goal criterion did not hold: {d}"),
            _ => "the trial run failed".to_string(),
        };
        clear_candidate_if(&playbook_dir, &version)?;
        update_trial(root, &id, &version, |t| {
            t.outcome = Some(OUTCOME_REJECTED.to_string());
            t.run_id = Some(run_id.clone());
            t.reason = Some(reason.clone());
        })?;
        log.append(EventPayload::CandidateRejected {
            version,
            run_id,
            reason,
        })?;
        return Ok(());
    }
    // A trial that migrated onto an in-run patch did not run the candidate
    // as it is: its success proves nothing about it (its failure does).
    if events
        .iter()
        .any(|e| matches!(e.payload, EventPayload::RunMigrated { .. }))
    {
        return Ok(());
    }
    let record = update_trial(root, &id, &version, |t| {
        t.successes = t.successes.saturating_add(1);
    })?;
    let candidate = Registry::open_dir(apb_dir)?.load(&id, Some(&version))?;
    let required = match promote_policy(&candidate.playbook) {
        PromotePolicy::Manual => None,
        PromotePolicy::OnSuccess | PromotePolicy::Always => Some(1),
        PromotePolicy::AfterNSuccesses(n) => Some(n.max(1)),
    };
    if !required.is_some_and(|n| record.successes >= n) {
        return Ok(());
    }
    promote_version(root, &id, &version)?;
    update_trial(root, &id, &version, |t| {
        t.outcome = Some(OUTCOME_PROMOTED.to_string());
        t.run_id = Some(run_id.clone());
    })?;
    log.append(EventPayload::CandidatePromoted {
        version,
        run_id,
        successes: record.successes,
    })?;
    Ok(())
}

/// A run's candidate trial, for the run surfaces (`run_status`, `apb runs`,
/// the dashboard run page).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CandidateTrial {
    /// The candidate version the run tried.
    pub version: String,
    /// `running`; `promoted` or `rejected` once the run decided it; `passed`
    /// for a success that did not promote yet (`after_n_successes`,
    /// `manual`); `undecided` for an end that judges nothing (a stop, a
    /// replaced candidate).
    pub verdict: String,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Why the start of the run in `run_dir` left a waiting candidate out and
/// ran `current` (`untrusted`, or `refused: <policy>`); `None` otherwise.
pub fn skipped_of(run_dir: &Path) -> Option<String> {
    crate::manifest::read(run_dir)
        .ok()
        .flatten()?
        .candidate_skipped
}

/// The candidate trial of the run in `run_dir`, `None` for a run that was
/// not one.
pub fn trial_of(run_dir: &Path, events: &[Event]) -> Option<CandidateTrial> {
    let manifest = crate::manifest::read(run_dir).ok().flatten()?;
    if !manifest.candidate_trial {
        return None;
    }
    let version = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunStarted { version, .. } => Some(version.clone()),
        _ => None,
    })?;
    for e in events {
        match &e.payload {
            EventPayload::CandidatePromoted { .. } => {
                return Some(CandidateTrial {
                    version,
                    verdict: OUTCOME_PROMOTED.into(),
                    reason: None,
                });
            }
            EventPayload::CandidateRejected { reason, .. } => {
                return Some(CandidateTrial {
                    version,
                    verdict: OUTCOME_REJECTED.into(),
                    reason: Some(reason.clone()),
                });
            }
            _ => {}
        }
    }
    let verdict = match RunState::fold(events).run_status {
        RunStatus::Succeeded => "passed",
        s if s.is_terminal() => "undecided",
        _ => "running",
    };
    Some(CandidateTrial {
        version,
        verdict: verdict.into(),
        reason: None,
    })
}
