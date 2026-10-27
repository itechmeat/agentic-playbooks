//! Known-problem and grader rules (V108-V111): the `known_problems:` block
//! and who grades the playbook's own quality criteria.

use super::*;
use crate::known_problems::block::{MAX_MAX_SHOWN, MAX_MIN_RUNS, ShowMode};
use crate::quality::criteria::CriteriaGrader;

/// Runs every known-problem and grader rule.
pub(crate) fn check_known_problems(playbook: &Playbook, r: &mut ValidationReport) {
    check_block(playbook, r); // V108, V109
    check_criteria_grader(playbook, r); // V110, V111
}

/// V108: the block reads; V109: something is there to show it to.
fn check_block(playbook: &Playbook, r: &mut ValidationReport) {
    let Some(b) = &playbook.known_problems else {
        return;
    };
    if let ShowMode::Unknown(word) = &b.show {
        r.error(
            "V108",
            None,
            format!("known_problems.show `{word}` is not one of significant, confirmed, off"),
        );
    }
    if !(1..=MAX_MIN_RUNS).contains(&b.min_runs) {
        r.error(
            "V108",
            None,
            format!(
                "known_problems.min_runs is {}; it must be 1 to {MAX_MIN_RUNS}",
                b.min_runs
            ),
        );
    }
    if !(1..=MAX_MAX_SHOWN).contains(&b.max_shown) {
        r.error(
            "V108",
            None,
            format!(
                "known_problems.max_shown is {}; it must be 1 to {MAX_MAX_SHOWN}",
                b.max_shown
            ),
        );
    }
    let has_agent = playbook
        .nodes
        .iter()
        .any(|n| matches!(n.kind, NodeKind::AgentTask { .. }));
    if b.shows() && !has_agent {
        r.warn(
            "V109",
            None,
            "known_problems shows problems to agents, but the playbook has no agent_task node: nothing is shown".into(),
        );
    }
}

/// V110: the grader is known; V111: it has criteria to grade.
fn check_criteria_grader(playbook: &Playbook, r: &mut ValidationReport) {
    let Some(q) = &playbook.quality else {
        return;
    };
    match &q.criteria_grader {
        CriteriaGrader::Unknown(word) => r.error(
            "V110",
            None,
            format!("quality.criteria_grader `{word}` is not one of host, model"),
        ),
        CriteriaGrader::Model if q.criteria.is_empty() => r.warn(
            "V111",
            None,
            "quality.criteria_grader is `model`, but the playbook declares no criteria of its own: the model grader has nothing to grade".into(),
        ),
        _ => {}
    }
}
