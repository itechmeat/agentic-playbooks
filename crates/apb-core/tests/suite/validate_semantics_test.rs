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

fn warn_codes(yaml: &str) -> Vec<&'static str> {
    let playbook = Playbook::from_yaml(yaml).unwrap();
    validate(&playbook, &ctx())
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Warning)
        .map(|i| i.code)
        .collect()
}

const ISO_WF: &str = r#"schema: 1
id: iso
name: Iso
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", profile: architect, isolation: PLACEHOLDER }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: done }
"#;

#[test]
fn v16_declared_isolation_warns_not_enforced() {
    // full and best_effort - a warning about unenforced isolation.
    for level in ["full", "best_effort"] {
        let yaml = ISO_WF.replace("PLACEHOLDER", level);
        assert!(
            error_codes(&yaml).is_empty(),
            "iso playbook must be structurally valid ({level})"
        );
        assert!(
            warn_codes(&yaml).contains(&"V16"),
            "isolation `{level}` must warn V16"
        );
    }
    // none - no warning (this is the default behavior).
    let yaml = ISO_WF.replace("PLACEHOLDER", "none");
    assert!(
        !warn_codes(&yaml).contains(&"V16"),
        "isolation none must not warn"
    );
}

#[test]
fn v09_condition_not_covering_outcomes() {
    // remove the failure branch from check
    let bad = VALID.replace(
        "  - { from: check, to: fix,  condition: { type: node_status, node: lint, equals: failure } }\n",
        "",
    );
    assert!(error_codes(&bad).contains(&"V09"));
}

#[test]
fn v10_condition_references_downstream_only_node() {
    // the condition references a node that can't be reached before check
    let bad = VALID.replace(
        "condition: { type: node_status, node: lint, equals: success }",
        "condition: { type: node_status, node: done, equals: success }",
    );
    assert!(error_codes(&bad).contains(&"V10"));
}

#[test]
fn v11_cycle_without_max_loops() {
    let bad = VALID.replace("    max_loops: 3\n", "");
    assert!(error_codes(&bad).contains(&"V11"));
}

#[test]
fn v12_script_path_escapes_version_dir() {
    let bad = VALID.replace("scripts/node-lint.sh", "../../etc/passwd");
    assert!(error_codes(&bad).contains(&"V12"));
}

#[test]
fn v13_template_references_unknown_param() {
    let bad = VALID.replace("{{params.task}}", "{{params.ghost}}");
    assert!(error_codes(&bad).contains(&"V13"));
}

#[test]
fn v13_template_references_unknown_node() {
    let bad = VALID.replace("{{nodes.lint.output}}", "{{nodes.ghost.output}}");
    assert!(error_codes(&bad).contains(&"V13"));
}

#[test]
fn v14_unknown_profile_reference() {
    // Explicit `scope: project` - existence is checked by the validator.
    // (`auto`/`global` are deferred to the scope-aware resolver at run start.)
    let bad = VALID.replace(
        "profile: architect",
        "profile: { name: ghost, scope: project }",
    );
    assert!(error_codes(&bad).contains(&"V14"));
}

// V17: an overlong trigger string is rejected as an error (spec 8.5).
const TRIGGER_WF: &str = r#"schema: 1
id: trig
name: Trig
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: done }
"#;

#[test]
fn v17_valid_trigger_passes() {
    let good = TRIGGER_WF.replace(
        "nodes:",
        "trigger:\n  when: [\"use when reviewing the project\"]\n  examples: [\"review the project\"]\nnodes:",
    );
    assert!(!error_codes(&good).contains(&"V17"));
}

#[test]
fn v17_overlong_trigger_line_is_error() {
    let long = "x".repeat(200);
    let bad = TRIGGER_WF.replace("nodes:", &format!("trigger:\n  when: [\"{long}\"]\nnodes:"));
    assert!(error_codes(&bad).contains(&"V17"));
}

#[test]
fn v17_too_many_trigger_items_is_error() {
    let items: Vec<String> = (0..8).map(|i| format!("\"item {i}\"")).collect();
    let bad = TRIGGER_WF.replace(
        "nodes:",
        &format!("trigger:\n  when: [{}]\nnodes:", items.join(", ")),
    );
    assert!(error_codes(&bad).contains(&"V17"));
}
