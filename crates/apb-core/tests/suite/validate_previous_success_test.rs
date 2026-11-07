//! The `run.previous_success` namespace: V176 refuses a malformed
//! reference, an unknown node, a worktree read, and a `when_none` value
//! that is neither an RFC 3339 time nor a template over `params.*` only,
//! and a `when_none` template that reads a short secret param (its rendered
//! time would be journaled). A read of the namespace is a guarded
//! feature.

use apb_core::schema::Playbook;
use apb_core::validate::{ValidationContext, validate};

fn ctx() -> ValidationContext {
    ValidationContext {
        profiles: vec!["dev".into()],
        ..Default::default()
    }
}

fn issues(nodes: &str, extra: &str) -> Vec<String> {
    let yaml = build_yaml(nodes, extra);
    let playbook = Playbook::from_yaml(&yaml).unwrap();
    validate(&playbook, &ctx())
        .issues
        .iter()
        .filter(|i| i.code == "V176")
        .map(|i| i.message.clone())
        .collect()
}

fn build_yaml(nodes: &str, extra: &str) -> String {
    format!(
        "schema: 2\nid: p\nname: P\nversion: 1.0.0\n{extra}defaults: {{ profile: dev }}\nnodes:\n  - {{ id: start, type: start }}\n{nodes}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n"
    )
}

fn v176(nodes: &str, extra: &str) -> bool {
    !issues(nodes, extra).is_empty()
}

#[test]
fn an_unknown_key_is_v176_and_not_v13() {
    let got = issues(
        "  - { id: read, type: agent_task, prompt: \"{{run.previous_success.ended}}\" }\n",
        "",
    );
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].contains("unknown key"), "{got:?}");
    // The grammar defers to V176: no V13 for the same reference.
    let yaml = build_yaml(
        "  - { id: read, type: agent_task, prompt: \"{{run.previous_success.ended}}\" }\n",
        "",
    );
    let playbook = Playbook::from_yaml(&yaml).unwrap();
    let codes: Vec<&str> = validate(&playbook, &ctx())
        .issues
        .iter()
        .map(|i| i.code)
        .collect();
    assert!(!codes.contains(&"V13"), "{codes:?}");
}

#[test]
fn a_deeper_path_is_v176() {
    let got = issues(
        "  - { id: read, type: agent_task, prompt: \"{{run.previous_success.nodes.a.output.f.g}}\" }\n",
        "",
    );
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].contains("one field only, never a path"), "{got:?}");
}

#[test]
fn an_unknown_node_is_v176() {
    let got = issues(
        "  - { id: read, type: agent_task, prompt: \"{{run.previous_success.nodes.x.output}}\" }\n",
        "",
    );
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0].contains("node `x` is not in this playbook"),
        "{got:?}"
    );
}

#[test]
fn a_worktree_read_is_v176() {
    let got = issues("", "worktree: \"trees/{{run.previous_success.id}}\"\n");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0].contains("worktree template reads `run.previous_success`"),
        "{got:?}"
    );
}

#[test]
fn a_when_none_literal_that_is_not_a_time_is_v176() {
    let extra = "previous_success:\n  when_none:\n    started_at: yesterday\n";
    let got = issues("", extra);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0].contains("neither an RFC 3339 time nor a template over params"),
        "{got:?}"
    );
}

#[test]
fn a_when_none_template_reading_a_node_is_v176() {
    let extra = "previous_success:\n  when_none:\n    started_at: \"{{nodes.a.output}}\"\n";
    let got = issues(
        "  - { id: a, type: script, script: s, runner: sh }\n",
        extra,
    );
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0].contains("only params.* may be read there"),
        "{got:?}"
    );
}

#[test]
fn a_when_none_template_reading_a_secret_param_is_v176() {
    let extra = "params:\n  - { name: token, type: text, secret: true }\nprevious_success:\n  when_none:\n    started_at: \"{{params.token}}\"\n";
    let got = issues("", extra);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].contains("reads secret param `token`"), "{got:?}");
}

#[test]
fn a_valid_when_none_literal_or_params_template_is_clean() {
    for extra in [
        "previous_success:\n  when_none:\n    started_at: \"2026-01-01T00:00:00Z\"\n",
        "params:\n  - { name: since, type: text, default: \"2026-01-01T00:00:00Z\" }\nprevious_success:\n  when_none:\n    started_at: \"{{params.since}}\"\n",
    ] {
        assert!(!v176("", extra), "{extra}");
    }
}
