//! V116-V120: a `publish` node and `publish_only`; V90 and V73 with a
//! publish node; the effects a publish node implies.

use apb_core::schema::{Effect, Playbook};
use apb_core::validate::{Severity, ValidationContext, validate};

/// A playbook `start -> work -> pub -> done`, `work` an agent_task with
/// `work_extra` and `pub` a publish node with `pub_extra`.
fn playbook(top: &str, work_extra: &str, pub_extra: &str) -> String {
    format!(
        "schema: 2\nid: p\nname: P\nversion: 1.0.0\n{top}params:\n  - {{ name: topic, type: text }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, type: agent_task, prompt: do, profile: p{work_extra} }}\n  - {{ id: pub, type: publish, from: work, remote: origin{pub_extra} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: pub }}\n  - {{ from: pub, to: done }}\n"
    )
}

const FULL: &str = ", isolation: { level: full, network: none }";

fn issues(yaml: &str) -> Vec<(&'static str, Severity, String)> {
    let p = Playbook::from_yaml(yaml).unwrap();
    validate(&p, &ValidationContext::default())
        .issues
        .into_iter()
        .filter(|i| {
            matches!(
                i.code,
                "V116" | "V117" | "V118" | "V119" | "V120" | "V90" | "V72" | "V73"
            )
        })
        .map(|i| (i.code, i.severity, i.message))
        .collect()
}

fn codes(yaml: &str) -> Vec<(&'static str, Severity)> {
    issues(yaml).into_iter().map(|(c, s, _)| (c, s)).collect()
}

#[test]
fn a_publish_node_from_an_isolated_agent_raises_nothing() {
    assert_eq!(codes(&playbook("", FULL, "")), vec![]);
    assert_eq!(
        codes(&playbook(
            "",
            FULL,
            ", branch: \"apb/{{run.id}}-{{params.topic}}\", timeout_seconds: 3600, prompt: check"
        )),
        vec![]
    );
}

#[test]
fn v116_from_must_name_a_reaching_agent_or_script() {
    let yaml = playbook("", FULL, "").replace("publish, from: work", "publish, from: nowhere");
    assert!(codes(&yaml).contains(&("V116", Severity::Error)));
    let yaml = playbook("", FULL, "").replace("publish, from: work", "publish, from: start");
    assert!(codes(&yaml).contains(&("V116", Severity::Error)));
    // `late` runs after the publish node, so the publish node is not
    // reachable from it.
    let yaml = playbook("", FULL, "")
        .replace(
            "  - { id: done, type: finish, outcome: success }\n",
            "  - { id: done, type: finish, outcome: success }\n  - { id: late, type: script, script: x.sh, runner: sh }\n",
        )
        .replace(
            "  - { from: pub, to: done }\n",
            "  - { from: pub, to: late }\n  - { from: late, to: done }\n",
        )
        .replace("publish, from: work", "publish, from: late");
    assert!(
        issues(&yaml)
            .iter()
            .any(|(c, _, m)| *c == "V116" && m.contains("not reachable"))
    );
}

#[test]
fn v116_refuses_a_list_source() {
    let yaml = playbook(
        "",
        &format!("{FULL}, list: {{ items: {{ inline: [ {{ id: a }} ] }} }}"),
        "",
    );
    let got = issues(&yaml);
    assert!(
        got.iter()
            .any(|(c, _, m)| *c == "V116" && m.contains("list node")),
        "{got:?}"
    );
}

#[test]
fn v117_checks_remote_branch_and_timeout() {
    let bad = [
        playbook("", FULL, "").replace("remote: origin", "remote: \"-o\""),
        playbook("", FULL, ", branch: \"a..b\""),
        playbook("", FULL, ", branch: \"{{run.instruction}}\""),
        playbook("", FULL, ", branch: \"{{nodes.ghost.output}}\""),
        playbook("", FULL, ", branch: \"{{params.missing}}\""),
        playbook("", FULL, ", branch: \"{{nodes.work.report}}\""),
        playbook("", FULL, ", branch: \"{{nodes.work.review_decision}}\""),
        playbook("", FULL, ", timeout_seconds: 0"),
        playbook("", FULL, ", timeout_seconds: 3601"),
    ];
    for yaml in bad {
        assert!(codes(&yaml).contains(&("V117", Severity::Error)), "{yaml}");
    }
    assert_eq!(
        codes(&playbook(
            "",
            FULL,
            ", branch: \"refs/heads/feat/{{nodes.work.output.slug}}\""
        )),
        vec![]
    );
}

#[test]
fn v118_refuses_fields_a_publish_node_does_not_take() {
    for extra in [
        ", cache: auto",
        ", success_check: { marker: DONE }",
        ", route: auto",
        ", completion_check: auto",
        ", option_descriptions: { publish: x }",
        ", auto_decide_ok: true",
        ", outputs: { files: [a] }",
        ", inputs: { files: [a] }",
        ", list: { items: { inline: [ { id: a } ] } }",
        ", fork: { on_branch_failure: cancel_siblings }",
    ] {
        let got = codes(&playbook("", FULL, extra));
        assert!(got.contains(&("V118", Severity::Error)), "{extra}: {got:?}");
    }
    // auto_decide stays V72.
    let got = codes(&playbook("", FULL, ", auto_decide: { allow: [skip] }"));
    assert!(got.contains(&("V72", Severity::Error)), "{got:?}");
}

