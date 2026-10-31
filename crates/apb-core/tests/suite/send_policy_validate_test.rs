//! Send-once declarations: the manifest rules for `idempotency` and
//! `identity` (each one refuses its case), the `on_unknown` binding field
//! (round-trips, shorthand included), and validator rules V69 and V84.

use std::collections::BTreeMap;

use apb_core::connector::def::ConnectorDoc;
use apb_core::connector::idempotency::IdempotencyMode;
use apb_core::connector::resolve::ConnectorFacts;
use apb_core::schema::{ConnectorBinding, FunctionsAllow, OnUnknown, Playbook};
use apb_core::validate::{Severity, ValidationContext, validate};

/// A valid connector: an identity read, a listing, a marker send, a key
/// send, and an smtp send without a key.
const DOC: &str = r#"
name: x
version: 0.1.0
identity: { function: me, path: login }
functions:
  - name: me
    description: d
    read_only: true
    method: GET
    url: "https://api.example.com/me"
  - name: list
    description: d
    read_only: true
    method: GET
    url: "https://api.example.com/items"
    args_schema: { type: object, properties: { repo: { type: string }, since: { type: string } } }
  - name: post
    description: d
    method: POST
    url: "https://api.example.com/items"
    body: "{{args}}"
    args_schema: { type: object, properties: { repo: { type: string }, text: { type: string } } }
    idempotency:
      mode: marker
      arg: text
      format: html_comment
      reconcile: { function: list, args_from_send: [repo], since_arg: since, text: text, author: user.login }
  - name: say
    description: d
    method: POST
    url: "https://api.example.com/say"
    headers: { Accept: application/json }
    body: { content: "{{args.content}}" }
    args_schema: { type: object, properties: { content: { type: string } } }
    idempotency: { mode: key, field: nonce, window_sec: 120 }
  - name: mail
    description: d
    smtp:
      connection: { host: h, port: "587", use_tls: "true" }
      message: { from_email: a@b.c, to: "{{args.to}}", subject: s, body_text: t }
    args_schema: { type: object, properties: { to: { type: string } } }
    idempotency: { mode: none }
"#;

fn parse(yaml: &str) -> Result<ConnectorDoc, String> {
    ConnectorDoc::from_yaml(yaml, "x").map_err(|e| e.to_string())
}

/// `DOC` with one text replaced; the replacement must apply.
fn variant(from: &str, to: &str) -> String {
    assert!(DOC.contains(from), "the fixture must contain `{from}`");
    DOC.replacen(from, to, 1)
}

#[test]
fn the_valid_declarations_parse() {
    let doc = parse(DOC).unwrap();
    let modes: Vec<IdempotencyMode> = doc
        .functions
        .iter()
        .filter_map(|f| f.idempotency.as_ref().map(|i| i.mode()))
        .collect();
    assert_eq!(
        modes,
        vec![
            IdempotencyMode::Marker,
            IdempotencyMode::Key,
            IdempotencyMode::None
        ]
    );
}

#[test]
fn each_manifest_rule_refuses_its_case() {
    let cases = [
        // idempotency on a read.
        (
            "    url: \"https://api.example.com/me\"\n",
            "    url: \"https://api.example.com/me\"\n    idempotency: { mode: none }\n",
            "read_only",
        ),
        // smtp may only declare none.
        (
            "    idempotency: { mode: none }\n",
            "    idempotency: { mode: key, header: K, window_sec: 1 }\n",
            "smtp",
        ),
        // key: both header and field, or neither.
        (
            "field: nonce, window_sec",
            "field: nonce, header: K, window_sec",
            "exactly one",
        ),
        (
            "{ mode: key, field: nonce, window_sec: 120 }",
            "{ mode: key, window_sec: 120 }",
            "exactly one",
        ),
        // key: a field that is an argument.
        ("field: nonce,", "field: content,", "is an argument"),
        // key: a header the function already sets.
        (
            "{ mode: key, field: nonce,",
            "{ mode: key, header: accept,",
            "collides",
        ),
        // key: an empty field, or a body that cannot carry one.
        ("field: nonce,", "field: \" \",", "field is empty"),
        (
            "    body: { content: \"{{args.content}}\" }\n",
            "    body: \"{{args.content}}\"\n",
            "renders as a JSON object",
        ),
        // Only the exact whole-args form renders as the arguments object,
        // the same rule the renderer applies.
        (
            "    body: { content: \"{{args.content}}\" }\n",
            "    body: \" {{args}}\"\n",
            "renders as a JSON object",
        ),
        // key: a zero window.
        ("window_sec: 120", "window_sec: 0", "at least 1"),
        // marker: not a string argument, not in the body, literal text.
        ("arg: text", "arg: repo_id", "string property"),
        (
            "    body: \"{{args}}\"\n    args_schema: { type: object, properties: { repo:",
            "    body: { r: \"{{args.repo}}\" }\n    args_schema: { type: object, properties: { repo:",
            "not used in the body",
        ),
        (
            "    idempotency:\n      mode: marker",
            "    text_args: { text: literal }\n    idempotency:\n      mode: marker",
            "literal",
        ),
        // marker: no identity block.
        ("identity: { function: me, path: login }\n", "", "identity"),
        // marker: the reconcile function.
        (
            "reconcile: { function: list,",
            "reconcile: { function: gone,",
            "unknown function",
        ),
        (
            "reconcile: { function: list,",
            "reconcile: { function: say,",
            "read_only HTTP",
        ),
        ("args_from_send: [repo]", "args_from_send: [text]", "both"),
        ("since_arg: since", "since_arg: when", "since_arg"),
        // reconcile: a fixed arg the listing does not take, a required arg
        // it is not given.
        (
            "since_arg: since, text:",
            "since_arg: since, fixed_args: { page: 1 }, text:",
            "fixed arg `page`",
        ),
        (
            "properties: { repo: { type: string }, since: { type: string } } }",
            "properties: { repo: { type: string }, since: { type: string }, kind: { type: string } }, required: [kind] }",
            "required argument `kind`",
        ),
        (
            "text: text, author: user.login",
            "text: \"\", author: user.login",
            "non-empty",
        ),
        // identity must name a read.
        (
            "identity: { function: me,",
            "identity: { function: say,",
            "read_only",
        ),
        (
            "identity: { function: me,",
            "identity: { function: nope,",
            "unknown function",
        ),
        // identity is called with no arguments.
        (
            "    url: \"https://api.example.com/me\"\n",
            "    url: \"https://api.example.com/me\"\n    args_schema: { type: object, properties: { q: { type: string } }, required: [q] }\n",
            "no required arguments",
        ),
    ];
    for (from, to, says) in cases {
        let err = parse(&variant(from, to)).expect_err(says);
        assert!(
            err.contains(says),
            "`{to}` must be refused with `{says}`: {err}"
        );
    }
}

