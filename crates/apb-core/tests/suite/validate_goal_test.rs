use apb_core::schema::Playbook;
use apb_core::validate::{Severity, ValidationContext, validate};

const VALID: &str = include_str!("../fixtures/valid.yaml");

fn ctx() -> ValidationContext {
    ValidationContext {
        profiles: vec!["architect".into(), "fullstack".into()],
        ..Default::default()
    }
}

fn error_codes(yaml: &str) -> Vec<&'static str> {
    let playbook = Playbook::from_yaml(yaml).unwrap();
    validate(&playbook, &ctx())
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .map(|i| i.code)
        .collect()
}

fn with_goal(goal_yaml: &str) -> String {
    format!("{goal_yaml}\n{VALID}")
}

#[test]
fn complete_goal_passes() {
    let yaml = with_goal(
        "goal:\n  statement: the invoice is recorded and sent\n  criteria:\n    - description: a row appears in the sheet\n",
    );
    assert!(!error_codes(&yaml).contains(&"V41"));
}

#[test]
fn v41_empty_statement() {
    let yaml =
        with_goal("goal:\n  statement: \"  \"\n  criteria:\n    - description: a row appears\n");
    assert!(error_codes(&yaml).contains(&"V41"));
}

#[test]
fn v41_no_criteria() {
    let yaml = with_goal("goal:\n  statement: the invoice is recorded\n  criteria: []\n");
    assert!(error_codes(&yaml).contains(&"V41"));
}

#[test]
fn v41_empty_criterion_description() {
    let yaml = with_goal(
        "goal:\n  statement: the invoice is recorded\n  criteria:\n    - description: \"\"\n",
    );
    assert!(error_codes(&yaml).contains(&"V41"));
}

#[test]
fn playbook_without_goal_has_no_v41() {
    assert!(!error_codes(VALID).contains(&"V41"));
}

#[test]
fn v41_marker_check_whitespace_only() {
    let yaml = with_goal(
        "goal:\n  statement: the invoice is recorded\n  criteria:\n    - description: a row appears\n      check: { type: marker, marker: \"   \" }\n",
    );
    assert!(error_codes(&yaml).contains(&"V41"));
}

#[test]
fn v41_script_check_path_traversal() {
    let yaml = with_goal(
        "goal:\n  statement: the invoice is recorded\n  criteria:\n    - description: a row appears\n      check: { type: script, path: ../../../etc/passwd }\n",
    );
    assert!(error_codes(&yaml).contains(&"V41"));
}

#[test]
fn v41_script_check_path_outside_scripts_dir() {
    let yaml = with_goal(
        "goal:\n  statement: the invoice is recorded\n  criteria:\n    - description: a row appears\n      check: { type: script, path: checks/ledger.sh }\n",
    );
    assert!(error_codes(&yaml).contains(&"V41"));
}

#[test]
fn v41_script_check_path_under_scripts_dir_passes() {
    let yaml = with_goal(
        "goal:\n  statement: the invoice is recorded\n  criteria:\n    - description: a row appears\n      check: { type: script, path: scripts/check.sh }\n",
    );
    assert!(!error_codes(&yaml).contains(&"V41"));
}

#[test]
fn v41_marker_check_non_empty_passes() {
    let yaml = with_goal(
        "goal:\n  statement: the invoice is recorded\n  criteria:\n    - description: a row appears\n      check: { type: marker, marker: NON_EMPTY }\n",
    );
    assert!(!error_codes(&yaml).contains(&"V41"));
}

#[test]
fn v41_script_check_absolute_path_fails() {
    let yaml = with_goal(
        "goal:\n  statement: the invoice is recorded\n  criteria:\n    - description: a row appears\n      check: { type: script, path: /etc/passwd }\n",
    );
    assert!(error_codes(&yaml).contains(&"V41"));
}

fn warning_codes(yaml: &str) -> Vec<&'static str> {
    let playbook = Playbook::from_yaml(yaml).unwrap();
    validate(&playbook, &ctx())
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Warning)
        .map(|i| i.code)
        .collect()
}

/// `enforce` acts on script and marker criteria only: with manual ones
/// alone it can never fail a run, which V41 warns about.
#[test]
fn v41_warns_when_enforce_has_only_manual_criteria() {
    let manual = with_goal(
        "goal:\n  statement: the invoice is recorded\n  enforce: true\n  criteria:\n    - description: a person checks the sheet\n",
    );
    assert!(warning_codes(&manual).contains(&"V41"));
    assert!(!error_codes(&manual).contains(&"V41"));
    let marker = with_goal(
        "goal:\n  statement: the invoice is recorded\n  enforce: true\n  criteria:\n    - description: a person checks the sheet\n    - { description: it says so, check: { type: marker, marker: RECORDED } }\n",
    );
    assert!(!warning_codes(&marker).contains(&"V41"));
    let reported = with_goal(
        "goal:\n  statement: the invoice is recorded\n  criteria:\n    - description: a person checks the sheet\n",
    );
    assert!(!warning_codes(&reported).contains(&"V41"));
}
