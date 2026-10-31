//! The one scrubber of text apb hands to someone outside the run or keeps
//! where a person or another agent reads it: a decision request, a
//! notification, the HTML run report, a journaled probe failure, a stored
//! known problem, a captured synopsis.
//!
//! [`Scrubber::scrub`] applies, in this order: known secret values (the
//! variables every installed connector references, and the provider keys
//! themselves) become `[redacted]`; absolute paths under the project become
//! repo-relative and other home paths `~/...`; token-shaped strings (JWTs,
//! well-known key prefixes, bearer values, long mixed-case runs with digits)
//! become `[redacted-token]`; e-mail addresses become `[email]`. Git hashes
//! and UUIDs are kept: they are identifiers, not credentials.
//!
//! [`Scrubber::flags`] answers whether a text still holds a secret after a
//! scrub: a configured secret value or a credential shape
//! ([`crate::token_shape::looks_like_credential`]). A known problem drops a
//! sighting (or keeps no excerpt) on it, so it stays as narrow as the
//! scrub: prose such as "the password: authentication rules" passes.
//!
//! [`Scrubber::refuses`] is the wider check of a writer that stores text it
//! must not rewrite (a captured playbook draft): since a refusal rewrites
//! nothing, it also refuses any value of 8 or more characters after a key
//! name and bare keys of 32 to 64 characters
//! ([`crate::token_shape::refuses_to_store`]).

use std::sync::LazyLock;

use regex::Regex;

use crate::secret::Secret;

/// Shorter values are not scrubbed by value: a two-letter secret would blank
/// every occurrence of that pair in the text. Token shapes still apply.
const MIN_SECRET_LEN: usize = 6;

static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)*\.[A-Za-z]{2,}")
        .expect("valid")
});
/// A home directory with its user name: `/home/u`, `/Users/u`,
/// `/mnt/c/Users/u` and `C:\Users\u` (the Windows forms in any case, a
/// URL's `/users/` path is left alone), with forward slashes
/// (`file:///C:/Users/u`) or JSON-escaped backslashes (`C:\\Users\\u`).
static HOME_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?:/home/|/Users/|/mnt/[a-z]/(?i:users)/|(?i:[a-z]:(?:\\{1,2}|/)users(?:\\{1,2}|/)))[^/\\\s:"'`]+"#,
    )
    .expect("valid")
});

/// Why [`Scrubber::flags`] refused a text. Names the kind of match only:
/// echoing even a part of the value would carry it into a report meant to
/// prove it was kept out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrubFlag {
    /// A value an installed connector or a provider key holds.
    SecretValue,
    /// A credential-shaped string ([`crate::token_shape`]).
    TokenShape,
}

impl ScrubFlag {
    pub fn as_str(self) -> &'static str {
        match self {
            ScrubFlag::SecretValue => "configured secret value",
            ScrubFlag::TokenShape => "credential-shaped value",
        }
    }
}

/// Scrubs text for one workspace. `Debug` never prints the secret values.
#[derive(Default)]
pub struct Scrubber {
    /// Secret values, longest first so a value containing another is
    /// replaced whole.
    secrets: Vec<Secret>,
    /// The project root, as written in paths (no trailing slash).
    root: Option<String>,
    home: Option<String>,
}

