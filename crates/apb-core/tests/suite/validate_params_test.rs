//! Secret params: V99 refuses a default, V100 anything but text, and a
//! plain param re-saves without the flag. Search params: V160 refuses a
//! secret one, V161 one named like a secret, V162 more than eight.
//! V340: a reference to a secret param anywhere in the definition.

use std::collections::BTreeSet;

use apb_core::schema::Playbook;
use apb_core::validate::{SecretParamRef, ValidationContext, secret_param_refs, validate};

fn playbook(params: &str) -> String {
    format!(
        "schema: 2\nid: p\nname: P\nversion: 1.0.0\nparams:\n{params}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n"
    )
}

fn codes(yaml: &str) -> Vec<&'static str> {
    let p = Playbook::from_yaml(yaml).unwrap();
    validate(&p, &ValidationContext::default())
        .issues
        .into_iter()
        .filter(|i| {
            matches!(
                i.code,
                "V99" | "V100" | "V160" | "V161" | "V162" | "V177" | "V178"
            )
        })
        .map(|i| i.code)
        .collect()
}

#[test]
fn a_secret_text_param_without_default_is_valid() {
    let yaml = playbook("  - { name: token, type: text, secret: true }\n");
    assert!(codes(&yaml).is_empty());
    let p = Playbook::from_yaml(&yaml).unwrap();
    assert!(p.params[0].secret);
}

#[test]
fn a_secret_with_a_default_is_v99() {
    let yaml = playbook("  - { name: token, type: text, secret: true, default: abc }\n");
    assert_eq!(codes(&yaml), vec!["V99"]);
}

#[test]
fn a_secret_that_is_not_text_is_v100() {
    let yaml = playbook("  - { name: level, type: int, secret: true }\n");
    assert_eq!(codes(&yaml), vec!["V100"]);
}

#[test]
fn a_plain_param_saves_without_the_secret_key() {
    let p = Playbook::from_yaml(&playbook("  - { name: mode, type: text }\n")).unwrap();
    assert!(!yaml_serde::to_string(&p).unwrap().contains("secret"));
}

#[test]
fn a_search_param_of_every_type_is_valid() {
    let yaml = playbook(
        "  - { name: customer, type: text, search: true }\n  - { name: tier, type: enum, options: [gold, silver], search: true }\n  - { name: ticket, type: int, search: true }\n  - { name: urgent, type: bool, search: true }\n",
    );
    assert!(codes(&yaml).is_empty(), "{:?}", codes(&yaml));
    let p = Playbook::from_yaml(&yaml).unwrap();
    assert!(p.params.iter().all(|p| p.search));
    // The key is written back, and a plain param saves without it.
    assert!(yaml_serde::to_string(&p).unwrap().contains("search: true"));
    let plain = Playbook::from_yaml(&playbook("  - { name: mode, type: text }\n")).unwrap();
    assert!(!yaml_serde::to_string(&plain).unwrap().contains("search"));
}

#[test]
fn a_secret_search_param_is_v160() {
    let yaml = playbook("  - { name: customer, type: text, secret: true, search: true }\n");
    assert_eq!(codes(&yaml), vec!["V160"]);
}

#[test]
fn a_search_param_named_like_a_secret_is_v161() {
    for name in ["api_token", "password", "clientSecret", "access_key"] {
        let yaml = playbook(&format!(
            "  - {{ name: {name}, type: text, search: true }}\n"
        ));
        assert_eq!(codes(&yaml), vec!["V161"], "{name}");
    }
    // A name that only counts tokens is not a secret.
    let yaml = playbook("  - { name: max_tokens, type: int, search: true }\n");
    assert!(codes(&yaml).is_empty());
}

#[test]
fn nine_search_params_are_v162() {
    let params: String = (0..9)
        .map(|i| format!("  - {{ name: field{i}, type: text, search: true }}\n"))
        .collect();
    assert_eq!(codes(&playbook(&params)), vec!["V162"]);
    let eight: String = (0..8)
        .map(|i| format!("  - {{ name: field{i}, type: text, search: true }}\n"))
        .collect();
    assert!(codes(&playbook(&eight)).is_empty());
}

/// A pattern applies to text params only, must compile, and holds the
/// default to it.
#[test]
fn a_pattern_on_a_param_that_is_not_text_is_v177() {
    let yaml = playbook("  - { name: n, type: int, pattern: '[0-9]+' }\n");
    assert_eq!(codes(&yaml), vec!["V177"]);
}

