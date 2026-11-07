//! Secret params: V99 refuses a default, V100 anything but text, and a
//! plain param re-saves without the flag. Search params: V160 refuses a
//! secret one, V161 one named like a secret, V162 more than eight.

use apb_core::schema::Playbook;
use apb_core::validate::{ValidationContext, validate};

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
