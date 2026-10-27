//! V102-V107: the `quality:` block's criteria, task type and built-ins, and
//! the supervisor policy that promotes by quality.

use apb_core::schema::Playbook;
use apb_core::validate::{Severity, ValidationContext, validate};

const BASE: &str = "schema: 2\nid: q\nname: Q\nversion: 1.0.0\nparams:\n  - { name: kind, type: text }\n  - { name: token, type: text, secret: true }\nnodes:\n  - { id: start, type: start }\n  - { id: draft, type: agent_task, prompt: hi, profile: x }\n  - { id: check, type: script, script: run.sh, runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: draft }\n  - { from: draft, to: check }\n  - { from: check, to: done }\n";

const LEVELS: &str = "levels: { good: fine, partial: so so, poor: bad }";

fn codes(extra: &str) -> Vec<(&'static str, Severity)> {
    let p = Playbook::from_yaml(&format!("{BASE}{extra}")).unwrap();
    validate(&p, &ValidationContext::default())
        .issues
        .into_iter()
        .filter(|i| i.code >= "V102" && i.code <= "V107" && i.code.len() == 4)
        .map(|i| (i.code, i.severity))
        .collect()
}

fn criterion(body: &str) -> String {
    format!("quality:\n  criteria:\n    - {{ {body} }}\n")
}

#[test]
fn a_valid_block_and_policy_raise_nothing() {
    let extra = format!(
        "quality:\n  task_type: \"{{{{params.kind}}}}\"\n  builtins:\n    scope: {{ off: \"moves the whole tree\" }}\n  criteria:\n    - {{ id: reply-tone, subject: {{ node: draft }}, {LEVELS} }}\nsupervisor:\n  policy:\n    promote_supervisor_patches: {{ when: quality, min_runs: 3 }}\n    trial_candidates: {{ share: 0.5 }}\n"
    );
    assert_eq!(codes(&extra), []);
    assert_eq!(codes(""), []);
}

#[test]
fn v102_a_malformed_criterion() {
    let error = vec![("V102", Severity::Error)];
    for body in [
        format!("id: Bad Id, {LEVELS}"),
        format!("id: loops, {LEVELS}"),
        format!("id: tone, subject: everything, {LEVELS}"),
        "id: tone, levels: { good: fine, partial: so so }".to_string(),
        format!("id: tone, applies_when: \"  \", {LEVELS}"),
        format!(
            "id: tone, levels: {{ good: \"{}\", partial: so so, poor: bad }}",
            "x".repeat(501)
        ),
        "id: tone, levels: { good: \"fine\u{200b}\", partial: so so, poor: bad }".to_string(),
    ] {
        assert_eq!(codes(&criterion(&body)), error, "{body}");
    }
    let twice = format!(
        "quality:\n  criteria:\n    - {{ id: tone, {LEVELS} }}\n    - {{ id: tone, {LEVELS} }}\n"
    );
    assert_eq!(codes(&twice), error);
}

#[test]
fn v103_too_many_criteria_or_a_subject_that_is_not_an_agent_task() {
    let four: String = (0..4)
        .map(|i| format!("    - {{ id: c{i}, {LEVELS} }}\n"))
        .collect();
    assert_eq!(
        codes(&format!("quality:\n  criteria:\n{four}")),
        [("V103", Severity::Error)]
    );
    for node in ["check", "ghost"] {
        assert_eq!(
            codes(&criterion(&format!(
                "id: tone, subject: {{ node: {node} }}, {LEVELS}"
            ))),
            [("V103", Severity::Error)],
            "{node}"
        );
    }
}

#[test]
fn v104_a_task_type_that_reads_more_than_plain_params() {
    for task in [
        "\"{{nodes.draft.output}}\"",
        "\"{{params.missing}}\"",
        "\"{{params.token}}\"",
    ] {
        assert_eq!(
            codes(&format!("quality:\n  task_type: {task}\n")),
            [("V104", Severity::Error)],
            "{task}"
        );
    }
    assert_eq!(codes("quality:\n  task_type: refunds\n"), []);
}

#[test]
fn v105_an_unknown_builtin_or_one_off_without_a_reason() {
    for builtins in ["vibes: { off: no }", "loops: { off: \"\" }"] {
        assert_eq!(
            codes(&format!("quality:\n  builtins:\n    {builtins}\n")),
            [("V105", Severity::Error)],
            "{builtins}"
        );
    }
}

#[test]
fn v106_an_unreadable_policy_warns_that_the_default_applies() {
    for policy in [
        "promote_supervisor_patches: { after_n_successes: many }",
        "trial_candidates: sometimes",
        "trial_candidates: { share: 25 }",
    ] {
        assert_eq!(
            codes(&format!("supervisor:\n  policy:\n    {policy}\n")),
            [("V106", Severity::Warning)],
            "{policy}"
        );
    }
}

#[test]
fn v106_an_unreadable_quality_gate_is_an_error() {
    for gate in [
        "{ when: quality, min_runs: 2 }",
        "{ when: quality, min_runs: 8, max_trials: 4 }",
        "{ when: quality, compre: better }",
    ] {
        assert_eq!(
            codes(&format!(
                "quality: {{}}\nsupervisor:\n  policy:\n    promote_supervisor_patches: {gate}\n"
            )),
            [("V106", Severity::Error)],
            "{gate}"
        );
    }
}

#[test]
fn v107_promotion_by_quality_without_a_quality_block() {
    assert_eq!(
        codes("supervisor:\n  policy:\n    promote_supervisor_patches: { when: quality }\n"),
        [("V107", Severity::Error)]
    );
}

#[test]
fn v107_promotion_by_quality_with_nothing_or_no_outcome_to_compare() {
    const POLICY: &str =
        "supervisor:\n  policy:\n    promote_supervisor_patches: { when: quality }\n";
    let all_off = "quality:\n  builtins:\n    outcome: { off: x }\n    loops: { off: x }\n    scope: { off: x }\n    consent: { off: x }\n    skill_use: { off: x }\n";
    let outcome_off = "quality:\n  builtins:\n    outcome: { off: x }\n";
    for block in [all_off, outcome_off] {
        assert_eq!(
            codes(&format!("{block}{POLICY}")),
            [("V107", Severity::Error)],
            "{block}"
        );
    }
    assert_eq!(
        codes(&format!("quality:\n  grade_on_finish: off\n{POLICY}")),
        [("V107", Severity::Warning)]
    );
}