#[test]
fn an_empty_or_overlong_or_uncompilable_pattern_is_v177() {
    assert_eq!(
        codes(&playbook("  - { name: n, type: text, pattern: '' }\n")),
        vec!["V177"]
    );
    let long = format!(
        "  - {{ name: n, type: text, pattern: '{}' }}\n",
        "a".repeat(513)
    );
    assert_eq!(codes(&playbook(&long)), vec!["V177"]);
    assert_eq!(
        codes(&playbook(
            "  - { name: n, type: text, pattern: '(unclosed' }\n"
        )),
        vec!["V177"]
    );
}

#[test]
fn a_default_that_does_not_match_its_pattern_is_v178() {
    let yaml = playbook("  - { name: n, type: text, default: EU, pattern: '[a-z]{2}' }\n");
    assert_eq!(codes(&yaml), vec!["V178"]);
    // A number default on a text param is checked through its text.
    let yaml = playbook("  - { name: n, type: text, default: 42, pattern: '[0-9]{2}' }\n");
    assert_eq!(codes(&yaml), Vec::<&str>::new());
}

#[test]
fn a_secret_param_may_have_a_pattern() {
    let yaml = playbook("  - { name: token, type: text, secret: true, pattern: '[a-z]{8,}' }\n");
    assert_eq!(codes(&yaml), Vec::<&str>::new());
}

#[test]
fn a_matching_default_and_a_plain_param_are_clean() {
    assert_eq!(
        codes(&playbook(
            "  - { name: n, type: text, default: eu, pattern: '[a-z]{2}' }\n"
        )),
        Vec::<&str>::new()
    );
    assert_eq!(
        codes(&playbook("  - { name: n, type: text }\n")),
        Vec::<&str>::new()
    );
}

// --- V340: a secret param is never rendered --------------------------------

/// A playbook declaring the secret param `s` (and the plain param `plain`),
/// with `top` above `nodes` and `nodes` added to a start and a finish. Only
/// V340 is looked at, so the rest need not be a runnable definition.
fn with_secret(top: &str, nodes: &str) -> String {
    format!(
        "schema: 2\nid: p\nname: P\nversion: 1.0.0\nparams:\n  - {{ name: s, type: text, secret: true }}\n  - {{ name: plain, type: text }}\n{top}nodes:\n  - {{ id: start, type: start }}\n{nodes}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n"
    )
}

/// Every V340 of `yaml`: the node and the message.
fn v340(yaml: &str) -> Vec<(Option<String>, String)> {
    let p = Playbook::from_yaml(yaml).unwrap_or_else(|e| panic!("{e}\n{yaml}"));
    validate(&p, &ValidationContext::default())
        .issues
        .into_iter()
        .filter(|i| i.code == "V340")
        .map(|i| (i.node, i.message))
        .collect()
}

/// Exactly one V340, on `node`, naming `field` and the param `param`.
fn assert_one(yaml: &str, node: Option<&str>, field: &str, param: &str) {
    let got = v340(yaml);
    assert_eq!(got.len(), 1, "{got:?}\n{yaml}");
    assert_eq!(got[0].0.as_deref(), node, "{got:?}");
    assert!(
        got[0]
            .1
            .contains(&format!("`{field}` references secret param `{param}`")),
        "{got:?}"
    );
}

