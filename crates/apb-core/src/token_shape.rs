//! Credential shapes: the one detector of token-like strings.
//!
//! JWTs, well-known key prefixes, Slack webhook paths, the password part of
//! a URL's user info, values written after a key name (`bearer`, `token`,
//! `secret`, `password`, `api_key`, `access_key`, `private_key`, also with a
//! prefix such as `refresh_token`), and long runs that mix letters and
//! digits count as credentials. Git hashes, UUIDs and other bare hex strings
//! do not: they are identifiers (a hex value after a key name still counts).
//! The decision redactor scrubs outgoing text with [`redact_tokens`]; the run
//! memory refuses to store a value for which [`looks_like_credential`] holds,
//! and a value of a param whose name [`name_looks_secret`].

use std::sync::LazyLock;

use regex::{Captures, Regex};

/// What a token-shaped string is replaced with.
pub const TOKEN_MARK: &str = "[redacted-token]";

static JWT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"eyJ[A-Za-z0-9_-]{5,}\.eyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}").expect("valid")
});
static PREFIXED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(?:sk-|sk_|pk_live_|rk_live_|ghp_|gho_|ghu_|ghs_|ghr_|github_pat_|glpat-|xox[abeprs]-|AKIA|ASIA|AIza|hf_|npm_)[A-Za-z0-9_\-]{8,}|hooks\.slack\.com/services/[A-Za-z0-9_/]{20,}",
    )
    .expect("valid")
});
/// A value after a key name, written as `Bearer v`, `token=v`,
/// `password: v` or the JSON `"api_key": "v"`. Group 1 is everything
/// before the value, group 2 the separator, group 3 the value.
static KEYED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(\b(?:bearer|[a-z0-9_]*(?:token|secret|password|passwd|api[_-]?key|access[_-]?key|private[_-]?key))["']?(\s*[=:]\s*|\s+)["']?)([^\s"'`,;<>(){}\[\]&?#]{6,})"#,
    )
    .expect("valid")
});
/// The user info of a URL with a password: group 1 is the scheme and the
/// user, group 2 the password.
static URL_USERINFO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(\b[a-z][a-z0-9+.\-]*://[^\s/:@]+:)([^\s/@]+)@").expect("valid")
});
static LONG_RUN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9_+/=\-]{32,}").expect("valid"));
static UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$")
        .expect("valid")
});

/// The length of an AWS secret access key, which may hold slashes.
const AWS_SECRET_LEN: usize = 40;

/// Whether a long run of token characters looks like a credential: mixed
/// case with digits, and neither a hex hash nor a UUID nor a plain path. A
/// run of one case (a base32 or base36 content hash, a store path) is an
/// identifier; a lowercase key counts once a key name precedes it.
fn looks_like_token(run: &str) -> bool {
    if run.chars().all(|c| c.is_ascii_hexdigit()) || UUID.is_match(run) {
        return false;
    }
    let core = run.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    let has = |f: fn(&char) -> bool| core.chars().any(|c| f(&c));
    let slashes = core.matches('/').count();
    has(char::is_ascii_digit)
        && has(char::is_ascii_uppercase)
        && has(char::is_ascii_lowercase)
        && (slashes <= 1 || core.len() == AWS_SECRET_LEN)
}

/// Whether the value of a [`KEYED`] match is a credential rather than
/// prose. After `=`: 12 or more characters, or 8 or more with something
/// other than letters. After `:` (also prose punctuation, "the password:
/// authentication rules") or a bare space (`Bearer v`), 8 or more
/// characters with a digit or mixed case, so words stay prose.
fn keyed_value(c: &Captures<'_>) -> bool {
    let value = &c[3];
    let has = |f: fn(&char) -> bool| value.chars().any(|ch| f(&ch));
    if c[2].contains('=') {
        value.len() >= 12 || (value.len() >= 8 && has(|ch| !ch.is_ascii_alphabetic()))
    } else {
        value.len() >= 8
            && (has(char::is_ascii_digit)
                || (has(char::is_ascii_uppercase) && has(char::is_ascii_lowercase)))
    }
}

/// Whether `text` contains anything credential-shaped.
pub fn looks_like_credential(text: &str) -> bool {
    JWT.is_match(text)
        || PREFIXED.is_match(text)
        || URL_USERINFO.is_match(text)
        || KEYED.captures_iter(text).any(|c| keyed_value(&c))
        || LONG_RUN
            .find_iter(text)
            .any(|m| looks_like_token(m.as_str()))
}

/// Whether a param named `name` holds a credential by its name: one of its
/// words (split at `_`, `-`, case changes and between letters and digits)
/// is `password(s)`, `passwd`, `passphrase`, `pwd`, `secret(s)`, `apikey`
/// or `credential(s)`, `token(s)` unless the name counts them
/// (`max_tokens`, `token_limit`), or two adjacent words are `api key`,
/// `access key` or `private key`.
pub fn name_looks_secret(name: &str) -> bool {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut prev: Option<char> = None;
    for c in name.chars() {
        let boundary = !c.is_ascii_alphanumeric()
            || prev.is_some_and(|p| {
                (c.is_ascii_uppercase() && p.is_ascii_lowercase())
                    || (c.is_ascii_digit() != p.is_ascii_digit() && p.is_ascii_alphanumeric())
            });
        if boundary && !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        if c.is_ascii_alphanumeric() {
            word.push(c.to_ascii_lowercase());
        }
        prev = Some(c);
    }
    words.push(word);
    let counted = words.iter().any(|w| {
        matches!(
            w.as_str(),
            "max" | "min" | "num" | "count" | "limit" | "total" | "budget"
        )
    });
    let single = |w: &str| match w {
        "password" | "passwords" | "passwd" | "passphrase" | "pwd" | "secret" | "secrets"
        | "apikey" | "credential" | "credentials" => true,
        "token" | "tokens" => !counted,
        _ => false,
    };
    words.iter().any(|w| single(w))
        || words
            .windows(2)
            .any(|p| p[1] == "key" && matches!(p[0].as_str(), "api" | "access" | "private"))
}

/// `text` with every credential-shaped string replaced by [`TOKEN_MARK`].
pub fn redact_tokens(text: &str) -> String {
    let out = JWT.replace_all(text, TOKEN_MARK);
    let out = PREFIXED.replace_all(&out, TOKEN_MARK);
    let out = URL_USERINFO.replace_all(&out, format!("${{1}}{TOKEN_MARK}@"));
    let out = KEYED.replace_all(&out, |c: &Captures<'_>| {
        if keyed_value(c) {
            format!("{}{TOKEN_MARK}", &c[1])
        } else {
            c[0].to_string()
        }
    });
    LONG_RUN
        .replace_all(&out, |c: &Captures<'_>| {
            let run = &c[0];
            if looks_like_token(run) {
                TOKEN_MARK.to_string()
            } else {
                run.to_string()
            }
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_detected_and_identifiers_are_not() {
        for (case, secret) in [
            "ghp_abcdefghijklmnop1234",
            "eyJhbGciOi.eyJzdWIiOiIx.c2lnbmF0dXJl",
            "Authorization: Bearer abc.DEF-ghi_123456",
            "Q2hhbmdlZCBhIHRva2VuIHdpdGggMTIzIGFuZCBYWVo=",
            "aws_secret_access_key=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "api_key: 9f86d081884c7d659a2feaa0c55ad015",
            "password=hunter22",
            "https://deploy:s3cretValue@git.example.com/repo.git",
            "https://hooks.slack.com/services/T0000000/B0000000/XXXXXXXXXXXXXXXXXXXXXXXX",
            "api_key: k3j4h5g6f7d8s9a0q1w2e3r4t5y6u7i8o9p0",
        ]
        .into_iter()
        .enumerate()
        {
            assert!(looks_like_credential(secret), "credential case {case}");
        }
        for plain in [
            "beta",
            "0123456789abcdef0123456789abcdef01234567",
            "123e4567-e89b-12d3-a456-426614174000",
            "Summarize the changes since the last tag",
            "Fix the refresh token implementation",
            "Explain the Bearer authentication flow",
            "Check the password: validation rules",
            "max_tokens: 4000",
            "release-notes-for-the-2026-10-27-build-of-apb",
            "Check the password: authentication rules",
            "api_key: environment-variable",
            "/nix/store/0c5b8vw40d1178xlpha3pl6ywqh3wrxv-hello",
        ] {
            assert!(!looks_like_credential(plain), "{plain}");
        }
    }

    #[test]
    fn a_param_name_can_mark_its_value_secret() {
        for name in [
            "password",
            "db_password",
            "apiKey",
            "API_KEY",
            "access-key",
            "github_token",
            "client_secret",
            "api_tokens",
            "passphrase",
            "OAUTH2TOKEN",
        ] {
            assert!(name_looks_secret(name), "{name}");
        }
        for name in [
            "max_tokens",
            "token_limit",
            "key",
            "branch",
            "secretary_name",
            "keyword",
        ] {
            assert!(!name_looks_secret(name), "{name}");
        }
    }

    #[test]
    fn redaction_keeps_the_text_around_a_token() {
        assert_eq!(
            redact_tokens("token=Zz9Yy8Xx7Ww6Vv5 done"),
            "token=[redacted-token] done"
        );
        assert_eq!(
            redact_tokens("clone https://deploy:s3cretValue@git.example.com/r"),
            "clone https://deploy:[redacted-token]@git.example.com/r"
        );
        assert_eq!(
            redact_tokens("the refresh token implementation"),
            "the refresh token implementation"
        );
        assert_eq!(
            redact_tokens("GET /x?token=abc123def456&page=2"),
            "GET /x?token=[redacted-token]&page=2"
        );
    }
}
