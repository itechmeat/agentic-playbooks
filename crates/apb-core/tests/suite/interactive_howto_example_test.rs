//! Task 13: the interactive-nodes YAML example quoted verbatim in
//! `docs/HOWTO-authoring.md`'s "Interactive nodes" section must parse and
//! validate clean (no V31/V32, no errors at all). This fixture is the
//! source of truth for that doc block: keep both in sync by hand.

use apb_core::schema::{AnswerBy, NodeKind, Playbook};
use apb_core::validate::{Severity, ValidationContext, validate};

const INTERACTIVE_EXAMPLE: &str = include_str!("../fixtures/interactive_howto_example.yaml");

#[test]
fn howto_interactive_example_parses() {
    let playbook = Playbook::from_yaml(INTERACTIVE_EXAMPLE).expect("must parse");
    assert_eq!(playbook.id, "deploy-with-confirmation");
    let confirm = playbook
        .nodes
        .iter()
        .find(|n| n.id == "confirm")
        .expect("confirm node");
    match &confirm.kind {
        NodeKind::AgentTask {
            interactive,
            answer_by,
            question_timeout_seconds,
            default_answer,
            ..
        } => {
            assert!(*interactive);
            assert_eq!(*answer_by, AnswerBy::Supervisor);
            assert_eq!(*question_timeout_seconds, Some(900));
            assert_eq!(default_answer.as_deref(), Some("abort"));
        }
        other => panic!("expected agent_task, got {other:?}"),
    }
}

#[test]
fn howto_interactive_example_validates_clean() {
    let playbook = Playbook::from_yaml(INTERACTIVE_EXAMPLE).expect("must parse");
    let report = validate(&playbook, &ValidationContext::default());
    let errors: Vec<_> = report
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "expected no errors, got {errors:?}");
    assert!(
        !report.issues.iter().any(|i| i.code == "V31"),
        "unexpected V31 on a node marked interactive: true"
    );
    assert!(
        !report.issues.iter().any(|i| i.code == "V32"),
        "unexpected V32: default_answer has a question_timeout_seconds"
    );
}
