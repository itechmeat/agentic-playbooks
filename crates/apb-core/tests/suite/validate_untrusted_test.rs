//! The untrusted-input rules: V179 refuses an agent that renders
//! untrusted text and can send outward without an accepted reason, V180
//! warns when it also reaches secrets, V181 refuses misplaced markers,
//! V182 warns about an acceptance of nothing.

use apb_core::schema::Playbook;
use apb_core::validate::{Severity, ValidationContext, validate};

fn issues(nodes: &str, extra: &str) -> Vec<(String, Severity, String)> {
    let yaml = format!(
        "schema: 2\nid: p\nname: P\nversion: 1.0.0\n{extra}defaults: {{ profile: dev }}\nnodes:\n  - {{ id: start, type: start }}\n{nodes}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n"
    );
    let playbook = Playbook::from_yaml(&yaml).unwrap();
    validate(&playbook, &ValidationContext::default())
        .issues
        .iter()
        .map(|i| (i.code.to_string(), i.severity, i.message.clone()))
        .filter(|(c, _, _)| matches!(c.as_str(), "V179" | "V180" | "V181" | "V182"))
        .collect()
}

const UNTRUSTED_PARAM: &str = "params:\n  - { name: body, type: text, untrusted: true }\n";

#[test]
fn an_unisolated_agent_reading_an_untrusted_param_is_v179() {
    let got = issues(
        "  - { id: w, type: agent_task, prompt: \"x {{params.body}}\" }\n",
        UNTRUSTED_PARAM,
    );
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].0, "V179");
    assert_eq!(got[0].1, Severity::Error);
    assert!(got[0].2.contains("accept the risk"), "{:?}", got[0].2);
}

#[test]
fn a_sandboxed_agent_without_network_or_connector_is_not_v179() {
    let got = issues(
        "  - { id: w, type: agent_task, prompt: \"x {{params.body}}\", isolation: { level: full, network: none } }\n",
        UNTRUSTED_PARAM,
    );
    assert!(got.iter().all(|(c, _, _)| c != "V179"), "{got:?}");
}

#[test]
fn an_accepted_risk_is_not_v179_but_still_v180_with_a_connector() {
    let got = issues(
        "  - { id: w, type: agent_task, prompt: \"x {{params.body}}\", accept_untrusted: \"labels come from params\", connectors: [{ name: gh }] }\n",
        UNTRUSTED_PARAM,
    );
    assert!(got.iter().all(|(c, _, _)| c != "V179"), "{got:?}");
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].0, "V180");
    assert_eq!(got[0].1, Severity::Warning);
    assert!(
        got[0].2.contains("connector gh with its account"),
        "{:?}",
        got[0].2
    );
}

#[test]
fn untrusted_on_a_node_that_is_not_agent_task_or_script_is_v181() {
    let got = issues("  - { id: g, type: human_review, untrusted: true }\n", "");
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].0, "V181");
    assert!(got[0].2.contains("agent_task and script"), "{:?}", got[0].2);
}

#[test]
fn a_param_that_is_secret_and_untrusted_is_v181() {
    let got = issues(
        "  - { id: w, type: agent_task, prompt: p }\n",
        "params:\n  - { name: token, type: text, secret: true, untrusted: true }\n",
    );
    assert_eq!(
        got.iter().filter(|(c, _, _)| c == "V181").count(),
        1,
        "{got:?}"
    );
    assert!(
        got.iter()
            .any(|(_, _, m)| m.contains("both secret and untrusted")),
        "{got:?}"
    );
}

#[test]
fn an_empty_reason_or_a_long_one_is_v181() {
    let got = issues(
        "  - { id: w, type: agent_task, prompt: \"x {{params.body}}\", accept_untrusted: \" \" }\n",
        UNTRUSTED_PARAM,
    );
    assert_eq!(
        got.iter().filter(|(c, _, _)| c == "V181").count(),
        1,
        "{got:?}"
    );
    let long = format!("\"{}\"", "a".repeat(301));
    let got = issues(
        &format!(
            "  - {{ id: w, type: agent_task, prompt: \"x {{{{params.body}}}}\", accept_untrusted: {long} }}\n"
        ),
        UNTRUSTED_PARAM,
    );
    assert_eq!(
        got.iter().filter(|(c, _, _)| c == "V181").count(),
        1,
        "{got:?}"
    );
}

#[test]
fn an_accepted_node_that_renders_nothing_or_cannot_send_is_v182() {
    // Renders no untrusted text.
    let got = issues(
        "  - { id: w, type: agent_task, prompt: p, accept_untrusted: \"nothing to accept\" }\n",
        "",
    );
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].0, "V182");
    assert_eq!(got[0].1, Severity::Warning);

    // Renders untrusted text but is isolated with no network and no
    // connector: nothing to accept either.
    let got = issues(
        "  - { id: w, type: agent_task, prompt: \"x {{params.body}}\", isolation: { level: full, network: none }, accept_untrusted: \"already safe\" }\n",
        UNTRUSTED_PARAM,
    );
    assert!(got.iter().all(|(c, _, _)| c != "V179"), "{got:?}");
    assert_eq!(
        got.iter().filter(|(c, _, _)| c == "V182").count(),
        1,
        "{got:?}"
    );
}

#[test]
fn an_empty_acceptance_reason_accepts_nothing() {
    let got = issues(
        "  - { id: w, type: agent_task, prompt: \"x {{params.body}}\", accept_untrusted: \"  \" }\n",
        UNTRUSTED_PARAM,
    );
    assert!(got.iter().any(|(c, _, _)| c == "V179"), "{got:?}");
    assert!(got.iter().any(|(c, _, _)| c == "V181"), "{got:?}");
}

#[test]
fn a_secret_read_by_a_writes_template_is_v180() {
    let got = issues(
        "  - { id: w, type: agent_task, prompt: \"x {{params.body}}\", accept_untrusted: \"labels only\", writes: \"{{params.spath}}\" }\n",
        "params:\n  - { name: body, type: text, untrusted: true }\n  - { name: spath, type: text, secret: true }\n",
    );
    assert!(
        got.iter()
            .any(|(c, _, m)| c == "V180" && m.contains("secret param spath")),
        "{got:?}"
    );
}
