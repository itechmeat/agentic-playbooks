//! The `baseline_check` block: it parses and round-trips, and each of
//! V62-V68 fires on its case and stays quiet on a valid block.

use apb_core::connector::resolve::ConnectorFacts;
use apb_core::schema::Playbook;
use apb_core::validate::{Severity, ValidationContext, validate};
use std::collections::BTreeMap;

const GOOD: &str = r#"
schema: 2
id: crm
name: CRM
version: 1.0.0
params:
  - { name: open_stages, type: text }
goal:
  statement: Every open deal has an owner
  criteria:
    - { description: Owner report, check: { type: marker, marker: DONE } }
baseline_check:
  checks:
    - id: unowned
      description: Open deals without an owner
      connector:
        name: twenty
        account: default
        function: list_opportunities
        args: { filter: "stage[in]:[{{params.open_stages}}]", limit: 1 }
      complete_when: { path: pageInfo.hasNextPage, equals: false }
      observe:
        - { id: count, type: number, path: totalCount, expect: { equals: 0 } }
        - { id: ids, type: list, path: "edges[].node.id", expect: { change: no_new } }
    - id: tests
      project_checks: { only: [test] }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: hi, profile: x }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

fn issues_with(yaml: &str, ctx: &ValidationContext) -> Vec<(&'static str, Severity)> {
    let p = Playbook::from_yaml(yaml).unwrap();
    validate(&p, ctx)
        .issues
        .into_iter()
        .map(|i| (i.code, i.severity))
        .collect()
}

fn issues(yaml: &str) -> Vec<(&'static str, Severity)> {
    issues_with(yaml, &ValidationContext::default())
}

