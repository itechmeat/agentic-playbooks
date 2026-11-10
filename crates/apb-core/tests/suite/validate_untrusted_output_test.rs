//! V187 (warning): a node granted a function whose results are outside text
//! (mail, events, files, search results) without `untrusted: true` on the
//! binding. The connector facts come in through
//! `ValidationContext::connectors`, so both a known connector and an
//! unknown one are testable offline.

use std::collections::BTreeMap;

use apb_core::connector::resolve::ConnectorFacts;
use apb_core::schema::Playbook;
use apb_core::validate::{Severity, ValidationContext, validate};

/// A connector with one marked read (`get_mail`), one unmarked read
/// (`list_mail`) and one unmarked write (`mark_read`).
fn facts() -> BTreeMap<String, ConnectorFacts> {
    BTreeMap::from([(
        "reader".to_string(),
        ConnectorFacts {
            functions: vec![
                "get_mail".to_string(),
                "list_mail".to_string(),
                "mark_read".to_string(),
            ],
            write_functions: vec!["mark_read".to_string()],
            untrusted_output_functions: vec!["get_mail".to_string()],
            ..Default::default()
        },
    )])
}

fn ctx() -> ValidationContext {
    ValidationContext {
        profiles: vec!["dev".into()],
        connectors: facts(),
        ..Default::default()
    }
}

fn v187(yaml: &str) -> Vec<(Severity, String)> {
    let playbook = Playbook::from_yaml(yaml).unwrap();
    validate(&playbook, &ctx())
        .issues
        .iter()
        .filter(|i| i.code == "V187")
        .map(|i| (i.severity, i.message.clone()))
        .collect()
}

fn pb(node: &str) -> String {
    format!(
        "schema: 2\nid: p\nname: P\nversion: 1.0.0\ndefaults: {{ profile: dev }}\nnodes:\n  - {{ id: start, type: start }}\n{node}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: w }}\n  - {{ from: w, to: done }}\n"
    )
}

#[test]
fn functions_all_granting_a_marked_read_warns() {
    let got = v187(&pb(
        "  - { id: w, type: agent_task, prompt: p, connectors: [{ name: reader,  }] }\n",
    ));
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].0, Severity::Warning);
    assert!(got[0].1.contains("reader.get_mail"), "{:?}", got[0].1);
    assert!(got[0].1.contains("untrusted: true"), "{:?}", got[0].1);
}

#[test]
fn a_read_only_grant_warns() {
    let got = v187(&pb(
        "  - { id: w, type: agent_task, prompt: p, connectors: [{ name: reader, functions: read_only }] }\n",
    ));
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].0, Severity::Warning);
}

#[test]
fn a_list_grant_warns() {
    let got = v187(&pb(
        "  - { id: w, type: agent_task, prompt: p, connectors: [{ name: reader, functions: [get_mail, mark_read] }] }\n",
    ));
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].0, Severity::Warning);
}

#[test]
fn a_marked_binding_is_silent() {
    let got = v187(&pb(
        "  - { id: w, type: agent_task, prompt: p, connectors: [{ name: reader, untrusted: true }] }\n",
    ));
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn a_list_of_unmarked_functions_only_is_silent() {
    let got = v187(&pb(
        "  - { id: w, type: agent_task, prompt: p, connectors: [{ name: reader, functions: [mark_read] }] }\n",
    ));
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn read_only_with_only_marked_writes_is_silent() {
    // `reader-writes`: every marked function is also a write, so the
    // read_only grant reaches none of them.
    let connectors = BTreeMap::from([(
        "reader-writes".to_string(),
        ConnectorFacts {
            functions: vec!["get_mail".to_string()],
            write_functions: vec!["get_mail".to_string()],
            untrusted_output_functions: vec!["get_mail".to_string()],
            ..Default::default()
        },
    )]);
    let c = ValidationContext {
        profiles: vec!["dev".into()],
        connectors,
        ..Default::default()
    };
    let playbook = Playbook::from_yaml(&pb(
        "  - { id: w, type: agent_task, prompt: p, connectors: [{ name: reader-writes, functions: read_only }] }\n",
    ))
    .unwrap();
    let report = validate(&playbook, &c);
    let got: Vec<_> = report.issues.iter().filter(|i| i.code == "V187").collect();
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn an_item_check_binding_is_never_checked() {
    // The synthetic `<node>#check` grant is built with `untrusted: false`
    // and could not be marked by anyone: the rule walks only authored
    // bindings.
    let yaml = "schema: 2\nid: p\nname: P\nversion: 1.0.0\ndefaults: { profile: dev }\nnodes:\n  - { id: start, type: start }\n  - id: source\n    type: agent_task\n    prompt: p\n    outputs: { fields: [mails] }\n  - id: w\n    type: agent_task\n    prompt: p\n    connectors:\n      - { name: reader, untrusted: true }\n    list:\n      items:\n        from: { node: source, field: mails }\n      check:\n        connector: { name: reader, function: get_mail, accept_when: { path: ok, exists: true } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: source }\n  - { from: source, to: w }\n  - { from: w, to: done }\n";
    let got = v187(yaml);
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn a_baseline_check_reading_one_is_never_checked() {
    let yaml = "schema: 2\nid: p\nname: P\nversion: 1.0.0\ndefaults: { profile: dev }\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: agent_task, prompt: p, connectors: [{ name: reader, untrusted: true }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: done }\nbaseline_check:\n  checks:\n    - id: b\n      description: d\n      connector:\n        name: reader\n        function: get_mail\n        args: {}\n      observe:\n        - { id: c, type: number, path: n, expect: { equals: 0 } }\n";
    let got = v187(yaml);
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn an_unknown_connector_is_silent() {
    let got = v187(&pb(
        "  - { id: w, type: agent_task, prompt: p, connectors: [{ name: nope,  }] }\n",
    ));
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn a_connector_with_a_load_error_is_silent() {
    let connectors = BTreeMap::from([(
        "broken".to_string(),
        ConnectorFacts {
            load_error: Some("bad manifest".to_string()),
            ..Default::default()
        },
    )]);
    let c = ValidationContext {
        profiles: vec!["dev".into()],
        connectors,
        ..Default::default()
    };
    let playbook = Playbook::from_yaml(&pb(
        "  - { id: w, type: agent_task, prompt: p, connectors: [{ name: broken,  }] }\n",
    ))
    .unwrap();
    let report = validate(&playbook, &c);
    let got: Vec<_> = report.issues.iter().filter(|i| i.code == "V187").collect();
    assert!(got.is_empty(), "{got:?}");
}