impl std::fmt::Debug for Scrubber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scrubber")
            .field("secrets", &self.secrets.len())
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl Scrubber {
    /// The scrubber of the workspace at `root`: the secret values every
    /// installed connector references, plus `extra` (a provider's keys).
    pub fn for_workspace(root: &std::path::Path, extra: Vec<Secret>) -> Self {
        let mut secrets = crate::connector::resolve::resolve_referenced(root);
        secrets.extend(extra);
        Self::new(secrets, root)
    }

    pub fn new(mut secrets: Vec<Secret>, root: &std::path::Path) -> Self {
        secrets.retain(|s| s.len() >= MIN_SECRET_LEN);
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup_by(|a, b| a.ct_eq(b.expose_secret()));
        let root = std::fs::canonicalize(root)
            .unwrap_or_else(|_| root.to_path_buf())
            .to_string_lossy()
            .trim_end_matches('/')
            .to_string();
        let home = std::env::var("HOME")
            .ok()
            .map(|h| h.trim_end_matches('/').to_string())
            .filter(|h| h.len() > 1);
        Scrubber {
            secrets,
            root: (root.len() > 1).then_some(root),
            home,
        }
    }

    /// `text` with every secret value, local path, token shape and e-mail
    /// address replaced.
    pub fn scrub(&self, text: &str) -> String {
        let mut out = text.to_string();
        for s in &self.secrets {
            out = s.scrub_as(&out, "[redacted]");
        }
        if let Some(root) = &self.root {
            out = out
                .replace(&format!("{root}/"), "")
                .replace(root.as_str(), ".");
        }
        if let Some(home) = &self.home {
            out = out.replace(&format!("{home}/"), "~/");
        }
        out = HOME_PATH.replace_all(&out, "~").into_owned();
        out = crate::token_shape::redact_tokens(&out);
        EMAIL.replace_all(&out, "[email]").into_owned()
    }

    /// [`Scrubber::scrub`] for text a person reads as written: every hidden
    /// character but a line break or a tab becomes `?`
    /// ([`crate::text::visible_keeping_layout`]).
    pub fn shown_text(&self, text: &str) -> String {
        crate::text::visible_keeping_layout(&self.scrub(text))
    }

    /// The one path of text that is stored where a person or another agent
    /// reads it later (a known problem's symptom or source excerpt, an
    /// example of a discovered repeat): [`Scrubber::shown_text`] of the
    /// whole text, cut to `max_chars` characters; `None` when only
    /// whitespace is left or the stored part still [`Scrubber::flags`]. The
    /// scrub runs before the cut, so a secret the cut runs through is
    /// never kept in part. A writer that must not keep even a keyed value
    /// checks [`Scrubber::refuses`] on the result as well.
    pub fn stored_text(&self, text: &str, max_chars: usize) -> Option<String> {
        let shown = self.shown_text(text);
        let cut: String = shown.chars().take(max_chars).collect();
        (!cut.trim().is_empty() && self.flags(&cut).is_none()).then_some(cut)
    }

    /// The engine's historical name of [`Scrubber::scrub`].
    pub fn redact(&self, text: &str) -> String {
        self.scrub(text)
    }

    /// [`Scrubber::scrub`] over every string of a JSON value, object keys
    /// included (a map keyed by a path, an address or a token). Scrub a
    /// value before serialising it: serialising escapes quotes, backslashes
    /// and newlines, and a secret written that way no longer matches. Two
    /// keys that scrub alike keep the later entry.
    pub fn redact_value(&self, v: serde_json::Value) -> serde_json::Value {
        use serde_json::Value;
        match v {
            Value::String(s) => Value::String(self.scrub(&s)),
            Value::Array(items) => {
                Value::Array(items.into_iter().map(|i| self.redact_value(i)).collect())
            }
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(k, v)| (self.scrub(&k), self.redact_value(v)))
                    .collect(),
            ),
            other => other,
        }
    }

    /// Whether `text` holds a configured secret value or a credential-shaped
    /// string, and which; `None` when it holds neither. Paths and e-mail
    /// addresses are not flagged (they are scrubbed, not refused).
    pub fn flags(&self, text: &str) -> Option<ScrubFlag> {
        self.check(text, crate::token_shape::looks_like_credential)
    }

    /// [`Scrubber::flags`] widened for a writer that refuses rather than
    /// rewrites: also any keyed value of 8 or more characters and bare keys
    /// of 32 to 64 characters ([`crate::token_shape::refuses_to_store`]).
    pub fn refuses(&self, text: &str) -> Option<ScrubFlag> {
        self.check(text, crate::token_shape::refuses_to_store)
    }

    /// [`Scrubber::refuses`] over every string of a JSON value, object keys
    /// included: checked before serialising, since serialising escapes
    /// quotes and backslashes and a configured value written that way no
    /// longer matches.
    pub fn refuses_value(&self, v: &serde_json::Value) -> Option<ScrubFlag> {
        use serde_json::Value;
        match v {
            Value::String(s) => self.refuses(s),
            Value::Array(items) => items.iter().find_map(|i| self.refuses_value(i)),
            Value::Object(map) => map
                .iter()
                .find_map(|(k, v)| self.refuses(k).or_else(|| self.refuses_value(v))),
            _ => None,
        }
    }

    fn check(&self, text: &str, shaped: fn(&str) -> bool) -> Option<ScrubFlag> {
        if self
            .secrets
            .iter()
            .any(|s| text.contains(s.expose_secret()))
        {
            return Some(ScrubFlag::SecretValue);
        }
        shaped(text).then_some(ScrubFlag::TokenShape)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_json_value_is_scrubbed_in_its_keys_as_well_as_its_values() {
        let root = tempfile::tempdir().unwrap();
        let r = Scrubber::new(vec![Secret::new("hunter2-secret-value")], root.path());
        let v = serde_json::json!({
            "outputs": { "hunter2-secret-value": "dev.person@example.org", "plain": ["hunter2-secret-value"] }
        });
        let out = r.redact_value(v).to_string();
        assert!(
            !out.contains("hunter2"),
            "a configured value reached the output"
        );
        assert!(!out.contains("example.org"));
        assert!(out.contains("plain"));
    }

    #[test]
    fn values_tokens_paths_and_emails_are_scrubbed() {
        let root = tempfile::tempdir().unwrap();
        let canon = std::fs::canonicalize(root.path()).unwrap();
        let r = Scrubber::new(
            vec![Secret::new("hunter2-secret-value"), Secret::new("ab")],
            root.path(),
        );
        let text = format!(
            "key hunter2-secret-value, gh ghp_abcdefghijklmnop1234, jwt eyJhbGciOi.eyJzdWIiOiIx.c2lnbmF0dXJl, \
             Authorization: Bearer abc.DEF-ghi_123456, blob Q2hhbmdlZCBhIHRva2VuIHdpdGggMTIzIGFuZCBYWVo=, \
             sha 0123456789abcdef0123456789abcdef01234567, id 123e4567-e89b-12d3-a456-426614174000, \
             file {}/src/main.rs, other /home/someone/notes.txt, mail dev.person@example.org, ab stays",
            canon.display()
        );
        let out = r.scrub(&text);
        for gone in [
            "hunter2-secret-value",
            "ghp_abcdefghijklmnop1234",
            "eyJhbGciOi",
            "abc.DEF-ghi_123456",
            "Q2hhbmdlZCBhIHRva2VuIHdpdGggMTIzIGFuZCBYWVo",
            "dev.person@example.org",
            "someone",
            &canon.display().to_string(),
        ] {
            assert!(!out.contains(gone), "a planted fixture survived the scrub");
        }
        for kept in [
            "0123456789abcdef0123456789abcdef01234567",
            "123e4567-e89b-12d3-a456-426614174000",
            "file src/main.rs",
            "~/notes.txt",
            "[email]",
            "ab stays",
        ] {
            assert!(out.contains(kept), "{kept} missing: {out}");
        }
    }

    #[test]
    fn home_paths_and_token_fields_are_scrubbed_in_every_spelling() {
        let root = tempfile::tempdir().unwrap();
        let r = Scrubber::new(vec![Secret::new("hunter2-secret-value")], root.path());
        let text = r#"u https://api.example.com/users/octo a c:\users\alice\x b C:/Users/bob/x c file:///C:/Users/carol/x d /mnt/c/users/dave/x e C:\\Users\\erin\\x f {"token": "a1b2c3d4e5f6g7h8i9j0"} g access_token=Zz9Yy8Xx7Ww6Vv5 h "api_key":"k1k2k3k4k5k6k7k8""#;
        let out = r.scrub(text);
        for gone in [
            "alice",
            "bob",
            "carol",
            "dave",
            "erin",
            "a1b2c3d4e5f6g7h8i9j0",
            "Zz9Yy8Xx7Ww6Vv5",
            "k1k2k3k4k5k6k7k8",
        ] {
            assert!(!out.contains(gone), "a planted fixture survived the scrub");
        }
        assert!(out.contains(r#""token": "[redacted-token]""#), "{out}");
        assert!(out.contains("access_token=[redacted-token]"), "{out}");
        assert!(out.contains("https://api.example.com/users/octo"), "{out}");
        assert!(!format!("{r:?}").contains("hunter2"));
    }

    #[test]
    fn flags_catches_keyed_values_and_configured_values_but_not_identifiers() {
        let root = tempfile::tempdir().unwrap();
        let configured = format!("cfg-{}", "v4lue".repeat(3));
        let r = Scrubber::new(vec![Secret::new(configured.clone())], root.path());
        assert_eq!(
            r.flags("api_key: abcd1234efgh5678"),
            Some(ScrubFlag::TokenShape)
        );
        assert_eq!(
            r.flags(&format!("the hook uses {configured} to sign")),
            Some(ScrubFlag::SecretValue)
        );
        assert_eq!(
            r.flags("fixed in 0123456789abcdef0123456789abcdef01234567"),
            None
        );
        assert_eq!(r.flags("run 123e4567-e89b-12d3-a456-426614174000"), None);
        assert_eq!(r.flags("Review a source file and write a report"), None);
    }

    /// The shapes the capture check refused before it moved here: any value
    /// of 8 or more characters after a key name (any case), and bare keys
    /// of 32 to 64 characters. Identifiers stay allowed.
    #[test]
    fn refuses_every_shape_the_capture_check_refused() {
        let root = tempfile::tempdir().unwrap();
        let r = Scrubber::new(Vec::new(), root.path());
        for case in [
            r#"{"password": "correcthorsebatterystaple"}"#,
            "secret=abcdefghijk",
            "token: abcdefghij",
            "API_KEY=lowercaseonly",
            "api-key: qwertyuiop",
            "apikey: zxcvbnmasdf",
            "Password: CorrectHorse",
            "abcdefghijklmnopqrstuvwxyz012345",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567ABCD",
            "key k9k8k7k6k5k4k3k2k1k0j9j8j7j6j5j4",
        ] {
            assert_eq!(r.refuses(case), Some(ScrubFlag::TokenShape), "{case}");
        }
        for case in [
            "fixed in 0123456789abcdef0123456789abcdef01234567",
            "digest 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "run 123e4567-e89b-12d3-a456-426614174000",
            "the password: short",
            "known_problems_shown_event_is_journaled",
            "Review a source file and write a report",
        ] {
            assert_eq!(r.refuses(case), None, "{case}");
        }
    }

    /// The narrow check a known problem uses lets prose and identifiers
    /// through: a sighting is never dropped for ordinary words.
    #[test]
    fn flags_lets_prose_and_identifiers_through() {
        let root = tempfile::tempdir().unwrap();
        let r = Scrubber::new(Vec::new(), root.path());
        for case in [
            "the password: authentication rules",
            "error: invalid token: unauthorized",
            "secret: unavailable",
            "apb_engine_known_problems_v2_migration_test",
            "2026-10-27-known-problems-review-notes",
        ] {
            assert_eq!(r.flags(case), None, "{case}");
        }
    }

    #[test]
    fn stored_text_scrubs_whole_then_cuts_and_drops_what_still_flags() {
        let root = tempfile::tempdir().unwrap();
        let configured = format!("cfg-{}", "v4lue".repeat(3));
        let r = Scrubber::new(vec![Secret::new(configured.clone())], root.path());
        // The secret sits across the cut: it is scrubbed before the cut, so
        // no part of it is kept.
        let text = format!("  run with {configured} now\u{200B}\tok\n  ");
        let kept = r.stored_text(text.trim(), 15).unwrap();
        assert_eq!(kept, "run with [redac");
        let whole = r.stored_text(text.trim(), 100).unwrap();
        assert_eq!(whole, "run with [redacted] now?\tok");
        assert_eq!(
            r.stored_text("api_key: abcd1234efgh5678", 100).as_deref(),
            Some("api_key: [redacted-token]")
        );
        assert_eq!(r.stored_text("   ", 10), None);
        assert_eq!(
            r.stored_text("Bearer eyJhbGciOi.eyJzdWIiOiIx", 100)
                .as_deref(),
            Some("Bearer [redacted-token]")
        );
    }

    #[test]
    fn refuses_value_checks_each_string_before_it_is_serialised() {
        let root = tempfile::tempdir().unwrap();
        let configured = r#"q"uo\te-v4lue"#;
        let r = Scrubber::new(vec![Secret::new(configured)], root.path());
        let v = serde_json::json!({ "steps": [format!("call with {configured}")] });
        assert!(
            r.flags(&v.to_string()).is_none(),
            "serialised form escapes it"
        );
        assert_eq!(r.refuses_value(&v), Some(ScrubFlag::SecretValue));
        let keyed = serde_json::json!({ configured: "x" });
        assert_eq!(r.refuses_value(&keyed), Some(ScrubFlag::SecretValue));
        assert_eq!(
            r.refuses_value(&serde_json::json!({ "a": ["plain text"] })),
            None
        );
    }
}
