//! Which run param values are sensitive, and how a param map is shown
//! without them.
//!
//! One rule serves every reader: remembered run input refuses to store a
//! sensitive value ([`crate::run_memory`]), and the run surfaces never show
//! one (the engine's run redactor). A value is sensitive when its param is
//! declared secret, when the param's name looks like a secret's, or when
//! the value itself looks like a credential.

use std::collections::{BTreeMap, BTreeSet};

use crate::token_shape::{looks_like_credential, name_looks_secret};

/// What a param map shows in place of a sensitive value.
pub const REDACTED: &str = "<redacted>";

/// Why a param value is sensitive, most specific first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sensitive {
    /// The param is declared `secret: true` (now or earlier in the run).
    Declared,
    /// The param's name looks like a secret's (`token`, `api_key`, ...).
    Named,
    /// The value looks like a credential ([`looks_like_credential`]).
    Value,
}

impl Sensitive {
    /// The machine code a refusal names.
    pub fn as_str(self) -> &'static str {
        match self {
            Sensitive::Declared => "secret_param",
            Sensitive::Named => "param_looks_secret",
            Sensitive::Value => "value_looks_secret",
        }
    }
}

/// Whether the value `value` of the param `name` is sensitive, and why;
/// `declared` says the param is declared secret.
pub fn sensitive(declared: bool, name: &str, value: &str) -> Option<Sensitive> {
    if declared {
        Some(Sensitive::Declared)
    } else if name_looks_secret(name) {
        Some(Sensitive::Named)
    } else if looks_like_credential(value) {
        Some(Sensitive::Value)
    } else {
        None
    }
}

/// `params` as a surface shows them: every sensitive value (a param named
/// in `declared` counts as declared secret) replaced by [`REDACTED`], and
/// the names of the replaced params in map order.
pub fn redacted(
    params: &BTreeMap<String, String>,
    declared: &BTreeSet<String>,
) -> (BTreeMap<String, String>, Vec<String>) {
    let mut names = Vec::new();
    let shown = params
        .iter()
        .map(|(name, value)| {
            if sensitive(declared.contains(name), name, value).is_some() {
                names.push(name.clone());
                (name.clone(), REDACTED.to_string())
            } else {
                (name.clone(), value.clone())
            }
        })
        .collect();
    (shown, names)
}

/// The declared secret params (`declared`) whose value in `params` is too
/// short for the scrubber to replace by value inside text
/// ([`crate::scrub::MIN_SECRET_LEN`], in bytes as the scrubber counts), in
/// map order. A surface still hides
/// the param itself; an output or an event that echoes the value keeps it.
pub fn too_short_to_scrub(
    params: &BTreeMap<String, String>,
    declared: &BTreeSet<String>,
) -> Vec<String> {
    params
        .iter()
        .filter(|(name, value)| {
            declared.contains(*name)
                && !value.is_empty()
                // Bytes, as the scrubber measures.
                && value.len() < crate::scrub::MIN_SECRET_LEN
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// The warning a start gives for [`too_short_to_scrub`] names; `None` when
/// there are none.
pub fn short_secret_warning(names: &[String]) -> Option<String> {
    (!names.is_empty()).then(|| {
        format!(
            "the value of the secret param {} is shorter than {} characters: a surface hides the param, but text that echoes the value (an output, an event) is not scrubbed of it",
            names.join(", "),
            crate::scrub::MIN_SECRET_LEN
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declared_value_under_the_scrub_floor_is_named_once() {
        let params: BTreeMap<String, String> = [
            ("pin", "1234"),
            ("token", "long-enough-value"),
            ("note", "abc"),
            ("empty", ""),
            ("wide", "\u{e9}\u{e9}\u{e9}"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let declared: BTreeSet<String> = ["pin", "token", "empty", "wide"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let names = too_short_to_scrub(&params, &declared);
        assert_eq!(names, vec!["pin".to_string()]);
        let warning = short_secret_warning(&names).unwrap();
        assert!(
            warning.contains("pin") && !warning.contains("1234"),
            "{warning}"
        );
        assert_eq!(short_secret_warning(&[]), None);
    }

    #[test]
    fn a_declared_param_is_sensitive_before_its_name_or_value() {
        assert_eq!(sensitive(true, "api_token", "x"), Some(Sensitive::Declared));
        assert_eq!(sensitive(false, "api_token", "x"), Some(Sensitive::Named));
        assert_eq!(sensitive(false, "message", "violet hedgehog"), None);
    }

    #[test]
    fn redacted_replaces_only_sensitive_values_and_names_them() {
        let params: BTreeMap<String, String> = [
            ("message".to_string(), "violet hedgehog".to_string()),
            ("topic".to_string(), "garden".to_string()),
        ]
        .into();
        let declared: BTreeSet<String> = ["message".to_string()].into();
        let (shown, names) = redacted(&params, &declared);
        assert_eq!(shown["message"], REDACTED);
        assert_eq!(shown["topic"], "garden");
        assert_eq!(names, vec!["message".to_string()]);
    }
}
