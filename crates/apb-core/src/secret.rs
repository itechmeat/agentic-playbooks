//! The sealed type every resolved secret travels in.
//!
//! A [`Secret`] is a resolved credential: a connector account field, a
//! decision-provider key, a server API key or session token, a run-hook key.
//! It cannot leak by accident:
//!
//! - `Debug` and `Display` print `***`, so a `{:?}` of a struct holding one,
//!   a log line or an error message never carries the value;
//! - it implements neither `Serialize` nor `Deserialize`, so it cannot reach
//!   a state file, a journal line or an API response;
//! - it has no `PartialEq` or `Hash`, so a comparison cannot be a plain,
//!   timing-leaking `==`: [`Secret::ct_eq`] is the only way to compare.
//!
//! The value is read only through [`Secret::expose_secret`], which keeps
//! every intended exposure (an outgoing request header, a login call, a
//! one-time print) greppable.
//!
//! ```compile_fail
//! let s = apb_core::secret::Secret::new("value");
//! let _ = serde_json::to_string(&s);
//! ```
//!
//! ```compile_fail
//! let a = apb_core::secret::Secret::new("value");
//! let b = apb_core::secret::Secret::new("value");
//! let _ = a == b;
//! ```

use std::fmt;
use std::sync::Arc;

use subtle::ConstantTimeEq;

/// What `Debug` and `Display` print instead of the value, and what
/// [`Secret::scrub`] puts in its place.
pub const MASK: &str = "***";

/// A resolved secret value. Cloning shares the same allocation.
#[derive(Clone)]
pub struct Secret(Arc<str>);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(Arc::from(value.into()))
    }

    /// The plain value. Every call site is an intended exposure: the place a
    /// request is signed, a login is sent or a key is printed once.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Constant-time comparison with a presented value (see [`ct_eq_str`]).
    pub fn ct_eq(&self, presented: &str) -> bool {
        ct_eq_str(&self.0, presented)
    }

    /// `text` with every occurrence of the value replaced by [`MASK`]. An
    /// empty secret changes nothing.
    pub fn scrub(&self, text: &str) -> String {
        self.scrub_as(text, MASK)
    }

    /// `text` with every occurrence of the value replaced by `with`. An
    /// empty secret changes nothing.
    pub fn scrub_as(&self, text: &str, with: &str) -> String {
        if self.is_empty() || !text.contains(&*self.0) {
            return text.to_string();
        }
        text.replace(&*self.0, with)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(MASK)
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(MASK)
    }
}

