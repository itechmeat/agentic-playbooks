use apb_core::schema::{NodeKind, Playbook};

const VALID: &str = include_str!("../fixtures/valid.yaml");

#[test]
fn parses_valid_playbook() {
    let playbook = Playbook::from_yaml(VALID).expect("must parse");
    assert_eq!(playbook.id, "implement-task");
    assert_eq!(playbook.version, "1.0.0");
    assert_eq!(playbook.nodes.len(), 6);
    assert_eq!(playbook.edges.len(), 6);
    assert!(matches!(playbook.nodes[0].kind, NodeKind::Start));
    match &playbook.nodes[1].kind {
        NodeKind::AgentTask {
            prompt, profile, ..
        } => {
            assert!(prompt.contains("{{params.task}}"));
            // The string YAML form `profile: architect` parses as a ref with scope auto.
            assert_eq!(profile.as_ref().map(|p| p.name.as_str()), Some("architect"));
            assert_eq!(
                profile.as_ref().map(|p| p.scope),
                Some(apb_core::profile::ProfileScope::Auto)
            );
        }
        other => panic!("expected agent_task, got {other:?}"),
    }
    assert_eq!(
        playbook.defaults.profile.as_ref().map(|p| p.name.as_str()),
        Some("architect")
    );
}

#[test]
fn rejects_unknown_node_type() {
    let bad = VALID.replace("type: start", "type: warp");
    let err = Playbook::from_yaml(&bad).unwrap_err();
    assert!(err.to_string().contains("warp"));
}

#[test]
fn json_uses_snake_case_type_tag() {
    let playbook = Playbook::from_yaml(VALID).unwrap();
    let json = serde_json::to_value(&playbook).unwrap();
    assert_eq!(json["nodes"][1]["type"], "agent_task");
}

const EDGE_WF: &str = r#"schema: 2
id: loop
name: Loop
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a, max_traversals: 3 }
  - { from: a, to: done }
"#;

#[test]
fn edge_max_traversals_round_trips_and_omits_when_absent() {
    let playbook = Playbook::from_yaml(EDGE_WF).unwrap();
    assert_eq!(playbook.edges[0].max_traversals, Some(3));
    assert_eq!(playbook.edges[1].max_traversals, None);

    // Serialize back to YAML: the field is preserved when set and omitted when
    // absent (skip_serializing_if).
    let yaml = serde_yaml_ng::to_string(&playbook).unwrap();
    assert!(
        yaml.contains("max_traversals: 3"),
        "set value must round-trip"
    );
    let reparsed = Playbook::from_yaml(&yaml).unwrap();
    assert_eq!(reparsed.edges[0].max_traversals, Some(3));
    assert_eq!(reparsed.edges[1].max_traversals, None);

    // The absent edge (index 1) must not emit a max_traversals key.
    let json = serde_json::to_value(&playbook).unwrap();
    assert!(json["edges"][1].get("max_traversals").is_none());
}
