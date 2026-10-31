//! V108-V111: the `known_problems:` block and who grades the playbook's own
//! quality criteria.

use apb_core::schema::Playbook;
use apb_core::validate::{Severity, ValidationContext, validate};

const AGENT: &str = "schema: 2\nid: k\nname: K\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: triage, type: agent_task, prompt: hi, profile: x }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: triage }\n  - { from: triage, to: done }\n";

const SCRIPT_ONLY: &str = "schema: 2\nid: k\nname: K\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: check, type: script, script: run.sh, runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: check }\n  - { from: check, to: done }\n";

fn codes(base: &str, extra: &str) -> Vec<(&'static str, Severity)> {
    let p = Playbook::from_yaml(&format!("{base}{extra}")).unwrap();
    validate(&p, &ValidationContext::default())
        .issues
        .into_iter()
        .filter(|i| ["V108", "V109", "V110", "V111"].contains(&i.code))
        .map(|i| (i.code, i.severity))
        .collect()
}

#[test]
fn valid_blocks_raise_nothing_and_old_playbooks_are_unchanged() {
    assert_eq!(codes(AGENT, ""), []);
    assert_eq!(codes(AGENT, "known_problems: {}\n"), []);
    assert_eq!(
        codes(
            AGENT,
            "known_problems: { show: confirmed, min_runs: 3, max_shown: 10 }\n"
        ),
        []
    );
    assert_eq!(
        codes(
            AGENT,
            "quality:\n  criteria_grader: model\n  criteria:\n    - { id: tone, levels: { good: a, partial: b, poor: c } }\n"
        ),
        []
    );
    let old = Playbook::from_yaml(AGENT).unwrap();
    assert!(old.known_problems.is_none());
    // A block-less playbook serializes without the field.
    let yaml = yaml_serde::to_string(&old).unwrap();
    assert!(!yaml.contains("known_problems"), "{yaml}");
}

#[test]
fn v108_a_malformed_block() {
    let error = vec![("V108", Severity::Error)];
    for extra in [
        "known_problems: { show: always }\n",
        "known_problems: { min_runs: 0 }\n",
        "known_problems: { min_runs: 21 }\n",
        "known_problems: { max_shown: 0 }\n",
        "known_problems: { max_shown: 11 }\n",
    ] {
        assert_eq!(codes(AGENT, extra), error, "{extra}");
    }
}

#[test]
fn v109_nothing_to_show_it_to() {
    assert_eq!(
        codes(SCRIPT_ONLY, "known_problems: {}\n"),
        [("V109", Severity::Warning)]
    );
    assert_eq!(codes(SCRIPT_ONLY, "known_problems: { show: off }\n"), []);
    // An unknown word is V108 alone, not also a V109 about showing.
    assert_eq!(
        codes(SCRIPT_ONLY, "known_problems: { show: always }\n"),
        [("V108", Severity::Error)]
    );
}

#[test]
fn v110_and_v111_the_criteria_grader() {
    assert_eq!(
        codes(AGENT, "quality:\n  criteria_grader: robot\n"),
        [("V110", Severity::Error)]
    );
    assert_eq!(
        codes(AGENT, "quality:\n  criteria_grader: model\n"),
        [("V111", Severity::Warning)]
    );
    assert_eq!(codes(AGENT, "quality:\n  criteria_grader: host\n"), []);
}
