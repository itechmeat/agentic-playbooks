//! Known problems on the surfaces: a playbook's problems (MCP
//! `playbook_known_problems`, `apb problems`, `GET
//! /api/playbooks/{id}/known-problems`), a person's decision on one
//! (confirm, dismiss, reopen, close with a quote), a recheck and a backfill
//! scan. Every surface goes through this module, so every act that writes
//! the store is refused from a run step here, once ([`super::outside_run`]):
//! a run never decides, rechecks or collects its own playbook's problems.
//! Reading is allowed anywhere. The logic is [`apb_engine::known_problems`].
//!
//! A host session relays a person's word over MCP, but in host execution
//! mode the same session does the run's work, and its MCP server has no run
//! step variable to tell it apart. So MCP decisions and rechecks of a
//! playbook are refused while a run of it waits on a host task
//! ([`outside_host_task`]). The HTTP API is not guarded against local
//! callers beyond the run-step variables of its own process: it is the
//! dashboard's, and anything that can reach it locally acts as the person
//! at the keyboard.

use std::path::Path;

use apb_core::known_problems::recheck::{ProposedVerdict, RecheckProposal};
use apb_core::known_problems::{KnownProblem, ProblemStatus, RecheckRecord};
use apb_core::scope::Scope;
use apb_core::step_env::StepCaller;
use apb_engine::known_problems::{
    self as kp, ChangedPassage, ProblemAction, ProblemView, ProblemsView, RecheckBy, ScanReport,
    SourceNow,
};
use serde::Serialize;

use super::{ToolError, outside_run};

/// What a refusal from a run step names.
const DECIDING: &str = "deciding a known problem";
const RECHECKING: &str = "rechecking a known problem";
const SCANNING: &str = "collecting known problems";

const HOST_TASK_OPEN: &str = "a run of this playbook waits on a host task, so this session is doing its work: a decision on its known problems is a person's, on the dashboard or with apb problems";

/// Refuses an MCP decision or recheck on `t` while a run of it waits on a
/// host task (see the module docs).
pub fn outside_host_task(root: &Path, t: &Target) -> Result<(), ToolError> {
    match kp::run_waiting_on_host(root, t.scope, &t.id) {
        Some(run) => Err(ToolError::Conflict(format!(
            "{HOST_TASK_OPEN} (run {run}; if nothing drives that run any more, stop it with apb stop {run})"
        ))),
        None => Ok(()),
    }
}

/// The longest reason a person's decision keeps.
pub const REASON_MAX: usize = 300;

/// The playbook a known-problem call is about: its id and the scope its
/// problems are stored under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub id: String,
    pub scope: Scope,
}

impl Target {
    /// Playbook `id` of `scope` (`project` or `global`); without a scope,
    /// where the playbook lives ([`kp::scope_for`]).
    pub fn resolve(root: &Path, id: &str, scope: Option<&str>) -> Result<Self, ToolError> {
        if !apb_core::registry::is_safe_segment(id) {
            return Err(ToolError::NotFound(format!("playbook `{id}`")));
        }
        let scope = match scope {
            None => kp::scope_for(root, id),
            Some(s) => Scope::parse(s).ok_or_else(|| {
                ToolError::Engine(format!("unknown scope `{s}`: use project or global"))
            })?,
        };
        Ok(Target {
            id: id.to_string(),
            scope,
        })
    }
}

/// A playbook's known problems as every surface shows them. Read-only.
pub fn problems_view(root: &Path, t: &Target) -> Result<ProblemsView, ToolError> {
    Ok(kp::view_current(root, t.scope, &t.id)?)
}

/// One problem and the passages of its source that are new since it was
/// seen: what a host or a person quotes from to close it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProblemDetail {
    pub problem: ProblemView,
    pub changed_passages: Vec<ChangedPassage>,
}

/// Problem `problem` of the playbook with its changed passages. Read-only.
pub fn problem_detail(root: &Path, t: &Target, problem: &str) -> Result<ProblemDetail, ToolError> {
    let view = problems_view(root, t)?;
    let found = view
        .problems
        .into_iter()
        .find(|p| p.problem.id == problem)
        .ok_or_else(|| ToolError::NotFound(format!("known problem `{problem}`")))?;
    let changed_passages = kp::changed_passages(root, t.scope, &t.id, problem)?;
    Ok(ProblemDetail {
        problem: found,
        changed_passages,
    })
}

/// A person's reason as the store keeps it: one line without hidden
/// characters, at most [`REASON_MAX`] characters.
fn clean_reason(reason: Option<&str>) -> String {
    let line = apb_core::text::replace_hidden(reason.unwrap_or_default(), ' ');
    line.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(REASON_MAX)
        .collect()
}

/// A person's decision on problem `problem`: confirm, dismiss, reopen, or
/// close with a quote of the source's new text. `by` names the surface
/// (`cli`, `mcp`, `dashboard`). Refused from a run step.
pub fn problem_decide(
    root: &Path,
    t: &Target,
    problem: &str,
    action: ProblemAction,
    reason: Option<&str>,
    by: &str,
    step: &StepCaller,
) -> Result<KnownProblem, ToolError> {
    outside_run(step, DECIDING)?;
    Ok(kp::decide(
        root,
        t.scope,
        &t.id,
        problem,
        action,
        by,
        &clean_reason(reason),
    )?)
}

