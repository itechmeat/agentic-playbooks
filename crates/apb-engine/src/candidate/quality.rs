//! Settling a candidate by quality (`promote_supervisor_patches: { when:
//! quality }`): the candidate's graded trial runs are compared with the
//! latest graded runs of `current` ([`apb_core::quality::compare`]), under
//! the criteria and the gate of `current`.
//!
//! - `promote` moves `current` to the candidate;
//! - `keep_current` drops the candidate ("change nothing");
//! - `wait` and `needs_person` keep it on trial and record why.
//!
//! One failed trial alone decides nothing: the failure is in its grade.
//! The decision is re-taken at a trial's end (journaled), whenever a grade
//! is stored later ([`resettle`], not journaled), and a person may decide
//! instead ([`decide_by_person`]).

use std::path::{Path, PathBuf};

use apb_core::candidate::{PromotePolicy, lineage_reaches, promote_policy};
use apb_core::quality::QualityRecord;
use apb_core::quality::compare::{QualityDecision, QualityGate, Verdict, decide, expected_of};
use apb_core::quality::criteria::criteria_of;
use apb_core::quality::grade::Grade;
use apb_core::schema::Playbook;
use serde::Serialize;

use super::line::{Line, Trial};
use crate::error::EngineError;
use crate::event::{EventLog, EventPayload};

/// The basis a promotion by quality journals.
pub const BASIS_QUALITY: &str = "quality";
/// Who decided when the rule did.
pub const DECIDED_BY_APB: &str = "apb";
/// One side of a comparison: the grades and how many trial runs there were.
struct Side {
    grades: Vec<Grade>,
    runs: u32,
}

/// Which runs of a version a side counts.
#[derive(Clone, Copy)]
enum Runs<'a> {
    /// The candidate's trial runs; `ending` names a trial that ends now.
    Trials { ending: Option<&'a str> },
    /// `current`'s newest graded runs, at most `window`.
    Graded { window: usize },
}

/// The graded runs of `version` that `runs` asks for. Only run
/// directories apb created on this machine are listed
/// ([`crate::run_history::all_runs_of`] verifies each one's origin stamp), and
/// only grades this installation signed for that very run count: a copied
/// or hand-made run folder or grade file is left out. Every run of the
/// playbook is looked at, however many other runs the project has.
fn side(line: &Line, version: &str, runs: Runs<'_>) -> Side {
    let dirs: Vec<PathBuf> =
        crate::run_history::all_runs_of(std::slice::from_ref(&line.root), &line.id, Some(version));
    let mut out = Side {
        grades: Vec::new(),
        runs: 0,
    };
    for d in dirs {
        let ending = match runs {
            Runs::Graded { window } if out.runs as usize >= window => break,
            Runs::Trials { ending } => {
                if !is_trial(&d) {
                    continue;
                }
                ending
            }
            Runs::Graded { .. } => None,
        };
        if !ran_as_written(&d, ending) {
            continue;
        }
        let run_id = d.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let grades: Vec<Grade> = crate::grade::grades_of(&d)
            .into_iter()
            .filter(|g| g.run_id == run_id && g.signed_here())
            .collect();
        if matches!(runs, Runs::Graded { .. }) && grades.is_empty() {
            continue;
        }
        out.runs += 1;
        out.grades.extend(grades);
    }
    out
}

/// Whether the run in `run_dir` started as a candidate trial.
fn is_trial(run_dir: &Path) -> bool {
    crate::manifest::read(run_dir)
        .ok()
        .flatten()
        .is_some_and(|m| m.candidate_trial)
}

/// A run that ended (`ending` names one that ends now) without migrating
/// onto an in-run patch: its grades describe the version as written.
fn ran_as_written(run_dir: &Path, ending: Option<&str>) -> bool {
    let ends_now =
        ending.is_some_and(|id| run_dir.file_name().and_then(|n| n.to_str()) == Some(id));
    let events = crate::run_view::read_events(run_dir).unwrap_or_default();
    let ended = ends_now
        || events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::RunFinished { .. }));
    ended
        && !events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::RunMigrated { .. }))
}

/// The comparison of the line's candidate with `current` under `gate`,
/// with the criteria of `current`'s definition.
fn evaluate(
    line: &Line,
    current: &Playbook,
    gate: &QualityGate,
    ending: Option<&str>,
) -> (QualityDecision, u32) {
    let expected = expected_of(&criteria_of(current.quality.as_ref()));
    let cand = side(line, &line.candidate, Runs::Trials { ending });
    let cur = side(
        line,
        &line.current,
        Runs::Graded {
            window: gate.window as usize,
        },
    );
    (
        decide(&expected, &cand.grades, &cur.grades, gate, cand.runs),
        cand.runs,
    )
}