/// D10 test 1: one playbook per rendered site, each one V340 naming the
/// node and the field.
#[test]
fn v340_at_every_template_site() {
    let r = "\"{{params.s}}\"";
    let node_sites: Vec<(&str, String, &str)> = vec![
        (
            "a",
            format!("  - {{ id: a, type: agent_task, prompt: {r} }}\n"),
            "prompt",
        ),
        (
            "pr",
            format!("  - {{ id: pr, type: prompt, prompt: {r} }}\n"),
            "prompt",
        ),
        (
            "fin",
            format!("  - {{ id: fin, type: finish, outcome: success, prompt: {r} }}\n"),
            "prompt",
        ),
        (
            "sub",
            format!("  - {{ id: sub, type: playbook, playbook: child, instruction: {r} }}\n"),
            "instruction",
        ),
        (
            "j",
            format!(
                "  - id: j\n    type: judge\n    state: {{ a: {r} }}\n    questions:\n      ok: {{ type: noul, instructions: i }}\n"
            ),
            "state.a",
        ),
        (
            "g",
            format!(
                "  - id: g\n    type: human_review\n    options: [approve]\n    fields:\n      - {{ name: amount, type: text, default: {r} }}\n"
            ),
            "fields[0].default",
        ),
        (
            "g",
            format!(
                "  - id: g\n    type: human_review\n    options: [approve]\n    show:\n      - {{ label: Total, text: {r} }}\n"
            ),
            "show[0].text",
        ),
        (
            "a",
            format!("  - {{ id: a, type: agent_task, prompt: p, workdir: {r} }}\n"),
            "workdir",
        ),
        (
            "sc",
            format!("  - {{ id: sc, type: script, script: s.sh, runner: sh, workdir: {r} }}\n"),
            "workdir",
        ),
        (
            "a",
            format!("  - {{ id: a, type: agent_task, prompt: p, writes: {r} }}\n"),
            "writes",
        ),
        (
            "a",
            format!("  - {{ id: a, type: agent_task, prompt: p, cache: {{ key: {r} }} }}\n"),
            "cache.key",
        ),
        (
            "pub",
            format!(
                "  - {{ id: work, type: agent_task, prompt: do }}\n  - {{ id: pub, type: publish, from: work, remote: origin, branch: {r} }}\n"
            ),
            "branch",
        ),
    ];
    for (node, nodes, field) in &node_sites {
        assert_one(&with_secret("", nodes), Some(node), field, "s");
    }

    // The list item check of a list node.
    let list = |check: &str| {
        format!(
            "  - {{ id: src, type: agent_task, prompt: p, outputs: {{ fields: [items] }} }}\n  - id: w\n    type: agent_task\n    prompt: p\n    connectors:\n      - {{ name: crm }}\n    list:\n      items:\n        from: {{ node: src, field: items }}\n      check:\n{check}"
        )
    };
    for (check, field) in [
        (
            format!(
                "        connector: {{ name: crm, function: get, args: {{ q: {r} }}, accept_when: {{ path: ok, exists: true }} }}\n"
            ),
            "list.check.connector.args.q",
        ),
        (
            format!(
                "        connector: {{ name: crm, function: get, accept_when: {{ path: ok, equals: {r} }} }}\n"
            ),
            "list.check.connector.accept_when.equals",
        ),
        (
            format!(
                "        connector: {{ name: crm, function: get, accept_when: {{ path: ok, contains: {r} }} }}\n"
            ),
            "list.check.connector.accept_when.contains",
        ),
        (
            format!(
                "        judge:\n          state: {{ subject: {r} }}\n          instructions: Plausible?\n          criteria: {{ accept: yes, reject: no }}\n"
            ),
            "list.check.judge.state.subject",
        ),
    ] {
        assert_one(&with_secret("", &list(&check)), Some("w"), field, "s");
    }

    // The definition-level sites.
    assert_one(
        &with_secret(&format!("worktree: {r}\n"), ""),
        None,
        "worktree",
        "s",
    );
    let baseline = format!(
        "baseline_check:\n  checks:\n    - id: unowned\n      description: Open deals\n      connector:\n        name: crm\n        function: list\n        args: {{ filter: {r} }}\n      observe:\n        - {{ id: count, type: number, path: total, expect: {{ equals: 0 }} }}\n"
    );
    assert_one(
        &with_secret(&baseline, ""),
        None,
        "baseline_check.checks[0].connector.args.filter",
        "s",
    );
    let complete = format!(
        "baseline_check:\n  checks:\n    - id: unowned\n      description: Open deals\n      connector:\n        name: crm\n        function: list\n      complete_when: {{ path: more, equals: {r} }}\n      observe:\n        - {{ id: count, type: number, path: total, expect: {{ equals: 0 }} }}\n"
    );
    assert_one(
        &with_secret(&complete, ""),
        None,
        "baseline_check.checks[0].complete_when.equals",
        "s",
    );
    let follow = format!(
        "follow_up:\n  after: 1m\n  check:\n    id: won\n    description: the deal moved\n    connector:\n      name: crm\n      function: get\n      args: {{ id: {r} }}\n    observe:\n      - {{ id: stage, type: field, path: stage, expect: {{ equals: won }} }}\n"
    );
    assert_one(
        &with_secret(&follow, ""),
        None,
        "follow_up.check.connector.args.id",
        "s",
    );
}