#[test]
fn v119_warns_on_agents_without_full_isolation_and_on_scripts() {
    let got = issues(&playbook("", "", ""));
    assert!(
        got.iter()
            .any(|(c, s, m)| *c == "V119" && *s == Severity::Warning && m.contains("`work`")),
        "{got:?}"
    );
    let yaml = playbook("", FULL, "").replace(
        "  - { id: done, type: finish, outcome: success }\n",
        "  - { id: done, type: finish, outcome: success }\n  - { id: s, type: script, script: x.sh, runner: sh }\n",
    );
    let yaml = yaml.replace(
        "  - { from: pub, to: done }\n",
        "  - { from: pub, to: s }\n  - { from: s, to: done }\n",
    );
    let got = issues(&yaml);
    assert!(
        got.iter()
            .any(|(c, s, m)| *c == "V119" && *s == Severity::Warning && m.contains("`s`")),
        "{got:?}"
    );
}

/// `playbook("", FULL, "")` with `extra` nodes between `pub` and `done`.
fn with_after_pub(top: &str, nodes: &str, ids: &[&str]) -> String {
    let mut chain = vec!["pub"];
    chain.extend_from_slice(ids);
    chain.push("done");
    let edges: String = chain
        .windows(2)
        .map(|w| format!("  - {{ from: {}, to: {} }}\n", w[0], w[1]))
        .collect();
    playbook(top, FULL, "")
        .replace(
            "  - { id: done, type: finish, outcome: success }\n",
            &format!("  - {{ id: done, type: finish, outcome: success }}\n{nodes}"),
        )
        .replace("  - { from: pub, to: done }\n", &edges)
}

#[test]
fn v119_names_sub_playbooks_and_the_scripts_outside_the_sandbox() {
    let top = "goal:\n  statement: s\n  criteria:\n    - { description: c, check: { type: script, path: checks/c.sh } }\nbaseline_check:\n  checks:\n    - { id: tests, project_checks: { only: [test] } }\n";
    let yaml = with_after_pub(
        top,
        "  - { id: sub, type: playbook, playbook: child }\n",
        &["sub"],
    );
    let got = issues(&yaml);
    let warned = |want: &str| {
        got.iter()
            .any(|(c, s, m)| *c == "V119" && *s == Severity::Warning && m.contains(want))
    };
    assert!(warned("`sub`"), "{got:?}");
    assert!(warned("baseline check `tests`"), "{got:?}");
    assert!(warned("goal criterion 1"), "{got:?}");
    // Under publish_only the child is held at its start, so a sub-playbook
    // is not an error here.
    let got = codes(&with_after_pub(
        "publish_only: true\n",
        "  - { id: sub, type: playbook, playbook: child }\n",
        &["sub"],
    ));
    assert_eq!(got, vec![("V119", Severity::Warning)]);
}

#[test]
fn v120_names_the_fix_for_agents_that_cannot_take_isolation() {
    let yaml = playbook("publish_only: true\n", FULL, "").replace(
        "{ id: done, type: finish, outcome: success }",
        "{ id: done, type: finish, outcome: success, prompt: \"Sum it up\" }",
    );
    let got = issues(&yaml);
    assert!(
        got.iter().any(|(c, s, m)| *c == "V120"
            && *s == Severity::Error
            && m.contains("`done`")
            && m.contains("drop the prompt")
            && !m.contains("set `isolation: full`")),
        "{got:?}"
    );
}

#[test]
fn v120_makes_the_condition_an_error_under_publish_only() {
    let got = codes(&playbook("publish_only: true\n", "", ""));
    assert!(got.contains(&("V120", Severity::Error)), "{got:?}");
    assert!(!got.contains(&("V119", Severity::Warning)), "{got:?}");
    assert_eq!(codes(&playbook("publish_only: true\n", FULL, "")), vec![]);
    // publish_only without a publish node is a warning.
    let yaml = "schema: 2\nid: p\nname: P\nversion: 1.0.0\npublish_only: true\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";
    assert_eq!(codes(yaml), vec![("V120", Severity::Warning)]);
}

#[test]
fn v90_never_fires_for_a_publish_node() {
    let yaml = playbook("", FULL, "").replace("id: pub,", "id: push_it,");
    let yaml = yaml
        .replace("to: pub }", "to: push_it }")
        .replace("from: pub,", "from: push_it,");
    assert!(!codes(&yaml).iter().any(|(c, _)| *c == "V90"));
}

#[test]
fn v73_counts_a_publish_node_after_a_gate_as_irreversible() {
    let yaml = "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: do, profile: p, isolation: { level: full, network: none } }\n  - { id: g, type: human_review, options: [approve, needs_changes], auto_decide: { allow: [needs_changes] } }\n  - { id: pub, type: publish, from: work, remote: origin }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: g }\n  - { from: g, to: pub }\n  - { from: pub, to: done }\n";
    let got = issues(yaml);
    assert!(
        got.iter()
            .any(|(c, s, m)| *c == "V73" && *s == Severity::Error && m.contains("`pub`")),
        "{got:?}"
    );
}

#[test]
fn a_publish_node_makes_the_playbook_irreversible() {
    let p = Playbook::from_yaml(&playbook("", FULL, "")).unwrap();
    let pub_node = p.node("pub").unwrap();
    assert_eq!(
        pub_node.kind.implied_effects(),
        &[Effect::Network, Effect::External, Effect::Irreversible]
    );
    assert!(apb_core::effects::node_acts(pub_node));
    assert!(pub_node.kind.drive_handled());
    // `publish_only_violations` reads `runs_agent`: a publish node never
    // counts as one, or every publish_only playbook would fail V120.
    assert!(!pub_node.kind.runs_agent());
    assert!(apb_core::effects::inferred(&p).contains(&Effect::Irreversible));
    assert!(apb_core::effects::effective(&p).contains(&Effect::Irreversible));
}