/// A comparison taken without the candidate lock, with the trial record it
/// was taken against.
pub(super) struct Taken {
    decision: QualityDecision,
    trials: u32,
    state: Option<String>,
}

impl Taken {
    /// The comparison of the line now. It reads every run of both
    /// versions, so it is taken before the lock is.
    pub(super) fn of(
        line: &Line,
        current: &Playbook,
        gate: &QualityGate,
        ending: Option<&str>,
    ) -> Self {
        let state = line.trial_state();
        let (decision, trials) = evaluate(line, current, gate, ending);
        Taken {
            decision,
            trials,
            state,
        }
    }

    /// The comparison to carry out, under the lock: this one while the
    /// trial record is the one it was taken against, else taken again (a
    /// trial ended or a decision was recorded meanwhile, so it is stale).
    fn confirmed(
        self,
        line: &Line,
        current: &Playbook,
        gate: &QualityGate,
        ending: Option<&str>,
    ) -> (QualityDecision, u32) {
        if line.trial_state() == self.state {
            (self.decision, self.trials)
        } else {
            evaluate(line, current, gate, ending)
        }
    }
}

fn record(decision: &QualityDecision, by: &str) -> QualityRecord {
    QualityRecord {
        verdict: decision.verdict,
        reason: decision.reason.clone(),
        compared: decision.per_criterion.clone(),
        decided_at: apb_core::clock::now_ms_u64(),
        decided_by: by.to_string(),
    }
}

/// Carries out `decision` on the line; returns the record kept and the
/// candidate's trial successes.
fn apply(
    line: &Line,
    decision: &QualityDecision,
    run_id: Option<&str>,
    by: &str,
) -> Result<(QualityRecord, u32), EngineError> {
    let rec = record(decision, by);
    let successes = match decision.verdict {
        Verdict::Promote => line.promote(run_id, Some(rec.clone()))?.successes,
        Verdict::KeepCurrent => {
            line.reject(
                run_id,
                &format!("kept current: {}", decision.reason),
                Some(rec.clone()),
            )?;
            0
        }
        Verdict::Wait | Verdict::NeedsPerson => {
            apb_core::candidate::update_trial(&line.root, &line.id, &line.candidate, |t| {
                t.quality = Some(rec.clone());
            })?
            .successes
        }
    };
    Ok((rec, successes))
}

/// Settles a trial run that ends now, under the candidate lock, and
/// journals what was decided. `taken` is the comparison taken before the
/// lock, used when nothing changed since.
pub(super) fn settle_trial(
    log: &mut EventLog,
    trial: &Trial,
    current: &Playbook,
    gate: &QualityGate,
    succeeded: bool,
    taken: Option<Taken>,
) -> Result<(), EngineError> {
    let line = &trial.line;
    if trial.migrated() {
        return Ok(());
    }
    let ending = Some(trial.run_id.as_str());
    // Confirmed before the success count moves the trial record.
    let (decision, _) = match taken {
        Some(t) => t.confirmed(line, current, gate, ending),
        None => evaluate(line, current, gate, ending),
    };
    if succeeded && trial.goal_failed().is_none() {
        apb_core::candidate::update_trial(&line.root, &line.id, &line.candidate, |t| {
            t.successes = t.successes.saturating_add(1);
        })?;
    }
    let (rec, successes) = apply(line, &decision, Some(&trial.run_id), DECIDED_BY_APB)?;
    let (version, run_id) = (line.candidate.clone(), trial.run_id.clone());
    log.append(match rec.verdict {
        Verdict::Promote => EventPayload::CandidatePromoted {
            version,
            run_id,
            successes,
            basis: Some(BASIS_QUALITY.to_string()),
        },
        Verdict::KeepCurrent => EventPayload::CandidateRejected {
            version,
            run_id,
            reason: format!("kept current: {}", rec.reason),
        },
        verdict => EventPayload::CandidateHeld {
            version,
            run_id,
            verdict: verdict.as_str().to_string(),
            reason: rec.reason,
        },
    })?;
    Ok(())
}

/// The line of playbook `id` with its candidate on a `current` it still
/// descends from, and `current`'s definition; `None` without such a
/// candidate.
fn open_line(root: &Path, id: &str) -> Result<Option<(Line, Playbook)>, EngineError> {
    let Some(line) = Line::open(root, id) else {
        return Ok(None);
    };
    if line.candidate == line.current
        || !lineage_reaches(&line.playbook_dir, &line.candidate, &line.current)
    {
        return Ok(None);
    }
    let current = line.current_definition()?.playbook;
    Ok(Some((line, current)))
}