/// D10 test 2: the forms the renderers resolve, and the ones they do not.
#[test]
fn v340_follows_the_forms_the_renderers_resolve() {
    for text in [
        "\"{{ params.s }}\"",
        "\"{{\\n\\tparams.s\\n}}\"",
        "\"x {{params.s}} y\"",
    ] {
        assert_one(
            &with_secret(
                "",
                &format!("  - {{ id: a, type: agent_task, prompt: {text} }}\n"),
            ),
            Some("a"),
            "prompt",
            "s",
        );
    }
    // A param whose name holds a dot, read by a list item check.
    let dotted = "schema: 2\nid: p\nname: P\nversion: 1.0.0\nparams:\n  - { name: a.b, type: text, secret: true }\nnodes:\n  - { id: start, type: start }\n  - { id: src, type: agent_task, prompt: p, outputs: { fields: [items] } }\n  - id: w\n    type: agent_task\n    prompt: p\n    list:\n      items:\n        from: { node: src, field: items }\n      check:\n        connector: { name: crm, function: get, args: { q: \"{{params.a.b}}\" }, accept_when: { path: ok, exists: true } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";
    assert_one(dotted, Some("w"), "list.check.connector.args.q", "a.b");
    // Forms that render nothing.
    for text in ["\"{{params . s}}\"", "\"{{Params.s}}\"", "\"{{params}}\""] {
        let yaml = with_secret(
            "",
            &format!("  - {{ id: a, type: agent_task, prompt: {text} }}\n"),
        );
        assert!(v340(&yaml).is_empty(), "{text}: {:?}", v340(&yaml));
    }
}

/// The YAML features a definition may use: an anchor reused through an
/// alias, a tagged scalar and a merge key each give V340 at every place the
/// reference lands.
#[test]
fn v340_sees_through_aliases_tags_and_merge_keys() {
    let alias = with_secret(
        "",
        "  - { id: a, type: agent_task, prompt: &r \"{{params.s}}\" }\n  - { id: b, type: agent_task, prompt: *r }\n",
    );
    let got = v340(&alias);
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(got[0].0.as_deref(), Some("a"), "{got:?}");
    assert_eq!(got[1].0.as_deref(), Some("b"), "{got:?}");
    assert_one(
        &with_secret(
            "",
            "  - { id: a, type: agent_task, prompt: !!str \"{{params.s}}\" }\n",
        ),
        Some("a"),
        "prompt",
        "s",
    );
    // The loader does not resolve a merge key: a node that takes its fields
    // through one does not load at all, so nothing of it is rendered.
    let merged_node = with_secret(
        "",
        "  - &n { id: a, type: agent_task, prompt: \"{{params.s}}\" }\n  - { <<: *n, id: b }\n",
    );
    assert!(Playbook::from_yaml(&merged_node).is_err());
    // Where a free mapping takes it (connector args), the merge key is a key
    // like any other and the reference under it is found.
    let check = |id: &str, args: &str| {
        format!(
            "    - id: {id}\n      description: Open deals\n      connector: {{ name: crm, function: list, args: {args} }}\n      observe:\n        - {{ id: count, type: number, path: total, expect: {{ equals: 0 }} }}\n"
        )
    };
    let merged_args = with_secret(
        &format!(
            "baseline_check:\n  checks:\n{}{}",
            check("c1", "&a { q: \"{{params.s}}\" }"),
            check("c2", "{ <<: *a }")
        ),
        "",
    );
    let got = v340(&merged_args);
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(
        got[0]
            .1
            .contains("`baseline_check.checks[0].connector.args.q`"),
        "{got:?}"
    );
    assert!(
        got[1]
            .1
            .contains("`baseline_check.checks[1].connector.args."),
        "{got:?}"
    );
}

/// D10 test 3: a text apb shows literally is refused too; a plain param
/// and an unreferenced secret are not.
#[test]
fn v340_covers_literal_fields_and_spares_plain_params() {
    assert_one(
        &with_secret(
            "",
            "  - { id: g, type: human_review, options: [approve], prompt: \"check {{params.s}}\" }\n",
        ),
        Some("g"),
        "prompt",
        "s",
    );
    let plain = with_secret(
        "worktree: \"{{params.plain}}\"\n",
        "  - { id: a, type: agent_task, prompt: \"{{params.plain}}\", workdir: \"{{ params.plain }}\" }\n",
    );
    assert!(v340(&plain).is_empty(), "{:?}", v340(&plain));
    let unreferenced = with_secret("", "  - { id: a, type: agent_task, prompt: hello }\n");
    assert!(v340(&unreferenced).is_empty());
}

