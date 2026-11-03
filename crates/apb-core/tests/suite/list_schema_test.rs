//! The `list:` block of an agent_task: it parses and round-trips (with a
//! `type: list` goal criterion), each of V85-V89 and V93-V95 fires on its
//! case and stays quiet on a valid list, and a connector item check is a
//! binding site of its own that an eval refuses by name.

use std::collections::BTreeMap;

use apb_core::connector::idempotency::IdempotencyMode;
use apb_core::connector::resolve::ConnectorFacts;
use apb_core::schema::{GoalCheck, Playbook};
use apb_core::validate::{Severity, ValidationContext, validate};

/// A valid list over a declared output field, checked by a judge.
const GOOD: &str = r#"
schema: 2
id: inbox
name: Inbox
version: 1.0.0
goal:
  statement: Every message is triaged
  criteria:
    - { description: All triaged, check: { type: list, node: triage } }
nodes:
  - { id: start, type: start }
  - id: search
    type: agent_task
    prompt: Find unread messages.
    profile: x
    outputs: { fields: [messages, total_unread] }
  - id: triage
    type: agent_task
    prompt: Triage the message named in the item section.
    profile: x
    connectors:
      - { name: imap, functions: [get_message, mark_read] }
    list:
      items:
        from: { node: search, field: messages }
        max_items: 100
      batch: { size: 10, parallel: 3, retries: 1 }
      item_outputs: [actionable, task_name]
      check:
        judge:
          state:
            subject: "{{item.subject}}"
            verdict: "{{item_output.actionable}}"
          instructions: Is the verdict plausible?
          criteria:
            accept: Plausible.
            accept_with_note: Plausible but vague.
            reject: Implausible.
          min_confidence: 0.6
          on_unavailable: accept_with_note
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: search }
  - { from: search, to: triage }
  - { from: triage, to: done }
"#;

fn imap_facts() -> ValidationContext {
    let mut idempotency = BTreeMap::new();
    idempotency.insert("mark_read".to_string(), IdempotencyMode::None);
    let mut connectors = BTreeMap::new();
    connectors.insert(
        "imap".to_string(),
        ConnectorFacts {
            functions: vec!["get_message".into(), "mark_read".into(), "move".into()],
            write_functions: vec!["mark_read".into(), "move".into()],
            idempotency,
            ..ConnectorFacts::default()
        },
    );
    ValidationContext {
        connectors,
        ..ValidationContext::default()
    }
}

/// `(code, severity)` of every list-rule issue (V85-V95) of `yaml`.
fn list_issues(yaml: &str, ctx: &ValidationContext) -> Vec<(&'static str, Severity)> {
    let p = Playbook::from_yaml(yaml).unwrap();
    validate(&p, ctx)
        .issues
        .into_iter()
        .filter(|i| {
            matches!(
                i.code,
                "V85" | "V86" | "V87" | "V88" | "V89" | "V93" | "V94" | "V95"
            )
        })
        .map(|i| (i.code, i.severity))
        .collect()
}

/// `GOOD` with its item check replaced by `check` (one YAML line under
/// `check:`), or with no check at all when `check` is empty.
fn with_check(check: &str) -> String {
    let start = GOOD.find("      check:\n").expect("GOOD has a check");
    let end = GOOD.find("  - { id: done").expect("GOOD has a done node");
    if check.is_empty() {
        return format!("{}{}", &GOOD[..start], &GOOD[end..]);
    }
    format!(
        "{}      check:\n        {check}\n{}",
        &GOOD[..start],
        &GOOD[end..]
    )
}

/// `GOOD` with `list.items` given a `where` line.
fn with_where(filter: &str) -> String {
    let yaml = GOOD.replace(
        "        max_items: 100",
        &format!("        max_items: 100\n        where: {filter}"),
    );
    assert_ne!(yaml, GOOD, "the where line went in");
    yaml
}

fn has(yaml: &str, code: &str, severity: Severity) -> bool {
    list_issues(yaml, &imap_facts()).contains(&(code, severity))
}