fn binding(on_unknown: Option<OnUnknown>, functions: FunctionsAllow) -> ConnectorBinding {
    ConnectorBinding {
        name: "x".into(),
        accounts: None,
        functions,
        max_calls: None,
        on_unknown,
    }
}

#[test]
fn on_unknown_round_trips_and_keeps_the_shorthand_when_unset() {
    let bare = binding(None, FunctionsAllow::All);
    assert_eq!(yaml_serde::to_string(&bare).unwrap().trim(), "x");
    let set = binding(Some(OnUnknown::Resend), FunctionsAllow::All);
    let yaml = yaml_serde::to_string(&set).unwrap();
    assert!(yaml.contains("on_unknown: resend"));
    let back: ConnectorBinding = yaml_serde::from_str(&yaml).unwrap();
    assert_eq!(back, set);
    assert!(yaml_serde::from_str::<ConnectorBinding>("{ name: x, on_unknown: maybe }").is_err());
}

fn playbook(on_unknown: &str, functions: &str) -> String {
    format!(
        r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - {{ id: s, type: start }}
  - id: a
    type: agent_task
    prompt: hi
    profile: x
    connectors: [{{ name: x, functions: {functions}, on_unknown: {on_unknown} }}]
  - {{ id: f, type: finish, outcome: success }}
edges:
  - {{ from: s, to: a }}
  - {{ from: a, to: f }}
"#
    )
}

fn facts() -> ValidationContext {
    let facts = ConnectorFacts {
        functions: vec!["list".into(), "post".into(), "say".into(), "merge".into()],
        write_functions: vec!["post".into(), "say".into(), "merge".into()],
        irreversible_functions: vec!["merge".into()],
        idempotency: BTreeMap::from([
            ("post".to_string(), IdempotencyMode::Marker),
            ("say".to_string(), IdempotencyMode::Key),
        ]),
        ..Default::default()
    };
    ValidationContext {
        connectors: BTreeMap::from([("x".to_string(), facts)]),
        ..Default::default()
    }
}

fn codes(yaml: &str, ctx: &ValidationContext) -> Vec<(&'static str, Severity)> {
    validate(&Playbook::from_yaml(yaml).unwrap(), ctx)
        .issues
        .into_iter()
        .filter(|i| i.code == "V69" || i.code == "V84")
        .map(|i| (i.code, i.severity))
        .collect()
}

#[test]
fn v69_fires_when_no_granted_function_can_take_the_policy() {
    let ctx = facts();
    assert_eq!(
        codes(&playbook("refuse", "[post]"), &ctx),
        vec![("V69", Severity::Error)]
    );
    assert_eq!(
        codes(&playbook("refuse", "read_only"), &ctx),
        vec![("V69", Severity::Error)]
    );
    assert!(codes(&playbook("refuse", "[say]"), &ctx).is_empty());
    assert!(codes(&playbook("refuse", "[post]"), &ValidationContext::default()).is_empty());
}

#[test]
fn v84_fires_on_a_resend_over_an_irreversible_function() {
    let ctx = facts();
    assert_eq!(
        codes(&playbook("resend", "[say, merge]"), &ctx),
        vec![("V84", Severity::Error)]
    );
    assert!(codes(&playbook("refuse", "[say, merge]"), &ctx).is_empty());
    assert!(codes(&playbook("resend", "[say]"), &ctx).is_empty());
    assert!(
        codes(
            &playbook("resend", "[say, merge]"),
            &ValidationContext::default()
        )
        .is_empty()
    );
}

/// A native imap write declares `none` like smtp, and nothing else.
#[test]
fn an_imap_write_may_declare_only_none() {
    let doc = |idem: &str| {
        format!(
            r#"
name: x
version: 0.1.0
functions:
  - name: mark
    description: d
    imap:
      connection: {{ host: h, port: "993", use_tls: "true", auth_method: password, username: u, password: p }}
      op: set_flags
      params: {{ folder: "{{{{args.folder}}}}", uids: "{{{{args.uids}}}}", seen: "true" }}
    args_schema: {{ type: object, properties: {{ folder: {{ type: string }}, uids: {{ type: string }} }} }}
    idempotency: {idem}
"#
        )
    };
    let ok = parse(&doc("{ mode: none }")).unwrap();
    assert_eq!(
        ok.functions[0].idempotency.as_ref().map(|i| i.mode()),
        Some(IdempotencyMode::None)
    );
    let err = parse(&doc("{ mode: key, header: K, window_sec: 1 }")).unwrap_err();
    assert!(
        err.contains("imap function `mark` may only declare"),
        "{err}"
    );
}
