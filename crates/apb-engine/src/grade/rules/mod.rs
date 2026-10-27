//! The rules grader (`rules/1`): the built-in criteria, graded
//! deterministically from the journal and the transcripts, free and
//! without a model.
//!
//! - `outcome` (here): the run's end;
//! - [`loops`]: signs of going in circles;
//! - [`scope`]: changes outside the task;
//! - [`consent`]: irreversible actions without consent;
//! - [`skills`]: whether a given skill was read, and whether it served.
//!
//! The grader reads every view whole first (the trace says so), so every
//! citation is a record it read and an absence rests on complete views.
//! Its proposals go through the same evidence validator as any grader's.

mod consent;
mod finding;
mod loops;
mod scope;
mod skills;

use apb_core::quality::criteria::{self, CriterionDef};
use apb_core::quality::grade::{GraderId, Level, SkillRead};

use super::evidence::Proposal;
use super::grader::Grader;
use super::reader::Reader;
use super::sources::RunSources;
use crate::event::EventPayload;
use finding::{Finding, j};

/// The rules grader.
pub struct RulesGrader;

impl Grader for RulesGrader {
    fn id(&self) -> GraderId {
        GraderId::RULES
    }

    fn grades(&self, def: &CriterionDef) -> bool {
        def.source == apb_core::quality::criteria::CriterionSource::Builtin
    }

    fn propose(
        &self,
        sources: &RunSources,
        defs: &[CriterionDef],
        reader: &mut Reader<'_>,
    ) -> Vec<Proposal> {
        for v in &sources.views {
            reader.scan(&v.id);
        }
        let reads = skills::reads(sources);
        defs.iter()
            .filter_map(|d| {
                let finding = match d.id() {
                    criteria::OUTCOME => outcome(sources),
                    criteria::LOOPS => loops::grade(sources),
                    criteria::SCOPE => scope::grade(sources),
                    criteria::CONSENT => consent::grade(sources),
                    criteria::SKILL_USE => skills::grade(sources, &reads),
                    _ => return None,
                };
                Some(finding.proposal(d.id()))
            })
            .collect()
    }

    fn skills(&self, sources: &RunSources) -> Vec<SkillRead> {
        skills::reads(sources)
    }
}

/// `outcome`: the run succeeded without a failure on the way (good), after
/// one it recovered from (partial), or failed (poor). A run a person
/// stopped says nothing about the playbook.
fn outcome(sources: &RunSources) -> Finding {
    let events = &sources.run.events;
    let last = |pred: &dyn Fn(&EventPayload) -> bool| {
        events
            .iter()
            .rev()
            .find(|e| pred(&e.payload))
            .map(|e| j(e.seq))
    };
    let failed_nodes: Vec<String> = events
        .iter()
        .filter(|e| {
            matches!(&e.payload, EventPayload::NodeFinished { status, .. } if status == "failed")
        })
        .map(|e| j(e.seq))
        .collect();
    let end = last(&|p| matches!(p, EventPayload::RunFinished { .. }));
    match sources.outcome.as_deref() {
        Some("succeeded") => {
            let at = end.or_else(|| last(&|p| matches!(p, EventPayload::NodeFinished { .. })));
            if failed_nodes.is_empty() {
                Finding::new(Level::Good, "succeeded", "the run succeeded")
                    .citing(at.into_iter().collect())
            } else {
                Finding::new(
                    Level::Partial,
                    "recovered",
                    format!(
                        "the run succeeded after {} failed node result(s)",
                        failed_nodes.len()
                    ),
                )
                .citing(failed_nodes)
            }
        }
        Some("failed") => {
            let at = last(&|p| matches!(p, EventPayload::RunError { .. })).or(end);
            Finding::new(Level::Poor, "run_failed", "the run failed")
                .citing(at.into_iter().collect())
        }
        Some(_) => Finding::new(
            Level::NotEnoughData,
            "stopped",
            "the run was stopped before it ended",
        ),
        None => Finding::new(Level::NotEnoughData, "live", "the run has not ended"),
    }
}
