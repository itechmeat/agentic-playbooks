//! `retry_delay`: it parses on an `agent_task` and on `defaults`, round-trips,
//! the node's own block wins, and each of V126-V130 fires on its case and
//! stays silent on the valid one.

use apb_core::schema::{Playbook, RetryDelay};
use apb_core::validate::{Severity, ValidationContext, validate};

fn playbook(defaults: &str, work: &str) -> String {
    format!(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\n{defaults}nodes:\n  - {{ id: start, type: start }}\n  - {work}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: done }}\n"
    )
}

fn agent(extra: &str) -> String {
    format!("{{ id: work, type: agent_task, prompt: hi, profile: x{extra} }}")
}

fn codes(yaml: &str) -> Vec<(&'static str, Severity)> {
    let p = Playbook::from_yaml(yaml).unwrap();
    validate(&p, &ValidationContext::default())
        .issues
        .into_iter()
        .filter(|i| matches!(i.code, "V126" | "V127" | "V128" | "V129" | "V130"))
        .map(|i| (i.code, i.severity))
        .collect()
}

fn node_codes(delay: &str) -> Vec<(&'static str, Severity)> {
    codes(&playbook(
        "",
        &agent(&format!(", max_retries: 2, retry_delay: {delay}")),
    ))
}

#[test]
fn a_node_block_parses_round_trips_and_wins_over_defaults() {
    let yaml = playbook(
        "defaults:\n  retry_delay: { initial: 10s }\n",
        &agent(", max_retries: 3, retry_delay: { initial: 1m, factor: 3, max: 15m }"),
    );
    let p = Playbook::from_yaml(&yaml).unwrap();
    let work = p.node("work").unwrap();
    let own = RetryDelay {
        initial: "1m".into(),
        factor: 3.0,
        max: Some("15m".into()),
    };
    assert_eq!(p.effective_retry_delay(work), Some(&own));
    assert_eq!(p.effective_max_retries(work), 3);
    let again = Playbook::from_yaml(&yaml_serde::to_string(&p).unwrap()).unwrap();
    assert_eq!(
        again.effective_retry_delay(again.node("work").unwrap()),
        Some(&own)
    );
    assert_eq!(again.defaults.retry_delay, p.defaults.retry_delay);
}

#[test]
fn defaults_apply_and_factor_defaults_to_two() {
    let yaml = playbook(
        "defaults:\n  max_retries: 1\n  retry_delay: { initial: 30s }\n",
        &agent(""),
    );
    let p = Playbook::from_yaml(&yaml).unwrap();
    let d = p.effective_retry_delay(p.node("work").unwrap()).unwrap();
    assert_eq!(d.factor, 2.0);
    assert_eq!(d.initial_secs(), Some(30));
    assert_eq!(d.max_secs(), Some(604_800), "7 days");
    assert!(codes(&yaml).is_empty());
    // A factor the author left out is not written back on a re-save.
    assert!(!yaml_serde::to_string(&p).unwrap().contains("factor"));
}

/// A misspelt key is refused by the parser, and a value that is not a
/// duration reaches the validator, which names the field.
#[test]
fn a_misspelt_key_is_refused_and_a_non_duration_is_named() {
    let typo = playbook(
        "",
        &agent(", max_retries: 2, retry_delay: { initial: 1m, facter: 3 }"),
    );
    let err = Playbook::from_yaml(&typo).unwrap_err().to_string();
    assert!(err.contains("facter"), "{err}");
    for value in ["1.5", "-5", "true"] {
        assert_eq!(
            node_codes(&format!("{{ initial: {value} }}")),
            vec![("V126", Severity::Error)],
            "{value}"
        );
    }
}

#[test]
fn a_playbook_without_the_field_re_saves_without_it() {
    let yaml = playbook("", &agent(", max_retries: 2"));
    let p = Playbook::from_yaml(&yaml).unwrap();
    assert!(!yaml_serde::to_string(&p).unwrap().contains("retry_delay"));
    assert!(p.effective_retry_delay(p.node("work").unwrap()).is_none());
}

