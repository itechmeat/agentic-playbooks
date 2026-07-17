use apb_core::schema::Playbook;
use apb_core::validate::{ValidationContext, validate};

#[test]
fn v19_warns_on_task_without_expected_duration_and_does_not_block() {
    let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: f }
"#;
    let pb = Playbook::from_yaml(yaml).unwrap();
    let r = validate(&pb, &ValidationContext::default());
    assert!(r.is_valid(), "V19 must be a warning, not an error");
    assert!(
        r.issues
            .iter()
            .any(|i| i.code == "V19" && i.node.as_deref() == Some("a"))
    );
}

#[test]
fn v20_errors_on_unparsable_expected_duration() {
    let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi, expected_duration: "5x" }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: f }
"#;
    let pb = Playbook::from_yaml(yaml).unwrap();
    let r = validate(&pb, &ValidationContext::default());
    assert!(!r.is_valid());
    assert!(
        r.issues
            .iter()
            .any(|i| i.code == "V20" && i.node.as_deref() == Some("a"))
    );
}

/// An invalid scalar `expected_duration` (float, negative, boolean, ...) loads
/// into the catch-all `ExpectedDuration::Invalid` variant, so the playbook
/// parses and the validator emits a clean V20 instead of failing at load.
fn assert_v20_for_scalar(scalar: &str) {
    let yaml = format!(
        r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: {{ profile: x }}
nodes:
  - {{ id: s, type: start }}
  - {{ id: a, type: agent_task, prompt: hi, expected_duration: {scalar} }}
  - {{ id: f, type: finish, outcome: success }}
edges:
  - {{ from: s, to: a }}
  - {{ from: a, to: f }}
"#
    );
    let pb = Playbook::from_yaml(&yaml)
        .unwrap_or_else(|e| panic!("`{scalar}` must load, not fail at parse: {e}"));
    let r = validate(&pb, &ValidationContext::default());
    assert!(!r.is_valid(), "`{scalar}` must be invalid");
    assert!(
        r.issues
            .iter()
            .any(|i| i.code == "V20" && i.node.as_deref() == Some("a")),
        "`{scalar}` must emit V20"
    );
}

#[test]
fn bare_float_expected_duration_is_v20() {
    assert_v20_for_scalar("1.5");
}

#[test]
fn negative_expected_duration_is_v20() {
    assert_v20_for_scalar("-30");
}

#[test]
fn boolean_expected_duration_is_v20() {
    assert_v20_for_scalar("true");
}

#[test]
fn invalid_expected_duration_serialize_round_trips_the_raw_value() {
    // The Invalid variant keeps the author's raw scalar verbatim, so
    // re-serializing the parsed playbook preserves it (e.g. `1.5`).
    let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi, expected_duration: 1.5 }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: f }
"#;
    let pb = Playbook::from_yaml(yaml).unwrap();
    let node = pb.node("a").unwrap();
    let value = serde_yaml_ng::to_value(node.expected_duration.as_ref().unwrap()).unwrap();
    assert_eq!(value, serde_yaml_ng::Value::from(1.5));
}
