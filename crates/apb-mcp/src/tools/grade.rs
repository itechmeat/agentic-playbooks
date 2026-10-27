//! Run quality on the surfaces: grading a finished run and reading its
//! grades. MCP (`run_grade`, `run_grade_*`, `run_grades`, the `grades` block of
//! `run_status`), the CLI (`apb grade`, the grade line of `apb runs <id>`)
//! and the HTTP API (`/api/runs/{id}/grades`) all go through this module,
//! so every stored grade is followed by the same candidate re-settle
//! ([`after_stored`]). Every call that grades, reads a grading session or
//! reopens one is refused from a run step (`step`, see
//! [`super::outside_run`]): a run never grades its own playbook's runs.
//! The grading itself is [`apb_engine::grade`].

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::quality::QualityRecord;
use apb_core::quality::grade::{Grade, GraderId, Level};
use apb_core::step_env::StepCaller;
use apb_engine::grade::evidence::Proposal;
use apb_engine::grade::reader::{ReadRequest, ReadResponse};
use apb_engine::grade::{Backfilled, GraderChoice, ModelGraded, SessionStart, SubmitOutcome};
use serde::Serialize;

use super::{ToolError, outside_run, resolve_run_dir};

/// What a refusal from a run step names.
const GRADING: &str = "grading a run";

/// What a stored grade did to the playbook's candidate: the decision
/// recorded under a quality policy, or why re-deciding failed. Both absent
/// when the playbook has no candidate on trial under a quality policy.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CandidateAfter {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub candidate: Option<QualityRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub candidate_error: Option<String>,
}

/// Re-decides the candidate of playbook `id` after a grade of one of its
/// runs was stored outside a run. The one place every grading surface
/// calls: a grade never changes a candidate's standing silently or on one
/// surface only. A playbook without a candidate is left untouched (no lock,
/// no directory is created). A failure is reported, never raised: the grade
/// is stored either way.
pub fn after_stored(root: &Path, id: &str) -> CandidateAfter {
    let dir = apb_core::candidate::playbook_dir(root, id);
    if apb_core::candidate::read_candidate(&dir).is_none() {
        return CandidateAfter::default();
    }
    match apb_engine::candidate::resettle(root, id) {
        Ok(candidate) => CandidateAfter {
            candidate,
            candidate_error: None,
        },
        Err(e) => CandidateAfter {
            candidate: None,
            candidate_error: Some(e.to_string()),
        },
    }
}

/// A grade on request: the stored grade and what it did to the candidate,
/// or why the grader stored nothing (a model grader whose use is off, over
/// its daily cap, or with no criteria of the playbook's own).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Graded {
    /// `rules` or `model`.
    pub grader: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub grade: Option<Grade>,
    /// Why nothing was stored: `off`, `no_criteria`, `budget`, `send`,
    /// `busy`, `state_too_large`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub skipped: Option<String>,
    #[serde(flatten)]
    pub after: CandidateAfter,
}

/// Grades the ended run `run_id` with `grader` (rules: the built-in
/// criteria, free; model: the playbook's own criteria through the
/// project's decision model, only when its `grade` use is on), stores the
/// grade and re-decides the candidate.
pub fn grade_by(
    root: &Path,
    run_id: &str,
    grader: GraderChoice,
    step: &StepCaller,
) -> Result<Graded, ToolError> {
    outside_run(step, GRADING)?;
    let dir = resolve_run_dir(root, run_id)?;
    let graded = match grader {
        GraderChoice::Rules => ModelGraded::Graded(Box::new(apb_engine::grade::grade_rules(&dir)?)),
        GraderChoice::Model => apb_engine::grade::grade_model(&dir)?,
    };
    Ok(match graded {
        ModelGraded::Graded(grade) => Graded {
            grader: grader.as_str().to_string(),
            after: after_stored(root, &grade.playbook),
            grade: Some(*grade),
            skipped: None,
        },
        ModelGraded::Skipped { reason } => Graded {
            grader: grader.as_str().to_string(),
            grade: None,
            skipped: Some(reason),
            after: CandidateAfter::default(),
        },
    })
}

/// A grader name from a surface argument (`rules` when absent).
pub fn grader_of(name: Option<&str>) -> Result<GraderChoice, ToolError> {
    match name {
        None => Ok(GraderChoice::Rules),
        Some(n) => GraderChoice::parse(n)
            .ok_or_else(|| ToolError::Engine(format!("unknown grader `{n}`: use rules or model"))),
    }
}

/// Opens a host grading session over the ended run `run_id` for its
/// playbook's own criteria.
pub fn grade_start(
    root: &Path,
    run_id: &str,
    budget_chars: Option<u64>,
    step: &StepCaller,
) -> Result<SessionStart, ToolError> {
    outside_run(step, GRADING)?;
    let dir = resolve_run_dir(root, run_id)?;
    Ok(apb_engine::grade::start_session(&dir, budget_chars)?)
}