#[test]
fn v126_initial_must_parse_and_be_between_one_second_and_seven_days() {
    assert!(node_codes("{ initial: 1s }").is_empty());
    assert_eq!(
        node_codes("{ initial: soon }"),
        vec![("V126", Severity::Error)]
    );
    assert_eq!(
        node_codes("{ initial: 0 }"),
        vec![("V126", Severity::Error)]
    );
    assert_eq!(
        node_codes("{ initial: 8d }"),
        vec![("V126", Severity::Error)]
    );
}

#[test]
fn v127_factor_must_be_between_one_and_ten() {
    assert!(node_codes("{ initial: 1s, factor: 1 }").is_empty());
    assert!(node_codes("{ initial: 1s, factor: 10 }").is_empty());
    assert_eq!(
        node_codes("{ initial: 1s, factor: 0.5 }"),
        vec![("V127", Severity::Error)]
    );
    assert_eq!(
        node_codes("{ initial: 1s, factor: 11 }"),
        vec![("V127", Severity::Error)]
    );
}

#[test]
fn v128_max_must_parse_and_lie_between_initial_and_seven_days() {
    assert!(node_codes("{ initial: 1m, max: 7d }").is_empty());
    assert_eq!(
        node_codes("{ initial: 1m, max: 30s }"),
        vec![("V128", Severity::Error)]
    );
    assert_eq!(
        node_codes("{ initial: 1m, max: 8d }"),
        vec![("V128", Severity::Error)]
    );
    assert_eq!(
        node_codes("{ initial: 1m, max: later }"),
        vec![("V128", Severity::Error)]
    );
}

#[test]
fn v128_is_checked_on_defaults_too() {
    let yaml = playbook(
        "defaults:\n  max_retries: 1\n  retry_delay: { initial: 1m, max: 10s }\n",
        &agent(""),
    );
    assert_eq!(codes(&yaml), vec![("V128", Severity::Error)]);
}

#[test]
fn v129_a_list_node_may_not_carry_a_retry_delay() {
    let list = "{ id: work, type: agent_task, prompt: hi, profile: x, max_retries: 1, \
                list: { items: { inline: [ { id: a } ] } }";
    let with = playbook("", &format!("{list}, retry_delay: {{ initial: 1m }} }}"));
    assert!(codes(&with).contains(&("V129", Severity::Error)));
    let without = playbook("", &format!("{list} }}"));
    assert!(codes(&without).is_empty());
}

#[test]
fn defaults_never_apply_to_a_list_node() {
    let yaml = playbook(
        "defaults:\n  max_retries: 1\n  retry_delay: { initial: 1m }\n",
        "{ id: work, type: agent_task, prompt: hi, profile: x, list: { items: { inline: [ { id: a } ] } } }",
    );
    let p = Playbook::from_yaml(&yaml).unwrap();
    assert!(p.effective_retry_delay(p.node("work").unwrap()).is_none());
    assert!(!codes(&yaml).contains(&("V129", Severity::Error)));
}

#[test]
fn v130_warns_when_no_retry_uses_the_delay() {
    assert_eq!(
        codes(&playbook("", &agent(", retry_delay: { initial: 1m }"))),
        vec![("V130", Severity::Warning)]
    );
    assert_eq!(
        codes(&playbook(
            "defaults:\n  retry_delay: { initial: 1m }\n",
            &agent("")
        )),
        vec![("V130", Severity::Warning)]
    );
    assert!(
        codes(&playbook(
            "defaults:\n  max_retries: 2\n  retry_delay: { initial: 1m }\n",
            &agent("")
        ))
        .is_empty()
    ); // Every retrying node has its own block: the defaults are never used.
    let own_only = "schema: 2\nid: r\nname: R\nversion: 1.0.0\n\
        defaults:\n  retry_delay: { initial: 1m }\nnodes:\n  - { id: start, type: start }\n  \
        - { id: a, type: agent_task, prompt: hi, profile: x, max_retries: 2, retry_delay: { initial: 5s } }\n  \
        - { id: b, type: agent_task, prompt: hi, profile: x }\n  \
        - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  \
        - { from: a, to: b }\n  - { from: b, to: done }\n";
    assert_eq!(codes(own_only), vec![("V130", Severity::Warning)]);
}
