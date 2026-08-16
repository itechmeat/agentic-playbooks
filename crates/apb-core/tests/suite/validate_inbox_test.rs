//! V42 and V43: the two playbook-facing inbox rules. Both read
//! `ValidationContext::connectors`, so both are silent when the caller has
//! no connector facts to offer, exactly like V14 is silent about a global
//! profile it cannot resolve.

use apb_core::connector::resolve::ConnectorFacts;
use apb_core::validate::{Severity, ValidationContext, validate};
use std::collections::{BTreeMap, BTreeSet};

const PB: &str = r#"
schema: 2
id: read-inbox
name: Read inbox
version: 1.0.0
nodes:
  - id: start
    type: start
    title: Start
  - id: drain
    type: agent_task
    title: Drain
    profile: architect
    prompt: "read the inbox"
    connectors:
      - name: echo-hooks
        accounts: [main]
        functions: [inbox_read, inbox_ack]
  - id: done
    type: finish
    title: Done
    outcome: success
edges:
  - { from: start, to: drain }
  - { from: drain, to: done }
"#;

fn facts(has_webhook: bool, account_fields: &[&str]) -> BTreeMap<String, ConnectorFacts> {
    let mut accounts = BTreeMap::new();
    accounts.insert(
        "main".to_string(),
        account_fields
            .iter()
            .map(|f| f.to_string())
            .collect::<BTreeSet<String>>(),
    );
    BTreeMap::from([(
        "echo-hooks".to_string(),
        ConnectorFacts {
            has_webhook,
            inbox_functions: vec!["inbox_read".to_string(), "inbox_ack".to_string()],
            webhook_secret_fields: vec!["app_secret".to_string(), "verify_token".to_string()],
            accounts,
        },
    )])
}

fn ctx(connectors: BTreeMap<String, ConnectorFacts>) -> ValidationContext {
    ValidationContext {
        profiles: vec!["architect".into()],
        connectors,
        ..Default::default()
    }
}

fn issues(pb: &str, ctx: &ValidationContext) -> Vec<(&'static str, Severity)> {
    let playbook = apb_core::schema::Playbook::from_yaml(pb).unwrap();
    validate(&playbook, ctx)
        .issues
        .iter()
        .map(|i| (i.code, i.severity))
        .collect()
}

#[test]
fn a_fully_configured_ingest_connector_is_valid() {
    let c = ctx(facts(true, &["app_secret", "verify_token", "base_url"]));
    let found = issues(PB, &c);
    assert!(
        !found
            .iter()
            .any(|(code, _)| *code == "V42" || *code == "V43"),
        "expected no inbox findings, got {found:?}"
    );
}

#[test]
fn v42_fires_when_the_granted_connector_has_no_webhook_block() {
    let c = ctx(facts(false, &["app_secret", "verify_token"]));
    let found = issues(PB, &c);
    assert!(
        found.contains(&("V42", Severity::Error)),
        "expected V42, got {found:?}"
    );
    let playbook = apb_core::schema::Playbook::from_yaml(PB).unwrap();
    let report = validate(&playbook, &c);
    let msg = report
        .issues
        .iter()
        .find(|i| i.code == "V42")
        .map(|i| i.message.clone())
        .unwrap();
    assert!(msg.contains("echo-hooks"), "names the connector: {msg}");
    assert!(msg.contains("inbox_read"), "names the function: {msg}");
    assert!(!msg.contains('!'), "no exclamation marks: {msg}");
    assert!(!msg.contains('\u{2014}'), "no em-dashes: {msg}");
}

#[test]
fn v43_fires_when_the_selected_account_lacks_a_referenced_webhook_field() {
    // The webhook block references `app_secret` and `verify_token`; the
    // account defines only one of them, so a delivery could never verify.
    let c = ctx(facts(true, &["app_secret"]));
    let found = issues(PB, &c);
    assert!(
        found.contains(&("V43", Severity::Error)),
        "expected V43, got {found:?}"
    );
    let playbook = apb_core::schema::Playbook::from_yaml(PB).unwrap();
    let report = validate(&playbook, &c);
    let msg = report
        .issues
        .iter()
        .find(|i| i.code == "V43")
        .map(|i| i.message.clone())
        .unwrap();
    assert!(msg.contains("main"), "names the account: {msg}");
    assert!(
        msg.contains("verify_token"),
        "names the missing field: {msg}"
    );
    assert!(!msg.contains('!'), "no exclamation marks: {msg}");
}

#[test]
fn both_rules_are_silent_without_connector_facts() {
    let c = ctx(BTreeMap::new());
    let found = issues(PB, &c);
    assert!(
        !found
            .iter()
            .any(|(code, _)| *code == "V42" || *code == "V43"),
        "an empty fact map means the rules cannot decide, got {found:?}"
    );
}

#[test]
fn a_node_that_grants_no_inbox_function_is_not_checked() {
    let pb = PB.replace("functions: [inbox_read, inbox_ack]", "functions: [ping]");
    // No webhook block, no account fields: neither rule may fire, because
    // the node never asked for an inbox function.
    let c = ctx(facts(false, &[]));
    let found = issues(&pb, &c);
    assert!(
        !found
            .iter()
            .any(|(code, _)| *code == "V42" || *code == "V43"),
        "got {found:?}"
    );
}

#[test]
fn read_only_shorthand_is_treated_as_granting_the_inbox_functions() {
    // `functions: read_only` expands at run start to every read_only
    // function, which for an ingest connector includes inbox_read, so the
    // rules must still apply.
    let pb = PB.replace("functions: [inbox_read, inbox_ack]", "functions: read_only");
    let c = ctx(facts(false, &[]));
    let found = issues(&pb, &c);
    assert!(
        found.contains(&("V42", Severity::Error)),
        "expected V42 under the read_only shorthand, got {found:?}"
    );
}

#[test]
fn a_binding_with_no_accounts_list_checks_every_configured_account() {
    let pb = PB.replace("        accounts: [main]\n", "");
    let c = ctx(facts(true, &["app_secret"]));
    let found = issues(&pb, &c);
    assert!(
        found.contains(&("V43", Severity::Error)),
        "an omitted accounts list means every account is selectable, got {found:?}"
    );
}
