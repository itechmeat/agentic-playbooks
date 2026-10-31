//! Quality rules (V102-V107): the `quality:` block's criteria, its task
//! type and built-ins, and the supervisor policy that promotes by quality.

use super::*;
use crate::candidate::policy::{
    PROMOTE_SUPERVISOR_PATCHES, PromotePolicy, asks_for_quality, policy_value, promote_policy,
    unreadable,
};
use crate::quality::criteria::{
    BUILTIN_IDS, Criterion, ID_MAX, MAX_CRITERIA, OUTCOME, QualityBlock, Subject, TEXT_MAX,
};

/// Runs every quality rule.
pub(crate) fn check_quality(playbook: &Playbook, r: &mut ValidationReport) {
    check_policy(playbook, r); // V106, V107
    let Some(q) = &playbook.quality else {
        return;
    };
    check_criteria(playbook, q, r); // V102, V103
    check_task_type(playbook, q, r); // V104
    check_builtins(q, r); // V105
}

fn valid_id(id: &str) -> bool {
    (1..=ID_MAX).contains(&id.len())
        && id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Why a criterion text is malformed, if it is.
fn text_problem(name: &str, text: &str) -> Option<String> {
    if text.trim().is_empty() {
        Some(format!("{name} is empty"))
    } else if text.chars().count() > TEXT_MAX {
        Some(format!("{name} is longer than {TEXT_MAX} characters"))
    } else if crate::text::has_hidden(text) {
        Some(format!("{name} holds a hidden character"))
    } else {
        None
    }
}

/// The V102 problems of one criterion.
fn criterion_problems(c: &Criterion) -> Vec<String> {
    let mut out = Vec::new();
    if !valid_id(&c.id) {
        out.push(format!(
            "id must match [a-z0-9][a-z0-9_-]* and be at most {ID_MAX} characters"
        ));
    }
    if BUILTIN_IDS.contains(&c.id.as_str()) {
        out.push("id is the name of a built-in criterion".into());
    }
    if let Subject::Named(word) = &c.subject
        && word != "run"
    {
        out.push(format!(
            "subject `{word}` is neither `run` nor `{{ node: <id> }}`"
        ));
    }
    for (level, text) in c.levels.named() {
        out.extend(text_problem(&format!("level `{level}`"), text));
    }
    if let Some(text) = &c.applies_when {
        out.extend(text_problem("`applies_when`", text));
    }
    out
}

fn check_criteria(playbook: &Playbook, q: &QualityBlock, r: &mut ValidationReport) {
    let mut seen = std::collections::BTreeSet::new();
    for c in &q.criteria {
        for problem in criterion_problems(c) {
            r.error(
                "V102",
                None,
                format!("quality criterion `{}`: {problem}", c.id),
            );
        }
        if !seen.insert(c.id.as_str()) {
            r.error(
                "V102",
                None,
                format!("quality criterion `{}` is declared twice", c.id),
            );
        }
        if let Subject::Node { node } = &c.subject
            && !playbook
                .node(node)
                .is_some_and(|n| matches!(n.kind, NodeKind::AgentTask { .. }))
        {
            r.error(
                "V103",
                Some(node),
                format!(
                    "quality criterion `{}` is about node `{node}`, which is not an agent_task of this playbook",
                    c.id
                ),
            );
        }
    }
    if q.criteria.len() > MAX_CRITERIA {
        r.error(
            "V103",
            None,
            format!(
                "quality declares {} criteria; at most {MAX_CRITERIA} besides the built-ins",
                q.criteria.len()
            ),
        );
    }
}

/// V104: the task type is known at start and is never a secret.
fn check_task_type(playbook: &Playbook, q: &QualityBlock, r: &mut ValidationReport) {
    let Some(text) = &q.task_type else {
        return;
    };
    for cap in crate::template::refs(text) {
        let param = match cap.split('.').collect::<Vec<&str>>().as_slice() {
            ["params", p] => playbook.params.iter().find(|d| d.name == *p),
            _ => None,
        };
        match param {
            None => r.error(
                "V104",
                None,
                format!("quality task_type `{{{{{cap}}}}}` is not allowed; a task type may read params.* only"),
            ),
            Some(p) if p.secret => r.error(
                "V104",
                None,
                format!(
                    "quality task_type reads the secret param `{}`; a task type is stored with every grade",
                    p.name
                ),
            ),
            Some(_) => {}
        }
    }
}

/// V105: a built-in setting names a built-in, and switching one off says why.
fn check_builtins(q: &QualityBlock, r: &mut ValidationReport) {
    for (name, setting) in &q.builtins {
        if !BUILTIN_IDS.contains(&name.as_str()) {
            r.error(
                "V105",
                None,
                format!(
                    "quality.builtins names `{name}`, which is not a built-in ({})",
                    BUILTIN_IDS.join(", ")
                ),
            );
        } else if setting.off.as_deref().is_some_and(|w| w.trim().is_empty()) {
            r.error(
                "V105",
                None,
                format!("quality.builtins.{name} is switched off without a reason"),
            );
        }
    }
}

/// V106: a policy value that does not read falls back to its default, a
/// warning; for a quality gate it is an error (the gate falls back to
/// `manual`, never to promoting on successes). V107: promotion by quality
/// needs a `quality:` block, criteria in force and `outcome` among them;
/// with grading on finish switched off no trial is graded unless someone
/// grades it (a warning).
fn check_policy(playbook: &Playbook, r: &mut ValidationReport) {
    for (key, why) in unreadable(playbook) {
        let message = format!("supervisor.policy.{key} does not read ({why})");
        let quality_gate = key == PROMOTE_SUPERVISOR_PATCHES
            && policy_value(playbook, key).is_some_and(asks_for_quality);
        if quality_gate {
            r.error(
                "V106",
                None,
                format!("{message}; a person decides every candidate until it does"),
            );
        } else {
            r.warn("V106", None, format!("{message}; the default applies"));
        }
    }
    if !matches!(promote_policy(playbook), PromotePolicy::Quality(_)) {
        return;
    }
    const AT: &str = "supervisor.policy.promote_supervisor_patches is `when: quality`";
    let Some(q) = &playbook.quality else {
        r.error(
            "V107",
            None,
            format!("{AT} but the playbook has no `quality:` block"),
        );
        return;
    };
    let defs = q.criteria_defs();
    if defs.is_empty() {
        r.error(
            "V107",
            None,
            format!("{AT} but no criterion is in force: there is nothing to compare"),
        );
    } else if !defs.iter().any(|d| d.id() == OUTCOME) {
        r.error(
            "V107",
            None,
            format!("{AT} but the built-in `outcome` is switched off: a candidate is never compared without whether its runs succeeded"),
        );
    }
    if !q.grades_on_finish() {
        r.warn(
            "V107",
            None,
            format!("{AT} but `grade_on_finish` is off: no trial is graded unless someone grades it, so every candidate waits for a person"),
        );
    }
}
