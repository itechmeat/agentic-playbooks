//! The `budget` block: it parses and round-trips, a playbook without it
//! re-saves without the key, and each of V96-V98 fires on its case.

use apb_core::budget::OnNoEstimate;
use apb_core::schema::Playbook;
use apb_core::validate::{Severity, ValidationContext, validate};

fn playbook(budget: &str, work: &str) -> String {
    format!(
        "schema: 2\nid: b\nname: B\nversion: 1.0.0\n{budget}nodes:\n  - {{ id: start, type: start }}\n  - {work}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: done }}\n"
    )
}

const AGENT: &str = "{ id: work, type: agent_task, prompt: hi, profile: x }";
const SCRIPT: &str = "{ id: work, type: script, script: run.sh, runner: sh }";

fn codes(yaml: &str) -> Vec<(&'static str, Severity)> {
    let p = Playbook::from_yaml(yaml).unwrap();
    validate(&p, &ValidationContext::default())
        .issues
        .into_iter()
        .filter(|i| matches!(i.code, "V96" | "V97" | "V98"))
        .map(|i| (i.code, i.severity))
        .collect()
}

#[test]
fn a_valid_block_round_trips_and_has_no_budget_issue() {
    let yaml = playbook(
        "budget:\n  tokens: 400000\n  usd: 4.0\n  on_no_estimate: continue\n",
        AGENT,
    );
    let p = Playbook::from_yaml(&yaml).unwrap();
    let b = p.budget.clone().unwrap();
    assert_eq!(
        (b.tokens, b.usd, b.on_no_estimate),
        (Some(400_000), Some(4.0), OnNoEstimate::Continue)
    );
    let again = Playbook::from_yaml(&yaml_serde::to_string(&p).unwrap()).unwrap();
    assert_eq!(again.budget, p.budget);
    assert!(codes(&yaml).is_empty());
    // The policy defaults to `ask`.
    let p = Playbook::from_yaml(&playbook("budget:\n  tokens: 1000\n", AGENT)).unwrap();
    assert_eq!(p.budget.unwrap().on_no_estimate, OnNoEstimate::Ask);
}

#[test]
fn a_playbook_without_budget_resaves_without_the_key() {
    let p = Playbook::from_yaml(&playbook("", AGENT)).unwrap();
    assert!(p.budget.is_none());
    assert!(!yaml_serde::to_string(&p).unwrap().contains("budget"));
}

#[test]
fn v96_budget_without_a_limit() {
    let got = codes(&playbook("budget:\n  on_no_estimate: pause\n", AGENT));
    assert_eq!(got, vec![("V96", Severity::Error)]);
}

#[test]
fn v97_zero_negative_or_nan_limits() {
    for block in [
        "budget:\n  tokens: 0\n",
        "budget:\n  usd: 0\n",
        "budget:\n  usd: -1.5\n",
        "budget:\n  usd: .nan\n",
        "budget:\n  usd: .inf\n",
    ] {
        let got = codes(&playbook(block, AGENT));
        assert_eq!(got, vec![("V97", Severity::Error)], "block {block}");
    }
}

#[test]
fn v98_budget_without_a_gated_node_warns() {
    let got = codes(&playbook("budget:\n  tokens: 1000\n", SCRIPT));
    assert_eq!(got, vec![("V98", Severity::Warning)]);
}

#[test]
fn an_unknown_policy_is_a_parse_error() {
    let yaml = playbook("budget:\n  tokens: 1000\n  on_no_estimate: maybe\n", AGENT);
    assert!(Playbook::from_yaml(&yaml).is_err());
}