/// One reading step of a host grading session.
pub fn grade_read(
    root: &Path,
    run_id: &str,
    session: &str,
    req: &ReadRequest,
    step: &StepCaller,
) -> Result<ReadResponse, ToolError> {
    outside_run(step, GRADING)?;
    let dir = resolve_run_dir(root, run_id)?;
    Ok(apb_engine::grade::read(&dir, session, req)?)
}

/// A host's submission and, when it was stored, what it did to the
/// candidate.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Submitted {
    #[serde(flatten)]
    pub outcome: SubmitOutcome,
    #[serde(flatten)]
    pub after: CandidateAfter,
}

/// Submits the host's levels for a grading session; an accepted round is
/// stored and re-decides the candidate.
pub fn grade_submit(
    root: &Path,
    run_id: &str,
    session: &str,
    proposals: &[Proposal],
    step: &StepCaller,
) -> Result<Submitted, ToolError> {
    outside_run(step, GRADING)?;
    let dir = resolve_run_dir(root, run_id)?;
    let outcome = apb_engine::grade::submit(&dir, session, proposals)?;
    let after = match &outcome {
        SubmitOutcome::Graded { grade } => after_stored(root, &grade.playbook),
        SubmitOutcome::Rejected { .. } => CandidateAfter::default(),
    };
    Ok(Submitted { outcome, after })
}

/// A person reopens the grading of run `run_id`, which has a host grade:
/// one more host grade may be stored (`by` names the surface).
pub fn grade_reopen(
    root: &Path,
    run_id: &str,
    by: &str,
    step: &StepCaller,
) -> Result<serde_json::Value, ToolError> {
    outside_run(step, "reopening a run's grading")?;
    let dir = resolve_run_dir(root, run_id)?;
    apb_engine::grade::reopen(&dir, by)?;
    Ok(serde_json::json!({
        "run_id": run_id,
        "reopened": true,
        "next": "a host may grade the run once more: run_grade_start",
    }))
}

/// Every stored grade of run `run_id`, oldest first.
pub fn run_grades(root: &Path, run_id: &str) -> Result<Vec<Grade>, ToolError> {
    let dir = resolve_run_dir(root, run_id)?;
    Ok(apb_engine::grade::grades_of(&dir))
}

/// What a backfill graded, and what that did to the candidate.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BackfillReport {
    pub runs: Vec<Backfilled>,
    #[serde(flatten)]
    pub after: CandidateAfter,
}

/// Grades with `grader` the latest `last` ended runs of playbook `id` (of
/// `version` when given) that have no grade of that grader yet, then
/// re-decides the candidate once. A model backfill stops at the first run
/// the decision model skips (its use off, its daily cap spent).
pub fn backfill(
    root: &Path,
    id: &str,
    version: Option<&str>,
    last: usize,
    grader: GraderChoice,
    step: &StepCaller,
) -> Result<BackfillReport, ToolError> {
    outside_run(step, GRADING)?;
    if !apb_core::registry::is_safe_segment(id) {
        return Err(ToolError::NotFound(format!("playbook `{id}`")));
    }
    let runs = apb_engine::grade::backfill(root, id, version, last, grader);
    let after = if runs.iter().any(|r| r.grade_id.is_some()) {
        after_stored(root, id)
    } else {
        CandidateAfter::default()
    };
    Ok(BackfillReport { runs, after })
}

/// The latest grade of one grader, as the run surfaces summarize it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GradeLine {
    /// `rules/1`, `host/1`.
    pub grader: String,
    pub grade_id: String,
    /// Criterion id to level.
    pub levels: BTreeMap<String, Level>,
}

impl GradeLine {
    /// `rules/1: outcome good, loops poor`.
    pub fn line(&self) -> String {
        let levels: Vec<String> = self
            .levels
            .iter()
            .map(|(id, level)| format!("{id} {}", level.as_str()))
            .collect();
        format!("{}: {}", self.grader, levels.join(", "))
    }
}

/// The run surfaces' `grades` block: the latest grade of each grader, in
/// grader order (the rules grader before a host); empty for a run nobody
/// graded.
pub fn grade_lines(run_dir: &Path) -> Vec<GradeLine> {
    let mut latest: BTreeMap<GraderId, Grade> = BTreeMap::new();
    for g in apb_engine::grade::grades_of(run_dir) {
        latest.insert(g.grader, g);
    }
    latest
        .into_values()
        .map(|g| GradeLine {
            grader: g.grader.to_string(),
            grade_id: g.grade_id,
            levels: g.criteria.into_iter().map(|c| (c.id, c.level)).collect(),
        })
        .collect()
}

/// Serializes a surface answer as the MCP layer returns it.
pub(crate) fn to_value(v: impl Serialize) -> Result<serde_json::Value, ToolError> {
    serde_json::to_value(v).map_err(|e| ToolError::Engine(e.to_string()))
}
