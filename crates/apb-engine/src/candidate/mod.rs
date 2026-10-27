//! Candidate trials (issue #192): the run side of a forward patch.
//!
//! - At start, the gate picks the candidate for a start without an explicit
//!   version per `supervisor.policy.trial_candidates` ([`choose_at_gate`]),
//!   and preparation marks a run that starts on the candidate a trial
//!   (`candidate_trial: true` in the manifest, [`is_trial_start`]).
//! - At the end, a trial decides the candidate ([`settle`]) per
//!   `promote_supervisor_patches` of `current`: by successes ([`settle`]
//!   module: a failure or a goal criterion that did not hold rejects, enough
//!   successes promote) or by quality ([`quality`]: the graded trials are
//!   compared with `current`'s graded runs; one failure alone decides
//!   nothing). What it decided is journaled right before `run_finished`
//!   (`candidate_promoted`, `candidate_rejected`, `candidate_held`).
//! - A grade stored after a run ended re-decides a candidate under the
//!   quality policy ([`resettle`]), and a person may decide at any time
//!   ([`decide_by_person`]); neither writes to a journal.
//! - The run surfaces read the verdict back with [`trial_of`], the
//!   playbook surfaces the comparison with [`quality_view`].

mod line;
mod quality;
mod settle;

pub use quality::{CandidateQualityView, PersonDecision, decide_by_person, quality_view, resettle};

use std::path::Path;

use apb_core::candidate::{
    OUTCOME_PROMOTED, OUTCOME_REJECTED, PromotePolicy, choose_for_start, lineage_reaches,
    promote_policy, random_draw, read_candidate,
};
use apb_core::registry::{LoadedPlaybook, Registry};
use apb_core::versioning::{supersede_candidate, supersede_candidate_locked};
use serde::Serialize;

use crate::error::EngineError;
use crate::event::{Event, EventLog, EventPayload, read_all};
use crate::state::{RunState, RunStatus};
use line::{Line, Trial, current_of};

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
    read_candidate(playbook_dir).as_deref() == Some(version) && current_of(playbook_dir) != version
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
            EventPayload::CandidatePromoted { .. }
                | EventPayload::CandidateRejected { .. }
                | EventPayload::CandidateHeld { .. }
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
    let (Some(root), Some(run_id)) = (
        crate::run_retro::project_root_of(run_dir),
        run_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned()),
    ) else {
        return Ok(());
    };
    let playbook_dir = apb_core::candidate::playbook_dir(&root, &id);
    // A quality comparison reads every run of both versions: it is taken
    // before the lock, and confirmed under it (see `quality::Taken`).
    let taken = Line::open(&root, &id)
        .filter(|l| l.candidate == version)
        .and_then(|l| {
            let current = l.current_definition().ok()?;
            match promote_policy(&current.playbook) {
                PromotePolicy::Quality(gate) => Some((
                    l.current.clone(),
                    quality::Taken::of(&l, &current.playbook, &gate, Some(&run_id)),
                )),
                _ => None,
            }
        });
    let lock = apb_core::candidate::lock(&playbook_dir)?;
    // Replaced by a newer forward patch or already decided: not ours to judge.
    let Some(line) = Line::open(&root, &id).filter(|l| l.candidate == version) else {
        return Ok(());
    };
    // `current` moved past the candidate's base while the trial ran: the
    // trial judged a line a person left, so it neither promotes nor
    // rejects; the candidate ends as superseded.
    if !lineage_reaches(&playbook_dir, &version, &line.current) {
        supersede_candidate_locked(
            &playbook_dir,
            None,
            &format!("`current` moved to {} during the trial", line.current),
            &lock,
        )?;
        return Ok(());
    }
    // The policy and the criteria come from `current`. A forward patch can
    // change neither its `supervisor` nor its `quality:` block, so when
    // `current` does not load (a person left it broken) the candidate's
    // own definition carries the same ones, and a failed trial is still
    // judged.
    let current = match line.current_definition() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "apb: `current` of `{id}` does not load ({e}); the candidate's policy decides"
            );
            Registry::open(&root)?.load(&id, Some(&version))?
        }
    };
    let trial = Trial {
        line,
        run_id,
        events,
    };
    match promote_policy(&current.playbook) {
        PromotePolicy::Quality(gate) => {
            // Taken on the same `current` only.
            let taken = taken
                .filter(|(at, _)| *at == trial.line.current)
                .map(|(_, t)| t);
            quality::settle_trial(log, &trial, &current.playbook, &gate, succeeded, taken)
        }
        policy => settle::by_successes(log, &trial, policy, succeeded),
    }
}

/// A run's candidate trial, for the run surfaces (`run_status`, `apb runs`,
/// the dashboard run page).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CandidateTrial {
    /// The candidate version the run tried.
    pub version: String,
    /// `running`; `promoted` or `rejected` once the run decided it; `wait`
    /// or `needs_person` for a quality comparison it left open (or, once
    /// that comparison was decided after the run, what the candidate's
    /// trial record says now); `passed`
    /// for a success that did not promote yet (`after_n_successes`,
    /// `manual`); `undecided` for an end that judges nothing (a stop, a
    /// replaced candidate).
    pub verdict: String,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// What the trial record of candidate `version` says now, for a held
/// trial run: its outcome once promoted or rejected; `undecided` once it
/// ended any other way (superseded, or no longer the candidate); else the
/// verdict recorded last. `None` when it cannot be read.
fn decided_since(run_dir: &Path, events: &[Event], version: &str) -> Option<CandidateTrial> {
    let playbook = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunStarted { playbook, .. } => Some(playbook.clone()),
        _ => None,
    })?;
    let root = crate::run_retro::project_root_of(run_dir)?;
    let trial = apb_core::versioning::read_provenance(&root, &playbook, version)
        .ok()
        .flatten()?
        .trial?;
    let reason = trial
        .quality
        .as_ref()
        .map(|q| q.reason.clone())
        .or(trial.reason.clone());
    let still_candidate = read_candidate(&apb_core::candidate::playbook_dir(&root, &playbook))
        .as_deref()
        == Some(version);
    let verdict = match trial.outcome.as_deref() {
        Some(o @ (OUTCOME_PROMOTED | OUTCOME_REJECTED)) => o.to_string(),
        Some(_) => UNDECIDED.to_string(),
        None if !still_candidate => UNDECIDED.to_string(),
        None => trial.quality.as_ref()?.verdict.as_str().to_string(),
    };
    Some(CandidateTrial {
        version: version.to_string(),
        verdict,
        reason,
    })
}

/// The verdict of a trial whose end judged nothing.
const UNDECIDED: &str = "undecided";

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
            EventPayload::CandidateHeld {
                verdict, reason, ..
            } => {
                // A held candidate is decided later (a grade stored after
                // the run, a person) without a journal event: the trial
                // record says what stands now.
                let decided = decided_since(run_dir, events, &version);
                return Some(decided.unwrap_or(CandidateTrial {
                    version,
                    verdict: verdict.clone(),
                    reason: Some(reason.clone()),
                }));
            }
            _ => {}
        }
    }
    let verdict = match RunState::fold(events).run_status {
        RunStatus::Succeeded => "passed",
        s if s.is_terminal() => UNDECIDED,
        _ => "running",
    };
    Some(CandidateTrial {
        version,
        verdict: verdict.into(),
        reason: None,
    })
}