/// Constant-time string comparison. Lengths are not secret (a length mismatch
/// returns immediately), the contents are: equal-length inputs are compared
/// with `subtle::ConstantTimeEq` so no byte position leaks through timing.
/// This is the one secret comparison of the workspace.
pub fn ct_eq_str(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

/// A secret to scrub out of text that leaves the process (a connector
/// response, an error message), with the label that names it in the
/// replacement: an env var name, or `cmd:<field>` for a command-sourced one.
/// The label never carries the value.
///
/// A value is caught as it was resolved and in the forms a request carries
/// it: percent-encoded as a query or form value and as a path segment. The
/// Basic credential of an HTTP call (`user:password` in base64) is one more
/// redaction, built with [`Redaction::basic`].
#[derive(Clone, Debug)]
pub struct Redaction {
    pub value: Secret,
    pub label: String,
}

impl Redaction {
    pub fn new(value: Secret, label: impl Into<String>) -> Self {
        Redaction {
            value,
            label: label.into(),
        }
    }

    /// The redaction of an HTTP Basic credential: the base64 of
    /// `user:password` that the `Authorization` header carries.
    pub fn basic(user: &str, password: &Secret, label: impl Into<String>) -> Self {
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(format!("{user}:{}", password.expose_secret()));
        Redaction::new(Secret::new(encoded), label)
    }

    /// The value as resolved and its URL-encoded forms, each once.
    fn forms(&self) -> Vec<Secret> {
        let raw = self.value.expose_secret();
        let mut out = vec![self.value.clone()];
        for form in [
            crate::url_component::encode(raw),
            crate::url_component::encode_path_segment(raw),
        ] {
            if !out.iter().any(|s| s.ct_eq(&form)) {
                out.push(Secret::new(form));
            }
        }
        out
    }

    /// `text` with the value, in every form, replaced by `[redacted:<label>]`.
    pub fn apply(&self, text: &str) -> String {
        Self::apply_all(text, std::slice::from_ref(self))
    }

    /// `text` with every redaction applied, see [`Self::apply_all_owned`].
    pub fn apply_all(text: &str, redactions: &[Redaction]) -> String {
        Self::apply_all_owned(text.to_string(), redactions)
    }

    /// `text` with every form of every redaction replaced by its label.
    /// The longest form goes first, whatever the order of `redactions`, so
    /// a value that contains another one is replaced whole instead of
    /// leaving its tail. Returns `text` itself when nothing matches.
    pub fn apply_all_owned(text: String, redactions: &[Redaction]) -> String {
        let mut forms: Vec<(Secret, &str)> = redactions
            .iter()
            .flat_map(|r| r.forms().into_iter().map(move |f| (f, r.label.as_str())))
            .filter(|(f, _)| !f.is_empty())
            .collect();
        forms.sort_by_key(|(f, _)| std::cmp::Reverse(f.len()));
        let mut out = text;
        for (form, label) in forms {
            if out.contains(form.expose_secret()) {
                out = form.scrub_as(&out, &format!("[redacted:{label}]"));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_mask_the_value() {
        let s = Secret::new("hunter2-value");
        assert_eq!(format!("{s:?}"), MASK);
        assert_eq!(format!("{s}"), MASK);
        let wrapped = Some(Redaction::new(s, "TOKEN"));
        let shown = format!("{wrapped:?}");
        assert!(!shown.contains("hunter2"), "value leaked through Debug");
        assert!(shown.contains("TOKEN"));
    }

    #[test]
    fn ct_eq_matches_only_the_exact_value() {
        let s = Secret::new("abc");
        assert!(s.ct_eq("abc"));
        assert!(!s.ct_eq("abd"));
        assert!(!s.ct_eq("abcd"));
        assert!(!s.ct_eq(""));
        assert!(Secret::new("").ct_eq(""));
    }

    #[test]
    fn ct_eq_str_is_plain_equality_at_the_edges() {
        assert!(ct_eq_str("abc", "abc"));
        assert!(!ct_eq_str("abc", "abd"));
        assert!(!ct_eq_str("abc", "abcd"), "a longer value");
        assert!(!ct_eq_str("abcd", "abc"), "a prefix");
        assert!(!ct_eq_str("", "abc"));
        assert!(!ct_eq_str("abc", ""));
        assert!(ct_eq_str("", ""));
    }

    #[test]
    fn scrub_masks_every_occurrence_and_ignores_an_empty_secret() {
        let s = Secret::new("tok-123");
        assert_eq!(s.scrub("a tok-123 b tok-123"), "a *** b ***");
        assert_eq!(Secret::new("").scrub("unchanged"), "unchanged");
    }

    #[test]
    fn redactions_label_the_replacement() {
        let reds = [
            Redaction::new(Secret::new("first-secret"), "A_TOKEN"),
            Redaction::new(Secret::new("second-secret"), "cmd:password"),
        ];
        let out = Redaction::apply_all("x first-secret y second-secret", &reds);
        assert_eq!(out, "x [redacted:A_TOKEN] y [redacted:cmd:password]");
    }

    /// A value that contains another is replaced whole whatever the order
    /// the redactions come in.
    #[test]
    fn the_longer_value_is_redacted_first() {
        let reds = [
            Redaction::new(Secret::new("abc"), "SHORT"),
            Redaction::new(Secret::new("abcdef"), "LONG"),
        ];
        let out = Redaction::apply_all("x abcdef y abc", &reds);
        assert_eq!(out, "x [redacted:LONG] y [redacted:SHORT]");
    }

    /// The URL-encoded forms a request carries are caught too.
    #[test]
    fn percent_encoded_forms_are_redacted() {
        let reds = [Redaction::new(Secret::new("p@ss w/rd"), "PASS")];
        for text in ["q=p%40ss%20w%2Frd", "/bot/p@ss%20w%2Frd/x"] {
            let out = Redaction::apply_all(text, &reds);
            assert!(!out.contains("ss%20w"), "an encoded form survived: {out}");
            assert!(out.contains("[redacted:PASS]"), "{out}");
        }
    }

    /// The Basic credential is caught as the header carries it.
    #[test]
    fn a_basic_credential_is_redacted_in_its_base64_form() {
        let red = Redaction::basic("user", &Secret::new("hunter2"), "basic");
        // base64("user:hunter2")
        let out = red.apply("echoed: Basic dXNlcjpodW50ZXIy");
        assert_eq!(out, "echoed: Basic [redacted:basic]");
    }
}