fn quality_gate(current: &Playbook) -> Option<QualityGate> {
    match promote_policy(current) {
        PromotePolicy::Quality(gate) => Some(gate),
        _ => None,
    }
}

/// Re-decides the candidate of playbook `id` under the project `root`
/// after a grade was stored outside a run: under a quality policy the
/// comparison is taken again and carried out. Returns what was recorded,
/// `None` when there is no candidate under a quality policy.
pub fn resettle(root: &Path, id: &str) -> Result<Option<QualityRecord>, EngineError> {
    // The comparison reads many runs: it is taken without the candidate
    // lock, so a trial ending meanwhile is not kept waiting; under the
    // lock it is carried out only on the line it was taken on, and taken
    // again when the trial record moved since.
    let Some((line, current)) = open_line(root, id)? else {
        return Ok(None);
    };
    let Some(gate) = quality_gate(&current) else {
        return Ok(None);
    };
    let taken = Taken::of(&line, &current, &gate, None);
    let _held = apb_core::candidate::lock(&apb_core::candidate::playbook_dir(root, id))?;
    if !line.unchanged() {
        return Ok(None);
    }
    let (decision, _) = taken.confirmed(&line, &current, &gate, None);
    Ok(Some(apply(&line, &decision, None, DECIDED_BY_APB)?.0))
}

/// A person's decision on a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonDecision {
    Promote,
    /// Change nothing: drop the candidate.
    KeepCurrent {
        reason: Option<String>,
    },
}

/// A person decides the candidate of playbook `id`: promote it, or keep
/// `current` and drop it. `by` names the surface (`cli`, `dashboard`,
/// `mcp:<client>`). Refused without a candidate.
pub fn decide_by_person(
    root: &Path,
    id: &str,
    decision: PersonDecision,
    by: &str,
) -> Result<QualityRecord, EngineError> {
    let missing = || EngineError::NotFound(format!("playbook `{id}` has no candidate on trial"));
    let Some((line, current)) = open_line(root, id)? else {
        return Err(missing());
    };
    let gate = quality_gate(&current);
    let taken = gate.as_ref().map(|g| Taken::of(&line, &current, g, None));
    let _held = apb_core::candidate::lock(&apb_core::candidate::playbook_dir(root, id))?;
    if !line.unchanged() {
        return Err(EngineError::Conflict(format!(
            "the candidate of playbook `{id}` changed while deciding: look again and decide"
        )));
    }
    let compared = match (taken, gate) {
        (Some(t), Some(g)) => t.confirmed(&line, &current, &g, None).0.per_criterion,
        _ => Vec::new(),
    };
    let (verdict, reason) = match &decision {
        PersonDecision::Promote => (Verdict::Promote, "promoted by a person".to_string()),
        PersonDecision::KeepCurrent { reason } => (
            Verdict::KeepCurrent,
            reason
                .clone()
                .unwrap_or_else(|| "a person kept current".to_string()),
        ),
    };
    let decided = QualityDecision {
        verdict,
        per_criterion: compared,
        reason,
    };
    Ok(apply(&line, &decided, None, by)?.0)
}

/// A candidate and how it compares, for the playbook surfaces.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CandidateQualityView {
    pub version: String,
    pub current: String,
    /// The candidate's ended trial runs.
    pub trials: u32,
    /// The gate under a quality policy; absent under the others.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub gate: Option<QualityGate>,
    /// The comparison as it stands now (quality policy only).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub decision: Option<QualityDecision>,
    /// The latest decision recorded on the candidate's trial record.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub recorded: Option<QualityRecord>,
}

/// The candidate of playbook `id` under `root` and how it compares with
/// `current` now; `None` without a candidate. Read-only.
pub fn quality_view(root: &Path, id: &str) -> Result<Option<CandidateQualityView>, EngineError> {
    let Some((line, current)) = open_line(root, id)? else {
        return Ok(None);
    };
    let gate = quality_gate(&current);
    let (decision, trials) = match &gate {
        Some(g) => {
            let (d, n) = evaluate(&line, &current, g, None);
            (Some(d), n)
        }
        None => (
            None,
            side(&line, &line.candidate, Runs::Trials { ending: None }).runs,
        ),
    };
    let recorded = apb_core::versioning::read_provenance(root, id, &line.candidate)
        .ok()
        .flatten()
        .and_then(|p| p.trial)
        .and_then(|t| t.quality);
    Ok(Some(CandidateQualityView {
        version: line.candidate,
        current: line.current,
        trials,
        gate,
        decision,
        recorded,
    }))
}
