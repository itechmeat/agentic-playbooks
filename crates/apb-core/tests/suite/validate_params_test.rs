//! Secret params: V99 refuses a default, V100 anything but text, and a
//! plain param re-saves without the flag.

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
        .filter(|i| matches!(i.code, "V99" | "V100"))
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
