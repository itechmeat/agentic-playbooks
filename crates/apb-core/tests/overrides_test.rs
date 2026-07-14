use apb_core::overrides::RunOverrides;
use apb_core::schema::{NodeKind, Playbook};

const PLAYBOOK: &str = r#"schema: 2
id: ov
name: Ov
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: a, type: agent_task, prompt: "a" }
  - { id: b, type: agent_task, prompt: "b", profile: main }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: b }
  - { from: b, to: done }
"#;

#[test]
fn apply_overrides_selects_other_profile_for_node() {
    let mut playbook = Playbook::from_yaml(PLAYBOOK).unwrap();
    let ov = RunOverrides::from_yaml("nodes:\n  b: { profile: { name: fast, scope: global } }\n")
        .unwrap();
    assert!(!ov.is_empty());
    ov.apply(&mut playbook).unwrap();

    // Node b got a different profile; node a stayed on defaults.
    let b = playbook.nodes.iter().find(|n| n.id == "b").unwrap();
    match &b.kind {
        NodeKind::AgentTask {
            profile: Some(p), ..
        } => {
            assert_eq!(p.name, "fast");
            assert_eq!(p.scope, apb_core::profile::ProfileScope::Global);
        }
        other => panic!("node b profile not overridden: {other:?}"),
    }
    let a = playbook.nodes.iter().find(|n| n.id == "a").unwrap();
    assert!(
        matches!(&a.kind, NodeKind::AgentTask { profile: None, .. }),
        "node a must keep its (default) profile"
    );
}

#[test]
fn empty_overrides_leave_playbook_unchanged() {
    let mut playbook = Playbook::from_yaml(PLAYBOOK).unwrap();
    let ov = RunOverrides::default();
    assert!(ov.is_empty());
    ov.apply(&mut playbook).unwrap();
    let b = playbook.nodes.iter().find(|n| n.id == "b").unwrap();
    assert!(matches!(&b.kind, NodeKind::AgentTask { profile: Some(p), .. } if p.name == "main"));
}

#[test]
fn override_for_unknown_node_is_error() {
    let mut playbook = Playbook::from_yaml(PLAYBOOK).unwrap();
    let ov = RunOverrides::from_yaml("nodes:\n  ghost: { profile: fast }\n").unwrap();
    let err = ov.apply(&mut playbook).unwrap_err();
    assert!(err.contains("unknown node `ghost`"), "got: {err}");
}

#[test]
fn ephemeral_executor_parses() {
    let ov = RunOverrides::from_yaml(
        "nodes:\n  b:\n    ephemeral_executor: { agent: codex, model: o1 }\n",
    )
    .unwrap();
    let no = ov.nodes.get("b").expect("node b override");
    let e = no
        .ephemeral_executor
        .as_ref()
        .expect("ephemeral executor parsed");
    assert_eq!(e.agent, "codex");
    assert_eq!(e.model, "o1");
    assert!(no.profile.is_none());
}

#[test]
fn override_for_non_agent_node_is_error() {
    let mut playbook = Playbook::from_yaml(PLAYBOOK).unwrap();
    // `start` exists, but it's not an agent_task.
    let ov = RunOverrides::from_yaml("nodes:\n  start: { profile: fast }\n").unwrap();
    let err = ov.apply(&mut playbook).unwrap_err();
    assert!(err.contains("not an agent_task"), "got: {err}");
}