#[test]
fn a_valid_list_round_trips_and_raises_no_list_issue() {
    let p = Playbook::from_yaml(GOOD).unwrap();
    let spec = p.node("triage").and_then(|n| n.list()).unwrap();
    assert_eq!(spec.batch.parallel, 3);
    assert_eq!(spec.items.key, "id");
    let back = Playbook::from_yaml(&yaml_serde::to_string(&p).unwrap()).unwrap();
    assert_eq!(back.node("triage").and_then(|n| n.list()), Some(spec));
    assert_eq!(
        back.goal.unwrap().criteria[0].check,
        GoalCheck::List {
            node: "triage".into()
        }
    );
    assert_eq!(list_issues(GOOD, &imap_facts()), []);
}

#[test]
fn v85_list_sits_only_where_items_can_run() {
    let interactive = GOOD.replace(
        "    profile: x\n    connectors:",
        "    profile: x\n    interactive: true\n    connectors:",
    );
    assert!(has(&interactive, "V85", Severity::Error));
    let numbers = GOOD.replace("max_items: 100", "max_items: 501");
    assert!(has(&numbers, "V85", Severity::Error));
    let retries = GOOD.replace("retries: 1 }", "retries: 6 }");
    assert!(has(&retries, "V85", Severity::Error));
    let on_review = GOOD.replace(
        "  - { id: done, type: finish, outcome: success }",
        "  - id: gate\n    type: human_review\n    list: { items: { inline: [ { id: a } ] } }\n  - { id: done, type: finish, outcome: success }",
    );
    assert!(has(&on_review, "V85", Severity::Error));
}

#[test]
fn v86_the_source_must_be_one_declared_field_and_prompts_never_read_the_item() {
    let undeclared = GOOD.replace("field: messages", "field: mails");
    assert!(has(&undeclared, "V86", Severity::Error));
    let both = GOOD.replace(
        "        max_items: 100",
        "        max_items: 100\n        inline: [ { id: a } ]",
    );
    assert!(has(&both, "V86", Severity::Error));
    let dup_inline = GOOD.replace(
        "        from: { node: search, field: messages }",
        "        inline: [ { id: a }, { id: a } ]",
    );
    assert!(has(&dup_inline, "V86", Severity::Error));
    let reads_item = GOOD.replace(
        "Triage the message named in the item section.",
        "Triage {{item.subject}}.",
    );
    let issues = list_issues(&reads_item, &imap_facts());
    assert!(issues.contains(&("V86", Severity::Error)));
    let p = Playbook::from_yaml(&reads_item).unwrap();
    assert!(
        !validate(&p, &imap_facts())
            .issues
            .iter()
            .any(|i| i.code == "V13"),
        "the item read is reported once, as V86"
    );
}

/// `where.field` and `where.equals` go together for every source;
/// `equals: null` is a value, an object or a list is not (it would never
/// match); `where.state` names a known state.
#[test]
fn v86_a_where_filter_is_whole() {
    for equals in ["{ a: 1 }", "[ 1 ]"] {
        let yaml = with_where(&format!("{{ field: kind, equals: {equals} }}"));
        assert!(has(&yaml, "V86", Severity::Error), "{equals}");
    }
    assert!(has(&with_where("{ field: kind }"), "V86", Severity::Error));
    let inline = with_where("{ field: kind }").replace(
        "        from: { node: search, field: messages }",
        "        inline: [ { id: a } ]",
    );
    assert!(has(&inline, "V86", Severity::Error), "inline items too");
    assert!(!has(
        &with_where("{ field: kind, equals: null }"),
        "V86",
        Severity::Error
    ));
    let p = Playbook::from_yaml(&with_where("{ field: kind, equals: null }")).unwrap();
    let filter = p
        .node("triage")
        .unwrap()
        .list()
        .unwrap()
        .items
        .filter
        .clone();
    assert_eq!(filter.unwrap().equals, Some(serde_json::Value::Null));
}