/// D10 test 4, the validation half: V104 and V210 own their fields, while
/// the walk itself skips nothing. Each code is reported while another error
/// stands too, and so is `previous_success.when_none`, which is V340's (V176
/// runs only on a definition with no other error).
#[test]
fn the_owned_fields_report_only_their_own_code_but_the_walk_sees_them() {
    let r = "\"{{params.s}}\"";
    for (top, code, field) in [
        (
            format!("quality:\n  task_type: {r}\n"),
            "V104",
            "quality.task_type",
        ),
        (
            format!("previous_success:\n  when_none:\n    started_at: {r}\n"),
            "V340",
            "previous_success.when_none.started_at",
        ),
        (
            format!("concurrency:\n  key: {r}\n"),
            "V210",
            "concurrency.key",
        ),
    ] {
        let yaml = with_secret(&top, "");
        if code == "V340" {
            assert_eq!(v340(&yaml).len(), 1, "{top}: {:?}", v340(&yaml));
        } else {
            assert!(v340(&yaml).is_empty(), "{top}: {:?}", v340(&yaml));
        }
        // Another error beside it (V99: a secret param with a default).
        let broken = yaml.replace(
            "{ name: s, type: text, secret: true }",
            "{ name: s, type: text, secret: true, default: x }",
        );
        let codes: Vec<&str> = validate(
            &Playbook::from_yaml(&broken).unwrap(),
            &ValidationContext::default(),
        )
        .issues
        .iter()
        .map(|i| i.code)
        .collect();
        assert!(
            codes.contains(&"V99") && codes.contains(&code),
            "{top}: {codes:?}"
        );
        let p = Playbook::from_yaml(&yaml).unwrap();
        let all: Vec<&str> = validate(&p, &ValidationContext::default())
            .issues
            .iter()
            .map(|i| i.code)
            .collect();
        assert!(all.contains(&code), "{top}: {all:?}");
        let hits = secret_param_refs(&p, &BTreeSet::new()).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].node, None);
        assert_eq!(hits[0].field, field);
        assert_eq!(hits[0].param, "s");
    }
}

/// The caller's own secret set widens the declared one (the run's set).
#[test]
fn the_callers_set_counts_as_secret_too() {
    let p = Playbook::from_yaml(&with_secret(
        "",
        "  - { id: a, type: agent_task, prompt: \"{{params.plain}}\" }\n",
    ))
    .unwrap();
    assert!(secret_param_refs(&p, &BTreeSet::new()).unwrap().is_empty());
    let run_set: BTreeSet<String> = ["plain".to_string()].into();
    let hits = secret_param_refs(&p, &run_set).unwrap();
    assert_eq!(
        hits,
        vec![SecretParamRef {
            node: Some("a".into()),
            field: "prompt".into(),
            param: "plain".into(),
        }]
    );
}

/// D10 test 5, the validation half: a sub-playbook's instruction.
#[test]
fn v340_on_a_sub_playbook_instruction() {
    assert_one(
        &with_secret(
            "",
            "  - { id: sub, type: playbook, playbook: child, instruction: \"do it with {{params.s}}\" }\n",
        ),
        Some("sub"),
        "instruction",
        "s",
    );
}

/// D10 test 9: a free-form block with keys of every kind.
#[test]
fn v340_walks_every_key_of_a_free_form_block() {
    let top = "supervisor:\n  policy:\n    ~: \"{{params.s}}\"\n    ? [a, b]\n    : \"{{params.s}}\"\n    \"{{params.s}}\": x\n";
    let got = v340(&with_secret(top, ""));
    assert_eq!(got.len(), 3, "{got:?}");
    assert!(got.iter().all(|(n, _)| n.is_none()), "{got:?}");
    for field in [
        "supervisor.policy.~",
        "supervisor.policy.[a, b]",
        "supervisor.policy.{key}",
    ] {
        assert!(
            got.iter().any(|(_, m)| m.contains(&format!("`{field}`"))),
            "{field}: {got:?}"
        );
    }
}

/// The playbooks apb ships, from the repository root.
fn shipped_playbooks() -> Vec<std::path::PathBuf> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut out = Vec::new();
    for dir in std::fs::read_dir(root.join("bundled/playbooks")).unwrap() {
        let path = dir.unwrap().path().join("playbook.yaml");
        if path.is_file() {
            out.push(path);
        }
    }
    for entry in std::fs::read_dir(root.join("examples/playbooks")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "yaml") {
            out.push(path);
        }
    }
    out.push(root.join(
        "examples/paperclip/apb-adapter/test-fixture/.apb/playbooks/apb-noop/1.0.0/playbook.yaml",
    ));
    out
}

/// D10 test 14: no shipped playbook meets the rule.
#[test]
fn no_shipped_playbook_references_a_secret_param() {
    let all = shipped_playbooks();
    assert!(all.len() > 10, "{all:?}");
    for path in all {
        let yaml = std::fs::read_to_string(&path).unwrap();
        let p = Playbook::from_yaml(&yaml).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let hits = secret_param_refs(&p, &BTreeSet::new());
        assert_eq!(hits, Ok(vec![]), "{}", path.display());
    }
}
