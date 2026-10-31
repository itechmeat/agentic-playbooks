//! Grading a finished run (issue: run quality): what a grader concluded
//! about each criterion of the playbook's `quality:` block, stored as a
//! write-once grade with the records it cites and a trace of what it read.
//!
//! - [`sources`]: every view of the run a grader may read;
//! - [`reader`]: the bounded, redacting, traced reader;
//! - [`evidence`]: the one validator every grader's output goes through;
//! - [`rules`]: the rules grader for the built-in criteria;
//! - [`model`]: the decision-model grader for the playbook's own criteria;
//! - [`session`]: a host's grading session for the playbook's own criteria;
//! - [`store`]: the grade files.
//!
//! Entry points: [`grade_rules`] and [`grade_model`] (any finished run, on
//! request), [`grade_ending`] (the inline grade as a run ends), [`start_session`],
//! [`read`] and [`submit`] (a host's session), [`grades_of`] and
//! [`backfill`]. A stored grade may change a candidate's standing: the
//! caller runs [`crate::candidate::resettle`] after storing one outside a
//! run.

pub mod evidence;
mod grader;
pub mod model;
pub mod reader;
pub mod rules;
pub mod session;
pub mod sources;
pub mod store;

use std::path::{Path, PathBuf};

use apb_core::quality::grade::{Grade, GraderId};
use serde::Serialize;

use crate::error::EngineError;
use grader::run_grader;
use sources::{RunEnd, RunSources};

pub use grader::Grader;
pub use session::{SessionStart, SubmitOutcome, read, reopen, start_session, submit};
pub use store::{grades_of, latest_by_grader, split_grades_of, trusted_grades_of};

/// The sources of the ended run at `run_dir`.
fn ended_sources(run_dir: &Path) -> Result<RunSources, EngineError> {
    let sources = RunSources::load(run_dir, RunEnd::Journaled)?;
    if sources.outcome.is_none() {
        return Err(EngineError::Conflict(
            "the run has not ended: a run is graded once it ends".into(),
        ));
    }
    Ok(sources)
}

/// Grades the finished run at `run_dir` by rules and stores the grade.
pub fn grade_rules(run_dir: &Path) -> Result<Grade, EngineError> {
    let sources = ended_sources(run_dir)?;
    let grade = run_grader(&rules::RulesGrader, &sources);
    store::write(run_dir, &grade)?;
    Ok(grade)
}

/// What [`grade_model`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelGraded {
    /// A `model/1` grade, stored.
    Graded(Box<Grade>),
    /// Nothing stored: `off` (the use is off, run outputs may not be sent,
    /// or no provider resolves),
    /// `no_criteria` (the playbook has none of its own), or the decider's
    /// reason (`budget`, `send`, `busy`, `state_too_large`) before it
    /// answered any criterion. A stop after the first answer stores the
    /// grade, the remaining criteria `not_enough_data` under that reason.
    Skipped { reason: String },
}

/// Grades the playbook's own criteria of the finished run at `run_dir` with
/// the decision-model grader and stores the grade. Asks a model only when
/// the project's `uses.grade` is not `off`; never on its own.
pub fn grade_model(run_dir: &Path) -> Result<ModelGraded, EngineError> {
    let sources = ended_sources(run_dir)?;
    let skipped = |reason: &str| {
        Ok(ModelGraded::Skipped {
            reason: reason.to_string(),
        })
    };
    let root = crate::run_retro::project_root_of(run_dir).unwrap_or_else(|| run_dir.to_path_buf());
    let Some(decider) = crate::decision::standalone::StandaloneDecider::for_project(&root)
        .filter(|d| {
            d.mode_for(apb_decide::UseSite::Grade) != apb_core::decisions::DecisionMode::Off
        })
        // A grade sends what the run produced, not only prompts.
        .filter(|d| d.sends(apb_core::decisions::SendClass::Outputs))
        .filter(|d| d.available())
    else {
        return skipped("off");
    };
    let grader = model::ModelGrader::new(&decider);
    if !grader::criteria_for(&sources)
        .iter()
        .any(|d| grader.grades(d))
    {
        return skipped("no_criteria");
    }
    let grade = run_grader(&grader, &sources);
    // A stop before any answer stores nothing. A stop part way keeps the
    // answers already paid for: the criteria it stopped before are
    // `not_enough_data` under the stop reason, and a later request grades
    // the run again.
    if let Some(reason) = grader.stopped()
        && !grader.answered_any()
    {
        return skipped(reason);
    }
    store::write(run_dir, &grade)?;
    Ok(ModelGraded::Graded(Box::new(grade)))
}

/// Which grader grades on request (a backfill, `apb grade`, MCP
/// `run_grade`, the dashboard).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraderChoice {
    Rules,
    Model,
}

impl GraderChoice {
    /// `rules` or `model`, as the surfaces name a grader.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "rules" => Some(GraderChoice::Rules),
            "model" => Some(GraderChoice::Model),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            GraderChoice::Rules => "rules",
            GraderChoice::Model => "model",
        }
    }

    pub fn id(self) -> GraderId {
        match self {
            GraderChoice::Rules => GraderId::RULES,
            GraderChoice::Model => GraderId::MODEL,
        }
    }
}

