//! The grader seam and what every grader shares: the criteria in force for
//! a run, a non-interactive grader's pass over it, and the grade assembled
//! from validated criteria and the reading trace.

use apb_core::quality::criteria::{CriterionDef, DEFAULT_TASK_TYPE, criteria_of};
use apb_core::quality::grade::{CriterionGrade, GRADE_SCHEMA, Grade, GraderId, SkillRead};

use super::evidence::{Proposal, Round, Validated, validate};
use super::reader::{ReadState, Reader};
use super::sources::RunSources;
use super::store;

/// A grader: which criteria it grades and what it proposes for them after
/// reading the run through a [`Reader`].
pub trait Grader {
    fn id(&self) -> GraderId;
    /// Whether this grader grades `def`.
    fn grades(&self, def: &CriterionDef) -> bool;
    fn propose(
        &self,
        sources: &RunSources,
        defs: &[CriterionDef],
        reader: &mut Reader<'_>,
    ) -> Vec<Proposal>;
    /// The skill reads it found, if it looks for them.
    fn skills(&self, _sources: &RunSources) -> Vec<SkillRead> {
        Vec::new()
    }
    /// Adjusts the validated criteria before the grade is signed (which of
    /// them count for a promotion).
    fn settle(&self, _criteria: &mut [CriterionGrade]) {}
}

/// The criteria in force for the run: its snapshot's `quality:` block.
pub(crate) fn criteria_for(sources: &RunSources) -> Vec<CriterionDef> {
    criteria_of(
        sources
            .run
            .snapshot
            .as_ref()
            .and_then(|p| p.quality.as_ref()),
    )
}

/// A grade of the run in `sources` from `criteria` and the reading
/// `state`.
pub(crate) fn assemble(
    sources: &RunSources,
    grader: GraderId,
    criteria: Vec<CriterionGrade>,
    state: &ReadState,
    skills: Vec<SkillRead>,
) -> Grade {
    let (playbook, version) = sources.started().unwrap_or_default();
    let created_at = apb_core::clock::now_ms_u64();
    let mut grade = Grade {
        schema: GRADE_SCHEMA,
        grade_id: store::new_grade_id(grader, created_at),
        run_id: sources.run_id().to_string(),
        playbook,
        version,
        task_type: sources
            .run
            .manifest
            .as_ref()
            .and_then(|m| m.task_type.clone())
            .unwrap_or_else(|| DEFAULT_TASK_TYPE.to_string()),
        grader,
        created_at,
        criteria,
        reading: state.trace(&sources.views),
        skills,
        mac: None,
    };
    if !grade.sign() {
        // Stored and shown, but it never counts in a promotion: say so
        // where it happens instead of leaving a candidate waiting.
        eprintln!(
            "apb: grade `{}` is stored unsigned (this installation has no key yet; run any apb command to make one): it does not count when a candidate is compared",
            grade.grade_id
        );
    }
    grade
}

/// Grades the run in `sources` with a non-interactive `grader`: its
/// proposals are validated as a final round, so what does not hold is
/// `not_enough_data` rather than refused.
pub(crate) fn run_grader(grader: &dyn Grader, sources: &RunSources) -> Grade {
    let defs: Vec<CriterionDef> = criteria_for(sources)
        .into_iter()
        .filter(|d| grader.grades(d))
        .collect();
    let budget = sources.views.all().iter().map(|v| v.chars()).sum();
    // Every grading view, a grade's `why` and the reader go through the
    // run's redactor: its own and its ancestors' secret values included.
    let redactor = crate::run_redact::RunRedactor::for_run_dir(&sources.run_dir);
    // The known values, even when discovery is incomplete: a model
    // grader's request withholds on its own, and the grade answers of
    // `run_grade`, `run_grades` and `run_grade_submit` go through the run's
    // redactor (refused while it withholds). The stored grade can still
    // carry text a secret of the tree matches later (a task type rendered
    // from the run's own ordinary param that a child declared secret), and
    // the stored grades `apb stats` and the candidate views show are not
    // redacted again.
    let mut reader = Reader::new(sources, redactor.known_values(), ReadState::new(budget));
    let proposals = grader.propose(sources, &defs, &mut reader);
    let state = reader.state;
    let round = Round::of(&proposals, &state);
    let mut criteria = match validate(&proposals, &defs, sources, &state, Some(&round)) {
        Validated::Accepted(c) => c,
        Validated::Rejected(_) => Vec::new(),
    };
    grader.settle(&mut criteria);
    assemble(
        sources,
        grader.id(),
        criteria,
        &state,
        grader.skills(sources),
    )
}
