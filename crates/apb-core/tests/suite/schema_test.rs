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

const EXTRACT_WF: &str = r#"schema: 2
id: extract
name: Extract
version: 1.0.0
nodes:
  - { id: start, type: start }
  - id: work
    type: agent_task
    prompt: "do it and wrap the result in <node_output>...</node_output>"
    outputs:
      extract: node_output
  - { id: plain, type: agent_task, prompt: "no contract" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: plain }
  - { from: plain, to: done }
"#;

#[test]
fn outputs_extract_parses_and_round_trips_and_omits_when_absent() {
    let playbook = Playbook::from_yaml(EXTRACT_WF).unwrap();
    let work = playbook.node("work").unwrap();
    assert_eq!(
        work.outputs.as_ref().and_then(|o| o.extract.as_deref()),
        Some("node_output")
    );
    // A node without the contract carries no extract marker.
    let plain = playbook.node("plain").unwrap();
    assert!(
        plain
            .outputs
            .as_ref()
            .and_then(|o| o.extract.as_deref())
            .is_none()
    );

    // Round-trip: the marker survives serialization and is omitted when absent
    // (skip_serializing_if), so existing playbooks stay byte-identical.
    let yaml = serde_yaml_ng::to_string(&playbook).unwrap();
    assert!(
        yaml.contains("extract: node_output"),
        "set value must round-trip"
    );
    let reparsed = Playbook::from_yaml(&yaml).unwrap();
    assert_eq!(
        reparsed
            .node("work")
            .unwrap()
            .outputs
            .as_ref()
            .and_then(|o| o.extract.as_deref()),
        Some("node_output")
    );
}

const HUMAN_REVIEW_PROMPT_WF: &str = r#"
schema: 2
id: rev
name: Review
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: start, type: start }
  - { id: gate, type: human_review, options: [approve, reject], prompt: "Check the changelog before deciding." }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: gate }
  - { from: gate, to: done }
"#;

/// issue #102.9: `human_review.prompt` is optional guidance for the reviewer,
/// parses when set, is absent (not an error) when omitted, and round-trips
/// through YAML unchanged.
#[test]
fn human_review_prompt_parses_and_round_trips() {
    let playbook = Playbook::from_yaml(HUMAN_REVIEW_PROMPT_WF).unwrap();
    match &playbook.node("gate").unwrap().kind {
        NodeKind::HumanReview { options, prompt } => {
            assert_eq!(options, &vec!["approve".to_string(), "reject".to_string()]);
            assert_eq!(
                prompt.as_deref(),
                Some("Check the changelog before deciding.")
            );
        }
        other => panic!("expected human_review, got {other:?}"),
    }

    let yaml = serde_yaml_ng::to_string(&playbook).unwrap();
    assert!(
        yaml.contains("Check the changelog before deciding."),
        "prompt must round-trip, got:\n{yaml}"
    );
    let reparsed = Playbook::from_yaml(&yaml).unwrap();
    match &reparsed.node("gate").unwrap().kind {
        NodeKind::HumanReview { prompt, .. } => {
            assert_eq!(
                prompt.as_deref(),
                Some("Check the changelog before deciding.")
            );
        }
        other => panic!("expected human_review, got {other:?}"),
    }
}

const HUMAN_REVIEW_NO_PROMPT_WF: &str = r#"
schema: 2
id: rev
name: Review
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: start, type: start }
  - { id: gate, type: human_review, options: [approve, reject] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: gate }
  - { from: gate, to: done }
"#;

/// A `human_review` node without `prompt:` parses fine (the field is
/// optional), carries `None`, and - because a prompt-less playbook is
/// re-serialized on real paths (`versioning::playbook_yaml_for_version`,
/// `playbook_create`'s canonical write, the run-snapshot writer) - must NOT
/// grow a `prompt: null` line on round-trip (review fix round 1: the field
/// needs `skip_serializing_if`, not just `#[serde(default)]`, or every
/// existing prompt-less `human_review` node in a playbook picks one up the
/// next time it is written).
#[test]
fn human_review_without_prompt_is_none() {
    let playbook = Playbook::from_yaml(HUMAN_REVIEW_NO_PROMPT_WF).unwrap();
    match &playbook.node("gate").unwrap().kind {
        NodeKind::HumanReview { prompt, .. } => assert_eq!(*prompt, None),
        other => panic!("expected human_review, got {other:?}"),
    }

    // Check the `gate` node's own mapping only - the playbook's `done` node is
    // a `finish` node whose *own* `prompt` field (a distinct, pre-existing,
    // out-of-scope field) legitimately serializes as `prompt: null` today, so
    // a whole-document substring check would false-positive on that sibling.
    let yaml = serde_yaml_ng::to_string(&playbook).unwrap();
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml).unwrap();
    let gate_node = value["nodes"]
        .as_sequence()
        .unwrap()
        .iter()
        .find(|n| n["id"].as_str() == Some("gate"))
        .expect("gate node must be present");
    assert!(
        gate_node.as_mapping().unwrap().get("prompt").is_none(),
        "a prompt-less human_review node must not emit a `prompt` key, got:\n{yaml}"
    );
    let reparsed = Playbook::from_yaml(&yaml).unwrap();
    match &reparsed.node("gate").unwrap().kind {
        NodeKind::HumanReview { prompt, .. } => assert_eq!(*prompt, None),
        other => panic!("expected human_review, got {other:?}"),
    }
}
