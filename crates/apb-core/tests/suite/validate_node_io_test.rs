//! Validation of `continue_session` (V44 error, V45 warning), reads of
//! undeclared output fields (V46 warning) and the node `workdir` template
//! (V47 error), the fields issue #67 added.

use apb_core::schema::Playbook;
use apb_core::validate::{Severity, ValidationContext, validate};

fn ctx() -> ValidationContext {
    ValidationContext {
        profiles: vec!["dev".into(), "rev".into()],
        ..Default::default()
    }
}

/// `(code, severity)` of every issue, for one node list plus edges.
fn issues(nodes: &str, edges: &str) -> Vec<(&'static str, Severity)> {
    let yaml = format!(
        "schema: 2\nid: p\nname: P\nversion: 1.0.0\ndefaults: {{ profile: dev }}\nnodes:\n  - {{ id: start, type: start }}\n{nodes}  - {{ id: done, type: finish, outcome: success }}\nedges:\n{edges}"
    );
    let playbook = Playbook::from_yaml(&yaml).unwrap();
    validate(&playbook, &ctx())
        .issues
        .iter()
        .map(|i| (i.code, i.severity))
        .collect()
}

fn has(list: &[(&'static str, Severity)], code: &str, sev: Severity) -> bool {
    list.iter().any(|(c, s)| *c == code && *s == sev)
}

const CHAIN: &str =
    "  - { from: start, to: a }\n  - { from: a, to: b }\n  - { from: b, to: done }\n";

#[test]
fn continue_session_on_an_earlier_agent_task_is_clean() {
    let got = issues(
        "  - { id: a, type: agent_task, prompt: x }\n  - { id: b, type: agent_task, prompt: y, continue_session: a }\n",
        CHAIN,
    );
    assert!(
        !got.iter().any(|(c, _)| *c == "V44" || *c == "V45"),
        "{got:?}"
    );
}

#[test]
fn continue_session_must_name_another_agent_task() {
    for (a, target) in [
        ("{ id: a, type: agent_task, prompt: x }", "nope"),
        ("{ id: a, type: agent_task, prompt: x }", "b"),
        ("{ id: a, type: prompt, prompt: x }", "a"),
    ] {
        let got = issues(
            &format!(
                "  - {a}\n  - {{ id: b, type: agent_task, prompt: y, continue_session: {target} }}\n"
            ),
            CHAIN,
        );
        assert!(has(&got, "V44", Severity::Error), "{target}: {got:?}");
    }
}

#[test]
fn a_handoff_that_would_start_cold_is_a_warning() {
    // Nothing orders `a` before `b` (parallel branches).
    let parallel = "  - { from: start, to: a }\n  - { from: start, to: b }\n  - { from: a, to: done }\n  - { from: b, to: done }\n";
    let got = issues(
        "  - { id: a, type: agent_task, prompt: x }\n  - { id: b, type: agent_task, prompt: y, continue_session: a }\n",
        parallel,
    );
    assert!(has(&got, "V45", Severity::Warning), "{got:?}");
    // Different profiles, isolation, different workdir.
    for (a_extra, b_extra) in [
        ("", ", profile: rev"),
        (", isolation: best_effort", ""),
        ("", ", workdir: sub"),
    ] {
        let got = issues(
            &format!(
                "  - {{ id: a, type: agent_task, prompt: x{a_extra} }}\n  - {{ id: b, type: agent_task, prompt: y, continue_session: a{b_extra} }}\n"
            ),
            CHAIN,
        );
        assert!(
            has(&got, "V45", Severity::Warning),
            "{a_extra}{b_extra}: {got:?}"
        );
        assert!(!has(&got, "V44", Severity::Error), "{got:?}");
    }
}

#[test]
fn reading_an_undeclared_field_of_a_declaring_node_is_a_warning() {
    let nodes = "  - { id: a, type: agent_task, prompt: x, outputs: { fields: [verdict] } }\n  - { id: b, type: agent_task, prompt: \"{{nodes.a.output.FIELD}}\" }\n";
    let ok = issues(&nodes.replace("FIELD", "verdict"), CHAIN);
    assert!(!ok.iter().any(|(c, _)| *c == "V46"), "{ok:?}");
    let bad = issues(&nodes.replace("FIELD", "verdit"), CHAIN);
    assert!(has(&bad, "V46", Severity::Warning), "{bad:?}");
    // An output_field edge is checked the same way.
    let edges = "  - { from: start, to: a }\n  - { from: a, to: b, condition: { type: output_field, node: a, field: verdit, equals: ok } }\n  - { from: a, to: done, fallback: true }\n  - { from: b, to: done }\n";
    let got = issues(
        "  - { id: a, type: agent_task, prompt: x, outputs: { fields: [verdict] } }\n  - { id: b, type: agent_task, prompt: y }\n",
        edges,
    );
    assert!(has(&got, "V46", Severity::Warning), "{got:?}");
    // A node that declares nothing is not checked.
    let got = issues(
        "  - { id: a, type: agent_task, prompt: x }\n  - { id: b, type: agent_task, prompt: \"{{nodes.a.output.anything}}\" }\n",
        CHAIN,
    );
    assert!(!got.iter().any(|(c, _)| *c == "V46"), "{got:?}");
}

#[test]
fn a_workdir_template_is_validated() {
    let node = |wd: &str, extra: &str| {
        format!(
            "  - {{ id: a, type: agent_task, prompt: x, outputs: {{ fields: [tree] }} }}\n  - {{ id: b, type: agent_task, prompt: y, workdir: \"{wd}\"{extra} }}\n"
        )
    };
    for wd in [
        "{{nodes.a.output.tree}}",
        "worktrees/{{run.instruction}}",
        "../other",
        "/abs/path",
    ] {
        let yaml_nodes = node(wd, "");
        let got = issues(&yaml_nodes, CHAIN);
        assert!(!has(&got, "V47", Severity::Error), "{wd}: {got:?}");
    }
    // Namespaces that make no sense for a path, an unknown node, and a
    // workdir combined with isolation.
    for (wd, extra) in [
        ("{{run.context}}", ""),
        ("{{nodes.zz.output.tree}}", ""),
        ("sub", ", isolation: best_effort"),
    ] {
        let got = issues(&node(wd, extra), CHAIN);
        assert!(
            has(&got, "V47", Severity::Error) || has(&got, "V13", Severity::Error),
            "{wd}{extra}: {got:?}"
        );
    }
    // A script node takes a workdir too.
    let got = issues(
        "  - { id: a, type: agent_task, prompt: x, outputs: { fields: [tree] } }\n  - { id: b, type: script, script: scripts/x.sh, runner: sh, workdir: \"{{run.context}}\" }\n",
        CHAIN,
    );
    assert!(has(&got, "V47", Severity::Error), "{got:?}");
}

/// A workdir that reads a node on a parallel branch may render empty: V38,
/// exactly like a prompt read.
#[test]
fn a_racy_workdir_read_is_v38() {
    let parallel = "  - { from: start, to: a }\n  - { from: start, to: b }\n  - { from: a, to: done }\n  - { from: b, to: done }\n";
    let got = issues(
        "  - { id: a, type: agent_task, prompt: x }\n  - { id: b, type: agent_task, prompt: y, workdir: \"{{nodes.a.output}}\" }\n",
        parallel,
    );
    assert!(has(&got, "V38", Severity::Warning), "{got:?}");
}

/// The profile-aware preflight (`apb validate`, `apb doctor`) warns when a
/// `continue_session` node's agent cannot continue a session at all.
#[test]
fn preflight_warns_when_the_bound_agent_cannot_continue_a_session() {
    let _cfg = crate::common::config_sandbox();
    for (agent, warns) in [("claude", false), ("codex", false), ("grok", true)] {
        let dir = tempfile::tempdir().unwrap();
        let prof = dir.path().join(".apb/profiles/dev");
        std::fs::create_dir_all(&prof).unwrap();
        std::fs::write(
            prof.join("profile.yaml"),
            format!("name: dev\ndescription: t\nexecutor:\n  agent: {agent}\n  model: m\n"),
        )
        .unwrap();
        std::fs::write(prof.join("SOUL.md"), "").unwrap();
        let playbook = Playbook::from_yaml(
            "schema: 2\nid: p\nname: P\nversion: 1.0.0\ndefaults: { profile: dev }\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: agent_task, prompt: x }\n  - { id: b, type: agent_task, prompt: y, continue_session: a }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: b }\n  - { from: b, to: done }\n",
        )
        .unwrap();
        let found: Vec<&str> = apb_core::preflight::findings(dir.path(), &playbook)
            .into_iter()
            .map(|(code, _)| code)
            .collect();
        assert_eq!(
            found.contains(&"session_handoff_cold"),
            warns,
            "{agent}: {found:?}"
        );
    }
}

/// Two nodes continuing one session on parallel branches would write into
/// it at the same time.
#[test]
fn parallel_continuations_of_one_session_are_a_warning() {
    let edges = "  - { from: start, to: a }\n  - { from: a, to: b }\n  - { from: a, to: c }\n  - { from: b, to: done }\n  - { from: c, to: done }\n";
    let got = issues(
        "  - { id: a, type: agent_task, prompt: x }\n  - { id: b, type: agent_task, prompt: y, continue_session: a }\n  - { id: c, type: agent_task, prompt: z, continue_session: a }\n",
        edges,
    );
    assert!(has(&got, "V45", Severity::Warning), "{got:?}");
    // In a chain the second continuation follows the first: no warning.
    let chain = "  - { from: start, to: a }\n  - { from: a, to: b }\n  - { from: b, to: c }\n  - { from: c, to: done }\n";
    let got = issues(
        "  - { id: a, type: agent_task, prompt: x }\n  - { id: b, type: agent_task, prompt: y, continue_session: a }\n  - { id: c, type: agent_task, prompt: z, continue_session: a }\n",
        chain,
    );
    assert!(!has(&got, "V45", Severity::Warning), "{got:?}");
}