/// The inline grade of a run that is ending now: by rules, when its
/// playbook's `quality:` block asks for it (`grade_on_finish: rules`).
/// `None` for a run that is not graded on finish, and for one whose
/// journal holds its inline grade already (an end replayed after a crash
/// between `run_graded` and `run_finished` grades nothing twice). A rules
/// grade stored before the crash but never journaled (a live run is never
/// graded otherwise) is returned as it is, so the caller journals it.
pub(crate) fn grade_ending(run_dir: &Path, succeeded: bool) -> Result<Option<Grade>, EngineError> {
    let on = crate::legacy_snapshot::load_run_playbook(run_dir)
        .and_then(|p| p.quality)
        .is_some_and(|q| q.grades_on_finish());
    if !on {
        return Ok(None);
    }
    let sources = RunSources::load(run_dir, RunEnd::Ending { succeeded })?;
    if sources
        .run
        .events
        .iter()
        .any(|e| matches!(e.payload, crate::event::EventPayload::RunGraded { .. }))
    {
        return Ok(None);
    }
    if let Some(stored) = store::latest_of(run_dir, GraderId::RULES) {
        return Ok(Some(stored));
    }
    let grade = run_grader(&rules::RulesGrader, &sources);
    store::write(run_dir, &grade)?;
    Ok(Some(grade))
}

/// What [`backfill`] did with one run.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Backfilled {
    pub run_id: String,
    /// The new grade, or absent when the run was skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub grade_id: Option<String>,
    /// Why it was skipped (`graded`, `live`, or the error).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub skipped: Option<String>,
}

/// How many run directories a backfill looks through.
pub const BACKFILL_SCAN: usize = 500;

/// Grades with `grader` up to `last` of the latest ended runs of playbook
/// `id` (of `version` when given) under the project `root` that have no
/// grade of that grader yet, so a comparison has a history. Runs already
/// graded or still live are reported as skipped and do not count toward
/// `last`. A model backfill stops at the first run the decider skips (the
/// use off, the daily cap spent), reported with that reason.
pub fn backfill(
    root: &Path,
    id: &str,
    version: Option<&str>,
    last: usize,
    grader: GraderChoice,
) -> Vec<Backfilled> {
    let dirs: Vec<PathBuf> =
        crate::run_history::runs_of(&[root.to_path_buf()], id, version, BACKFILL_SCAN);
    let mut out = Vec::new();
    let mut tried = 0;
    for d in dirs {
        if tried >= last {
            break;
        }
        let run_id = d
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let skip = |why: &str| Backfilled {
            run_id: run_id.clone(),
            grade_id: None,
            skipped: Some(why.to_string()),
        };
        if store::latest_of(&d, grader.id()).is_some() {
            out.push(skip(SKIP_GRADED));
            continue;
        }
        let events = match crate::run_view::read_events(&d) {
            Ok(events) => events,
            Err(e) => {
                // A journal that does not read is not a live run.
                out.push(skip(&e.to_string()));
                continue;
            }
        };
        let ended = events.iter().any(|e| {
            matches!(
                e.payload,
                crate::event::EventPayload::RunFinished { .. }
                    | crate::event::EventPayload::RunAborted { .. }
            )
        });
        if !ended {
            out.push(skip(SKIP_LIVE));
            continue;
        }
        tried += 1;
        let graded = match grader {
            GraderChoice::Rules => grade_rules(&d).map(|g| ModelGraded::Graded(Box::new(g))),
            GraderChoice::Model => grade_model(&d),
        };
        match graded {
            Ok(ModelGraded::Graded(g)) => out.push(Backfilled {
                run_id: run_id.clone(),
                grade_id: Some(g.grade_id),
                skipped: None,
            }),
            Ok(ModelGraded::Skipped { reason }) => {
                let stops = reason != "no_criteria";
                out.push(skip(&reason));
                if stops {
                    break;
                }
            }
            Err(e) => out.push(skip(&e.to_string())),
        }
    }
    out
}

/// Why a backfill skipped a run that has a grade of its grader.
pub const SKIP_GRADED: &str = "graded";
/// Why a backfill skipped a run that has not ended.
pub const SKIP_LIVE: &str = "live";

#[cfg(test)]
mod tests {
    use super::*;

    const SNAPSHOT: &str = "schema: 2\nid: g\nname: G\nversion: 1.0.0\nquality: {}\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";

    #[test]
    fn an_end_replayed_after_its_inline_grade_grades_nothing_again() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("playbook.yaml"), SNAPSHOT).unwrap();
        let head = r#"{"seq":0,"ts":1,"type":"run_started","playbook":"g","version":"1.0.0"}"#;
        std::fs::write(dir.join("events.jsonl"), format!("{head}\n")).unwrap();
        let first = grade_ending(dir, true).unwrap().expect("graded");
        // The crash came after `run_graded` was journaled: the replayed end
        // finds it and stores no second grade.
        let graded = format!(
            r#"{{"seq":1,"ts":2,"type":"run_graded","grade_id":"{}","grader":"rules/1","levels":{{}}}}"#,
            first.grade_id
        );
        std::fs::write(dir.join("events.jsonl"), format!("{head}\n{graded}\n")).unwrap();
        assert!(grade_ending(dir, true).unwrap().is_none());
        assert_eq!(grades_of(dir).len(), 1);
    }

    #[test]
    fn a_grade_stored_but_never_journaled_is_returned_not_made_again() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("playbook.yaml"), SNAPSHOT).unwrap();
        let head = r#"{"seq":0,"ts":1,"type":"run_started","playbook":"g","version":"1.0.0"}"#;
        std::fs::write(dir.join("events.jsonl"), format!("{head}\n")).unwrap();
        let first = grade_ending(dir, true).unwrap().expect("graded");
        // The crash came after the grade file, before `run_graded`.
        let again = grade_ending(dir, true).unwrap().expect("the stored grade");
        assert_eq!(again.grade_id, first.grade_id);
        assert_eq!(grades_of(dir).len(), 1);
    }
}