fn has(got: &[(&'static str, Severity)], code: &str, sev: Severity) -> bool {
    got.iter().any(|(c, s)| *c == code && *s == sev)
}

fn none_of(got: &[(&'static str, Severity)], codes: &[&str]) -> bool {
    !got.iter().any(|(c, _)| codes.contains(c))
}

#[test]
fn the_block_parses_and_round_trips() {
    let p = Playbook::from_yaml(GOOD).unwrap();
    let block = p.baseline_check.as_ref().unwrap();
    assert!(!block.enforce);
    assert_eq!(block.checks.len(), 2);
    assert_eq!(block.checks[0].timeout_seconds, 300);
    assert_eq!(block.checks[0].observe[1].path, "edges[].node.id");
    let yaml = yaml_serde::to_string(&p).unwrap();
    let again = Playbook::from_yaml(&yaml).unwrap();
    assert_eq!(again.baseline_check, p.baseline_check);
    // An absent block stays absent on the way back out.
    let plain = GOOD.split("baseline_check:").next().unwrap().to_string()
        + "nodes:"
        + GOOD.split("\nnodes:").nth(1).unwrap();
    let p = Playbook::from_yaml(&plain).unwrap();
    assert!(
        !yaml_serde::to_string(&p)
            .unwrap()
            .contains("baseline_check")
    );
}

#[test]
fn a_valid_block_has_no_baseline_issues() {
    let got = issues(GOOD);
    assert!(
        none_of(&got, &["V62", "V63", "V64", "V65", "V66", "V67", "V68"]),
        "{got:?}"
    );
}

#[test]
fn v62_structure() {
    for (from, to) in [
        (
            "      project_checks: { only: [test] }",
            "      project_checks: { only: [deploy] }",
        ),
        ("    - id: tests\n", "    - id: unowned\n"),
        ("    - id: tests\n", "    - id: Tests\n"),
        (
            "      description: Open deals",
            "      timeout_seconds: 0\n      description: Open deals",
        ),
        (
            "      project_checks: { only: [test] }",
            "      project_checks: { only: [test] }\n      script: scripts/x.sh",
        ),
        (
            "      project_checks: { only: [test] }",
            "      project_checks: { only: [test] }\n      observe: [{ id: n, type: number, path: n, expect: { equals: 0 } }]",
        ),
        ("        - { id: ids,", "        - { id: count,"),
        // complete_when is a connector's; a script says `complete: false`.
        (
            "      project_checks: { only: [test] }",
            "      script: scripts/n.sh\n      complete_when: { path: done, equals: true }\n      observe: [{ id: n, type: number, path: n, expect: { equals: 0 } }]",
        ),
    ] {
        assert!(GOOD.contains(from), "{from}");
        let got = issues(&GOOD.replace(from, to));
        assert!(has(&got, "V62", Severity::Error), "{to}: {got:?}");
    }
    let got = issues(&GOOD.replace(
        "      project_checks: { only: [test] }",
        "      description: no source",
    ));
    assert!(has(&got, "V62", Severity::Error), "{got:?}");
}

#[test]
fn v63_source_safety() {
    let script = GOOD.replace(
        "      project_checks: { only: [test] }",
        "      script: ../escape.sh\n      observe: [{ id: n, type: number, path: n, expect: { equals: 0 } }]",
    );
    assert!(has(&issues(&script), "V63", Severity::Error));
    assert!(!has(
        &issues(&script.replace("../escape.sh", "scripts/failing.sh")),
        "V63",
        Severity::Error
    ));

    let facts = |f: ConnectorFacts| ValidationContext {
        connectors: BTreeMap::from([("twenty".to_string(), f)]),
        ..Default::default()
    };
    let read = ConnectorFacts {
        functions: vec!["list_opportunities".into(), "update".into()],
        write_functions: vec!["update".into()],
        ..Default::default()
    };
    assert!(!has(
        &issues_with(GOOD, &facts(read.clone())),
        "V63",
        Severity::Error
    ));
    let write = GOOD.replace("function: list_opportunities", "function: update");
    assert!(has(
        &issues_with(&write, &facts(read.clone())),
        "V63",
        Severity::Error
    ));
    let undeclared = GOOD.replace("function: list_opportunities", "function: missing");
    assert!(has(
        &issues_with(&undeclared, &facts(read)),
        "V63",
        Severity::Error
    ));
    let inbox = ConnectorFacts {
        functions: vec!["list_opportunities".into()],
        inbox_functions: vec!["list_opportunities".into()],
        has_webhook: true,
        ..Default::default()
    };
    assert!(has(
        &issues_with(GOOD, &facts(inbox)),
        "V63",
        Severity::Error
    ));
    // A malformed connector name is one diagnostic (V23), not two.
    let got = issues(&GOOD.replace("name: twenty", "name: Bad_Name"));
    assert!(has(&got, "V23", Severity::Error), "{got:?}");
    assert!(!has(&got, "V63", Severity::Error), "{got:?}");
}

#[test]
fn v64_rules_types_and_paths() {
    for (from, to) in [
        ("expect: { equals: 0 } }", "expect: { change: no_new } }"),
        (
            "expect: { equals: 0 } }",
            "expect: { equals: 0, at_most: 3 } }",
        ),
        ("expect: { equals: 0 } }", "expect: {} }"),
        ("expect: { equals: 0 } }", "expect: { at_most: .inf } }"),
        ("path: totalCount,", "path: \"total count\","),
        ("type: list, path:", "type: list, count: true, path:"),
        ("path: pageInfo.hasNextPage,", "path: \"pageInfo..x\","),
        // A `[]` path reads a list: a plain number or field cannot hold it.
        ("path: totalCount,", "path: \"edges[].total\","),
        // A read is complete only on one value, so `complete_when` maps nothing.
        (
            "path: pageInfo.hasNextPage,",
            "path: \"edges[].node.done\",",
        ),
    ] {
        assert!(GOOD.contains(from), "{from}");
        let got = issues(&GOOD.replace(from, to));
        assert!(has(&got, "V64", Severity::Error), "{to}: {got:?}");
    }
}

#[test]
fn v65_only_params_may_be_templated() {
    let got = issues(&GOOD.replace("{{params.open_stages}}", "{{nodes.work.output}}"));
    assert!(has(&got, "V65", Severity::Error), "{got:?}");
    let got = issues(&GOOD.replace("equals: false }", "equals: \"{{run.context}}\" }"));
    assert!(has(&got, "V65", Severity::Error), "{got:?}");
    // Only a declared param, named whole.
    for typo in ["{{params.open_stage}}", "{{params.open_stages.x}}"] {
        let got = issues(&GOOD.replace("{{params.open_stages}}", typo));
        assert!(has(&got, "V65", Severity::Error), "{typo}: {got:?}");
    }
}

#[test]
fn v66_a_baseline_without_a_checked_goal_warns() {
    let manual = GOOD.replace(", check: { type: marker, marker: DONE }", "");
    assert!(has(&issues(&manual), "V66", Severity::Warning));
}

#[test]
fn v67_a_tree_source_with_a_node_resolved_worktree_warns() {
    let yaml = GOOD.replace(
        "baseline_check:",
        "worktree: \"{{nodes.work.output}}\"\nbaseline_check:",
    );
    assert!(has(&issues(&yaml), "V67", Severity::Warning));
    let params_tree = GOOD.replace(
        "baseline_check:",
        "worktree: \"{{params.open_stages}}\"\nbaseline_check:",
    );
    assert!(!has(&issues(&params_tree), "V67", Severity::Warning));
}

#[test]
fn v68_the_baseline_site_is_reserved() {
    let yaml = GOOD
        .replace("id: work,", "id: \"@baseline\",")
        .replace("to: work }", "to: \"@baseline\" }")
        .replace("from: work,", "from: \"@baseline\",");
    assert!(has(&issues(&yaml), "V68", Severity::Error));
}