/// A person's action from its surface name (`confirm`, `dismiss`,
/// `reopen`, `close`); a close needs `quote`.
pub fn action_of(name: &str, quote: Option<String>) -> Result<ProblemAction, ToolError> {
    match (name, quote) {
        ("confirm", _) => Ok(ProblemAction::Confirm),
        ("dismiss", _) => Ok(ProblemAction::Dismiss),
        ("reopen", _) => Ok(ProblemAction::Reopen),
        ("close", Some(q)) if !q.trim().is_empty() => Ok(ProblemAction::Close { quote: q }),
        ("close", _) => Err(ToolError::Engine(
            "a close needs quote: text copied exactly from the new source that shows the fix"
                .into(),
        )),
        (other, _) => Err(ToolError::Engine(format!(
            "unknown action `{other}`: use confirm, dismiss, reopen or close"
        ))),
    }
}

/// A host's or a person's proposal from surface arguments.
pub fn proposal_of(
    verdict: &str,
    quote: Option<String>,
    why: Option<String>,
) -> Result<RecheckProposal, ToolError> {
    let verdict = match verdict {
        "holds" => ProposedVerdict::Holds,
        "closed" => ProposedVerdict::Closed,
        "undetermined" => ProposedVerdict::Undetermined,
        other => {
            return Err(ToolError::Engine(format!(
                "unknown verdict `{other}`: use holds, closed or undetermined"
            )));
        }
    };
    Ok(RecheckProposal {
        verdict,
        quote,
        why: why.unwrap_or_default(),
    })
}

/// A person's word that a problem still holds after its source changed.
pub fn person_holds() -> RecheckBy {
    RecheckBy::Person(RecheckProposal {
        verdict: ProposedVerdict::Holds,
        quote: None,
        why: String::new(),
    })
}

/// Rechecks problem `problem` now, by `by` (the project's decision model, a
/// host's proposal, or a person), and stores the record. Refused from a run
/// step.
pub fn problem_recheck(
    root: &Path,
    t: &Target,
    problem: &str,
    by: RecheckBy,
    surface: &str,
    step: &StepCaller,
) -> Result<RecheckRecord, ToolError> {
    outside_run(step, RECHECKING)?;
    Ok(kp::recheck_one(root, t.scope, &t.id, problem, by, surface)?)
}

/// One problem a recheck of every changed source looked at.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Rechecked {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<RecheckRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The open problems whose source changed since they were last checked.
pub fn changed(view: &ProblemsView) -> Vec<&ProblemView> {
    view.problems
        .iter()
        .filter(|p| {
            p.problem.status == ProblemStatus::Open && p.source_now == Some(SourceNow::Changed)
        })
        .collect()
}

/// Rechecks by the project's decision model every open problem whose source
/// changed. Refused up front when no model is on for rechecks; stops at the
/// first the model does not answer (over the daily cap, busy, failed); any
/// other error is recorded for its problem and the rest go on. Refused from
/// a run step.
pub fn recheck_changed_by_model(
    root: &Path,
    t: &Target,
    surface: &str,
    step: &StepCaller,
) -> Result<Vec<Rechecked>, ToolError> {
    outside_run(step, RECHECKING)?;
    let view = problems_view(root, t)?;
    let changed = changed(&view);
    if !changed.is_empty() && kp::recheck::decider(root).is_none() {
        return Err(ToolError::Conflict(kp::recheck::MODEL_OFF.into()));
    }
    let mut out = Vec::new();
    for p in changed {
        let id = p.problem.id.clone();
        match kp::recheck_one(root, t.scope, &t.id, &id, RecheckBy::Model, surface) {
            Ok(check) => out.push(Rechecked {
                id,
                check: Some(check),
                error: None,
            }),
            Err(e) => {
                let silent = kp::recheck::is_model_silent(&e);
                out.push(Rechecked {
                    id,
                    check: None,
                    error: Some(e.to_string()),
                });
                if silent {
                    break;
                }
            }
        }
    }
    Ok(out)
}

/// Collects the problems of up to `last` (at most
/// [`apb_core::known_problems::OBSERVED_RUNS_KEPT`]) of the latest ended
/// runs of the playbook of `t`'s scope (a backfill: a run observed before
/// adds nothing again). Refused from a run step.
pub fn problems_scan(
    root: &Path,
    t: &Target,
    last: usize,
    step: &StepCaller,
) -> Result<ScanReport, ToolError> {
    outside_run(step, SCANNING)?;
    Ok(kp::scan(root, t.scope, &t.id, last)?)
}

/// The run surfaces' `known_problems` block: what the run showed its agents
/// and what it hid; absent for a run whose playbook shows nothing.
pub fn run_block(events: &[apb_engine::event::Event]) -> Option<kp::RunKnownProblems> {
    kp::of_run(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reason_is_one_short_line() {
        assert_eq!(clean_reason(None), "");
        assert_eq!(clean_reason(Some("a\nb\u{202e} c")), "a b c");
        assert_eq!(
            clean_reason(Some(&"x".repeat(REASON_MAX + 9)))
                .chars()
                .count(),
            REASON_MAX
        );
    }

    #[test]
    fn an_action_and_a_verdict_take_known_names_only() {
        assert_eq!(action_of("confirm", None).unwrap(), ProblemAction::Confirm);
        assert!(action_of("close", None).is_err());
        assert!(action_of("close", Some("  ".into())).is_err());
        assert!(matches!(
            action_of("close", Some("fixed in v2 of the API".into())).unwrap(),
            ProblemAction::Close { .. }
        ));
        assert!(action_of("delete", None).is_err());
        assert_eq!(
            proposal_of("holds", None, None).unwrap().verdict,
            ProposedVerdict::Holds
        );
        assert!(proposal_of("fixed", None, None).is_err());
    }
}