/// From a list node `where` may set a state and a field together (both
/// must hold); the field still needs its `equals`, and a state still needs
/// a list source.
#[test]
fn v86_a_where_filter_may_join_a_state_and_a_field_from_a_list_node() {
    let downstream = |filter: &str| {
        GOOD.replace(
            "  - { id: done, type: finish, outcome: success }\n",
            &format!(
                "  - id: file\n    type: agent_task\n    prompt: File the item.\n    profile: x\n    list:\n      items:\n        from: {{ node: triage, field: items }}\n        where: {filter}\n      item_outputs: [gid]\n  - {{ id: done, type: finish, outcome: success }}\n"
            ),
        )
        .replace("  - { from: triage, to: done }", "  - { from: triage, to: file }\n  - { from: file, to: done }")
    };
    let both = downstream(r#"{ state: passed, field: actionable, equals: "true" }"#);
    assert!(Playbook::from_yaml(&both).unwrap().node("file").is_some());
    assert!(!has(&both, "V86", Severity::Error));
    assert!(has(
        &downstream("{ state: passed, field: actionable }"),
        "V86",
        Severity::Error
    ));
    assert!(has(
        &with_where(r#"{ state: passed, field: kind, equals: "a" }"#),
        "V86",
        Severity::Error
    ));
}

/// A node id ending in the item-check suffix would share that site.
#[test]
fn v85_a_node_id_may_not_take_the_item_check_site() {
    let yaml = GOOD
        .replace("id: search\n", "id: search#check\n")
        .replace("node: search,", "node: search#check,")
        .replace("to: search }", "to: search#check }")
        .replace("from: search,", "from: search#check,");
    assert!(has(&yaml, "V85", Severity::Error));
}

/// A judge node publishes its questions' fields without declaring them, so
/// a list may read its items from one.
#[test]
fn v86_a_judge_source_offers_its_question_fields() {
    let yaml = GOOD.replace(
        "    outputs: { fields: [messages, total_unread] }\n",
        "    outputs: { fields: [messages, total_unread] }\n  - id: pick\n    type: judge\n    state: { a: \"x\" }\n    questions:\n      messages: { type: noul, instructions: i }\n",
    )
    .replace("{ node: search, field: messages }", "{ node: pick, field: messages }")
    .replace("  - { from: search, to: triage }", "  - { from: search, to: pick }\n  - { from: pick, to: triage }");
    let p = Playbook::from_yaml(&yaml).unwrap();
    let issues = validate(&p, &imap_facts()).issues;
    assert!(!issues.iter().any(|i| i.code == "V86"), "{issues:?}");
}

#[test]
fn v87_the_check_names_one_source_reading_only_the_item() {
    let two = GOOD.replace(
        "      check:\n        judge:",
        "      check:\n        script: scripts/c.sh\n        judge:",
    );
    assert!(has(&two, "V87", Severity::Error));
    let bad_ref = GOOD.replace("{{item_output.actionable}}", "{{item_output.nope}}");
    assert!(has(&bad_ref, "V87", Severity::Error));
    let write_check = with_check(
        "connector: { name: imap, function: move, args: { uid: \"{{item.id}}\" }, accept_when: { path: ok, exists: true } }",
    );
    assert!(has(&write_check, "V87", Severity::Error));
    let read_check = write_check.replace("function: move", "function: get_message");
    assert!(!has(&read_check, "V87", Severity::Error));
    let undeclared_param = GOOD.replace("{{item.subject}}", "{{params.nope}}");
    assert!(has(&undeclared_param, "V87", Severity::Error));
    let empty_path = GOOD.replace("{{item.subject}}", "{{item.}}");
    assert!(has(&empty_path, "V87", Severity::Error));
    let equals_ref = with_check(
        "connector: { name: imap, function: get_message, accept_when: { path: uid, equals: \"{{item_output.nope}}\" } }",
    );
    assert!(
        has(&equals_ref, "V87", Severity::Error),
        "accept_when.equals is rendered, so it is checked"
    );
}

#[test]
fn v87_accept_when_contains_names_one_scalar_and_no_other_rule() {
    let contains = |rule: &str| {
        with_check(&format!(
            "connector: {{ name: imap, function: get_message, accept_when: {{ path: \"flags[].name\", {rule} }} }}"
        ))
    };
    for ok in [
        "contains: \"{{item_output.task_name}}\"",
        "contains: 3",
        "contains: true",
    ] {
        assert!(!has(&contains(ok), "V87", Severity::Error), "{ok}");
    }
    for bad in [
        "contains: [a]",
        "contains: { a: 1 }",
        "contains: null",
        "contains: a, equals: a",
        "contains: a, exists: true",
        "contains: \"{{item_output.nope}}\"",
    ] {
        assert!(has(&contains(bad), "V87", Severity::Error), "{bad}");
    }
}

/// Validation facts of the github connector apb ships, read from its
/// embedded manifest.
fn github_facts() -> ValidationContext {
    let doc = apb_core::connector::official::get("github")
        .and_then(|c| c.doc())
        .expect("the github connector is embedded");
    let mut connectors = BTreeMap::new();
    connectors.insert("github".to_string(), ConnectorFacts::of_manifest(&doc));
    ValidationContext {
        connectors,
        ..ValidationContext::default()
    }
}

#[test]
fn the_bundled_issue_triage_labels_each_issue_in_a_list_and_v88_still_refuses_an_undeclared_write()
{
    let yaml = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../bundled/playbooks/apb-issue-triage/playbook.yaml"),
    )
    .unwrap();
    let ctx = github_facts();
    let p = Playbook::from_yaml(&yaml).unwrap();
    let labels = p.node("label_bug").expect("a labelling node");
    assert!(labels.list().is_some(), "labels are applied per item");
    assert!(
        labels
            .list()
            .and_then(|l| l.connector_check())
            .and_then(|c| c.accept_when.contains.as_ref())
            .is_some(),
        "the engine reads each label back"
    );
    assert_eq!(list_issues(&yaml, &ctx), vec![], "list rules hold");
    let undeclared = yaml.replace("functions: [add_labels]", "functions: [add_assignees]");
    assert_ne!(undeclared, yaml);
    assert!(
        list_issues(&undeclared, &ctx).contains(&("V88", Severity::Error)),
        "add_assignees declares no idempotency"
    );
}

/// A repeatable write (github's `add_labels`) may serve a list step where
/// an undeclared one may not, and `on_unknown` over it is refused (V69):
/// there is no unknown send to act on.
#[test]
fn v88_accepts_a_repeatable_write_and_v69_refuses_on_unknown_over_it() {
    let ctx = github_facts();
    let labels = GITHUB_LIST.replace("FUNCTION", "add_labels");
    assert!(
        !list_issues(&labels, &ctx).contains(&("V88", Severity::Error)),
        "a repeatable write may serve a list step"
    );
    let assignees = GITHUB_LIST.replace("FUNCTION", "add_assignees");
    assert!(
        list_issues(&assignees, &ctx).contains(&("V88", Severity::Error)),
        "an undeclared write may not"
    );
    // `on_unknown` has nothing to act on over a repeatable-only grant: the
    // call is retried anyway, so V69 refuses it as for an undeclared one.
    for policy in ["resend", "refuse"] {
        let resend = labels.replace(
            "functions: [add_labels] }",
            &format!("functions: [add_labels], on_unknown: {policy} }}"),
        );
        assert_ne!(resend, labels);
        let p = Playbook::from_yaml(&resend).unwrap();
        assert!(
            validate(&p, &ctx)
                .issues
                .iter()
                .any(|i| i.code == "V69" && i.severity == Severity::Error),
            "on_unknown: {policy}"
        );
    }
}

/// GitHub connector 0.2.1 declares `repeatable` on both label functions;
/// an apb older than that mode cannot read it (its parser denies an unknown
/// mode) and needs the 0.2.0 manifest back (docs/CONNECTORS.md). This apb
/// reads both: the 0.2.0 form (no declaration) parses and stays untracked
/// as before, only without the list-step use.
#[test]
fn the_github_manifest_parses_with_and_without_the_repeatable_declaration() {
    let current = include_str!("../../../../connectors/github/connector.yaml");
    let declaration = "    idempotency: { mode: repeatable }\n";
    let earlier = current
        .replace(declaration, "")
        .replacen("version: 0.2.1", "version: 0.2.0", 1);
    for (text, repeatable) in [(current, true), (earlier.as_str(), false)] {
        let doc = apb_core::connector::def::ConnectorDoc::from_yaml(text, "github").unwrap();
        for name in ["add_labels", "remove_label"] {
            let f = doc.functions.iter().find(|f| f.name == name).unwrap();
            assert_eq!(f.is_repeatable(), repeatable, "{name}");
            assert!(f.send_once().is_none(), "{name}: never tracked as a send");
        }
    }
}

/// A list step granted one github write, `FUNCTION`.
const GITHUB_LIST: &str = r#"schema: 2
id: label-each
name: Label each
version: 1.0.0
nodes:
  - { id: s, type: start }
  - id: each
    type: agent_task
    prompt: Label the issue named in the item section.
    profile: x
    list:
      items: { inline: [ { id: a }, { id: b } ] }
    connectors:
      - { name: github, accounts: [default], functions: [FUNCTION] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: each }
  - { from: each, to: done }
"#;

#[test]
fn v88_a_list_step_writes_only_through_send_once() {
    let undeclared = GOOD.replace(
        "functions: [get_message, mark_read]",
        "functions: [get_message, move]",
    );
    assert!(has(&undeclared, "V88", Severity::Error));
    assert!(
        !has(GOOD, "V88", Severity::Error),
        "`none` declares send-once"
    );
}

#[test]
fn v89_a_judge_check_answers_with_a_known_verdict() {
    let unknown_key = GOOD.replace("reject: Implausible.", "maybe: Unsure.");
    assert!(has(&unknown_key, "V89", Severity::Error));
    let confidence = GOOD.replace("min_confidence: 0.6", "min_confidence: 1.5");
    assert!(has(&confidence, "V89", Severity::Error));
    let fallback = GOOD.replace("on_unavailable: accept_with_note", "on_unavailable: shrug");
    assert!(has(&fallback, "V89", Severity::Error));
}

#[test]
fn v93_and_v94_warn_about_self_reported_items_and_a_capped_pool() {
    let unchecked = with_check("");
    assert!(has(&unchecked, "V93", Severity::Warning));
    let not_passed = GOOD.replace(
        "      item_outputs:",
        "      rerun: not_passed\n      item_outputs:",
    );
    assert!(has(&not_passed, "V93", Severity::Warning), "no cycle");
    let wide = GOOD.replace("parallel: 3", "parallel: 8");
    assert!(has(&wide, "V94", Severity::Warning));
    assert!(!has(GOOD, "V94", Severity::Warning));
}

#[test]
fn v95_a_list_goal_criterion_names_a_list_node() {
    let wrong = GOOD.replace("node: triage }", "node: search }");
    assert!(has(&wrong, "V95", Severity::Error));
}

#[test]
fn a_connector_check_is_its_own_site_and_an_eval_refuses_it() {
    let yaml = with_check(
        "connector: { name: imap, function: get_message, accept_when: { path: uid, exists: true } }",
    );
    let p = Playbook::from_yaml(&yaml).unwrap();
    let site = p
        .connector_sites()
        .find(|(id, _)| id == "triage#check")
        .map(|(_, b)| b.into_owned())
        .expect("the item check is a binding site");
    assert_eq!(site[0].name, "imap");
    let refusals = apb_core::eval::refusal(&p);
    assert!(
        refusals
            .iter()
            .any(|r| r.contains("the item check of node `triage`")),
        "{refusals:?}"
    );
}
