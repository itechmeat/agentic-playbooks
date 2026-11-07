//! The declared write scope: V170 to V172 (spec node-contracts D3).

use apb_core::schema::Playbook;
use apb_core::validate::{Severity, ValidationContext, validate};

fn playbook(head: &str, extra: &str, edges: &str) -> String {
    format!(
        "schema: 2\nid: ws\nname: WS\nversion: 1.0.0\ndefaults: {{ profile: x }}\nnodes:\n  - {{ id: start, type: start }}\n{head}{extra}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: w }}\n{edges}  - {{ from: w, to: done }}\n"
    )
}

fn found(yaml: &str, code: &str) -> Vec<Severity> {
    let p = Playbook::from_yaml(yaml).unwrap();
    validate(&p, &ValidationContext::default())
        .issues
        .into_iter()
        .filter(|i| i.code == code)
        .map(|i| i.severity)
        .collect()
}

#[test]
fn v170_refuses_writes_on_other_kinds_read_only_list_or_cache() {
    let cases: [(&str, &str); 5] = [
        ("prompt", "    writes:\n      - src/**\n"),
        (
            "agent_task",
            "    read_only: true\n    writes:\n      - src/**\n",
        ),
        (
            "agent_task",
            "    cache: auto\n    writes:\n      - src/**\n",
        ),
        (
            "agent_task",
            "    list:\n      items:\n        inline: [{}]\n    writes:\n      - src/**\n",
        ),
        ("script", "    max_changed_files: 0\n"),
    ];
    for (kind, extra) in cases {
        let head = if kind == "script" {
            "  - id: w\n    type: script\n    script: s\n    runner: sh\n".to_string()
        } else {
            format!("  - id: w\n    type: {kind}\n    prompt: hi\n")
        };
        let yaml = playbook(&head, extra, "");
        assert_eq!(
            found(&yaml, "V170"),
            vec![Severity::Error],
            "{kind} {extra}"
        );
    }
}

#[test]
fn v170_refuses_a_malformed_scope() {
    let cases = [
        ("    writes: []\n", true),
        ("    writes:\n      - \"\"\n", true),
        ("    writes:\n      - ../x\n", true),
        ("    writes:\n      - /abs\n", true),
        ("    writes:\n      - \"{{x}}\"\n", true),
        ("    writes:\n      - \"[\"\n", true),
        (
            &format!(
                "    writes:\n{}",
                (0..201)
                    .map(|i| format!("      - f{i}\n"))
                    .collect::<Vec<_>>()
                    .join("")
            )[..],
            true,
        ),
        ("    writes: \"see {{nodes.w.output.files}}\"\n", true),
        ("    writes: \"{{nodes.nobody.output.f}}\"\n", true),
        (
            "    writes:\n      - src/**\n    max_changed_files: 5\n",
            false,
        ),
        ("    writes: \"{{nodes.done.output.x}}\"\n", false),
    ];
    for (extra, bad) in cases {
        let yaml = playbook(
            &format!("  - id: w\n    type: agent_task\n    prompt: hi\n{extra}"),
            "",
            "",
        );
        let got = found(&yaml, "V170");
        assert_eq!(got.is_empty(), !bad, "{extra}: {got:?}");
    }
}

#[test]
fn v170_stays_quiet_on_a_valid_scope() {
    let yaml = playbook(
        "  - id: w\n    type: agent_task\n    prompt: hi\n",
        "    writes:\n      - src/**\n    max_changed_files: 5\n",
        "",
    );
    assert!(found(&yaml, "V170").is_empty());
    let yaml = playbook(
        "  - id: w\n    type: script\n    script: s\n    runner: sh\n",
        "    writes: \"{{nodes.done.output.x}}\"\n",
        "",
    );
    assert!(found(&yaml, "V170").is_empty(), "{yaml}");
}

#[test]
fn v170_fires_for_a_max_changed_files_only_node_that_is_read_only() {
    let yaml = playbook(
        "  - id: w\n    type: script\n    script: s\n    runner: sh\n    read_only: true\n",
        "    max_changed_files: 2\n",
        "",
    );
    assert_eq!(found(&yaml, "V170"), vec![Severity::Error]);
}

/// Two agent_task siblings of one source, `a_extra` and `b_extra` added to
/// each.
fn siblings(a_extra: &str, b_extra: &str) -> String {
    format!(
        "schema: 2\nid: ws\nname: WS\nversion: 1.0.0\ndefaults: {{ profile: x }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: a, type: agent_task, prompt: hi{a_extra} }}\n  - {{ id: b, type: agent_task, prompt: hi{b_extra} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: a }}\n  - {{ from: start, to: b }}\n  - {{ from: a, to: done }}\n  - {{ from: b, to: done }}\n"
    )
}

/// V171 names two unscoped sibling writers; V172 names a pair where at
/// least one is scoped, two scoped siblings included (they run one after
/// another). A `max_changed_files` alone scopes a node for the batch rule,
/// so its pair is V172.
#[test]
fn v171_and_v172_tell_unscoped_from_scoped_sibling_writers() {
    let w = |g: &str| format!(", writes: [\"{g}/**\"]");
    let cases: [(String, String, &str); 4] = [
        (String::new(), String::new(), "V171"),
        (w("src"), String::new(), "V172"),
        (w("src"), w("docs"), "V172"),
        (", max_changed_files: 2".into(), String::new(), "V172"),
    ];
    for (a, b, code) in cases {
        let yaml = siblings(&a, &b);
        let other = if code == "V171" { "V172" } else { "V171" };
        assert_eq!(found(&yaml, code), vec![Severity::Warning], "{a} {b}");
        assert!(found(&yaml, other).is_empty(), "{a} {b}");
    }
    // `max_parallel: 1` runs every pair one after another: neither warns.
    let serial = siblings("", "").replace(
        "defaults: { profile: x }",
        "defaults: { profile: x, max_parallel: 1 }",
    );
    assert!(found(&serial, "V171").is_empty());
}

/// A `writes` template is read like a prompt: an undeclared field of the
/// node it reads is V46, and a read of a node the graph does not order
/// before the writer is V38.
#[test]
fn a_writes_template_is_checked_like_a_prompt() {
    let pb = |edges: &str, field: &str| {
        format!(
            "schema: 2\nid: ws\nname: WS\nversion: 1.0.0\ndefaults: {{ profile: x }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: plan, type: agent_task, prompt: hi, outputs: {{ fields: [files] }} }}\n  - {{ id: w, type: script, script: s, runner: sh, writes: \"{{{{nodes.plan.output.{field}}}}}\" }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n{edges}"
        )
    };
    let ordered =
        "  - { from: start, to: plan }\n  - { from: plan, to: w }\n  - { from: w, to: done }\n";
    assert!(found(&pb(ordered, "files"), "V46").is_empty());
    assert!(found(&pb(ordered, "files"), "V38").is_empty());
    assert_eq!(found(&pb(ordered, "filez"), "V46"), vec![Severity::Warning]);
    let racing = "  - { from: start, to: plan }\n  - { from: start, to: w }\n  - { from: plan, to: done }\n  - { from: w, to: done }\n";
    assert_eq!(found(&pb(racing, "files"), "V38"), vec![Severity::Warning]);
}
