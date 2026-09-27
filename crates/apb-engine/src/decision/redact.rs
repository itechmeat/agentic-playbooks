//! Redaction and clipping of what a decision request sends.
//!
//! Applied to every text field before it leaves the machine, in this order:
//! known secret values (the variables every installed connector references,
//! and the provider keys themselves) become `[redacted]`; absolute paths
//! under the project become repo-relative and other home paths `~/...`;
//! token-shaped strings (JWTs, well-known key prefixes, bearer values, long
//! mixed-case runs with digits) become `[redacted-token]`; e-mail addresses
//! become `[email]`. Git hashes and UUIDs are kept: they are identifiers,
//! not credentials.

use std::sync::LazyLock;

use regex::Regex;

/// Shorter values are not redacted by value: a two-letter secret would blank
/// every occurrence of that pair in the text. Token shapes still apply.
const MIN_SECRET_LEN: usize = 6;

static JWT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"eyJ[A-Za-z0-9_-]{5,}\.eyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}").expect("valid")
});
static PREFIXED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(?:sk-|sk_|pk_live_|rk_live_|ghp_|gho_|ghu_|ghs_|ghr_|github_pat_|glpat-|xox[abprs]-|AKIA|ASIA|AIza|hf_|npm_)[A-Za-z0-9_\-]{8,}",
    )
    .expect("valid")
});
static BEARER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(bearer|token)(\s+|=|:\s*)[A-Za-z0-9._~+/=\-]{12,}").expect("valid")
});
static LONG_RUN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9_+/=\-]{32,}").expect("valid"));
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)*\.[A-Za-z]{2,}")
        .expect("valid")
});
static HOME_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:/home/|/Users/|/mnt/[a-z]/Users/|[A-Za-z]:\\Users\\)[^/\\\s:"'`]+"#)
        .expect("valid")
});
static UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$")
        .expect("valid")
});

/// Whether a long run of token characters looks like a credential: mixed
/// case with digits, and neither a hex hash nor a UUID nor a plain path.
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
        && slashes <= 1
}

/// Redacts text for one run.
#[derive(Debug, Default)]
pub(crate) struct Redactor {
    /// Secret values, longest first so a value containing another is
    /// replaced whole.
    secrets: Vec<String>,
    /// The project root, as written in paths (no trailing slash).
    root: Option<String>,
    home: Option<String>,
}

impl Redactor {
    pub(crate) fn new(mut secrets: Vec<String>, root: &std::path::Path) -> Self {
        secrets.retain(|s| s.len() >= MIN_SECRET_LEN);
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        let root = std::fs::canonicalize(root)
            .unwrap_or_else(|_| root.to_path_buf())
            .to_string_lossy()
            .trim_end_matches('/')
            .to_string();
        let home = std::env::var("HOME")
            .ok()
            .map(|h| h.trim_end_matches('/').to_string())
            .filter(|h| h.len() > 1);
        Redactor {
            secrets,
            root: (root.len() > 1).then_some(root),
            home,
        }
    }

    pub(crate) fn redact(&self, text: &str) -> String {
        let mut out = text.to_string();
        for s in &self.secrets {
            if out.contains(s.as_str()) {
                out = out.replace(s.as_str(), "[redacted]");
            }
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
        out = JWT.replace_all(&out, "[redacted-token]").into_owned();
        out = PREFIXED.replace_all(&out, "[redacted-token]").into_owned();
        out = BEARER
            .replace_all(&out, |c: &regex::Captures| {
                format!("{} [redacted-token]", &c[1])
            })
            .into_owned();
        out = LONG_RUN
            .replace_all(&out, |c: &regex::Captures| {
                let run = &c[0];
                if looks_like_token(run) {
                    "[redacted-token]".to_string()
                } else {
                    run.to_string()
                }
            })
            .into_owned();
        EMAIL.replace_all(&out, "[email]").into_owned()
    }
}

/// Byte index at or below `i` that is a char boundary.
fn floor_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Byte index at or above `i` that is a char boundary.
fn ceil_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// `text` cut to its first `head` and last `tail` bytes (on char
/// boundaries), with the cut marked. Unchanged when it already fits.
pub(crate) fn clip(text: &str, head: usize, tail: usize) -> String {
    if text.len() <= head + tail {
        return text.to_string();
    }
    let h = floor_boundary(text, head);
    let t = ceil_boundary(text, text.len() - tail);
    let cut = t - h;
    let mut out = String::with_capacity(h + (text.len() - t) + 32);
    out.push_str(&text[..h]);
    if tail == 0 {
        out.push_str(&format!("\n[... {cut} bytes cut]"));
    } else {
        out.push_str(&format!("\n[... {cut} bytes cut ...]\n"));
    }
    out.push_str(&text[t..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_tokens_paths_and_emails_are_redacted() {
        let root = tempfile::tempdir().unwrap();
        let canon = std::fs::canonicalize(root.path()).unwrap();
        let r = Redactor::new(
            vec!["hunter2-secret-value".into(), "ab".into()],
            root.path(),
        );
        let text = format!(
            "key hunter2-secret-value, gh ghp_abcdefghijklmnop1234, jwt eyJhbGciOi.eyJzdWIiOiIx.c2lnbmF0dXJl, \
             Authorization: Bearer abc.DEF-ghi_123456, blob Q2hhbmdlZCBhIHRva2VuIHdpdGggMTIzIGFuZCBYWVo=, \
             sha 0123456789abcdef0123456789abcdef01234567, id 123e4567-e89b-12d3-a456-426614174000, \
             file {}/src/main.rs, other /home/someone/notes.txt, mail dev.person@example.org, ab stays",
            canon.display()
        );
        let out = r.redact(&text);
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
            assert!(!out.contains(gone), "{gone} survived: {out}");
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
    fn clipping_keeps_head_and_tail_on_char_boundaries() {
        assert_eq!(clip("short", 4, 4), "short");
        let text = format!("{}{}{}", "a".repeat(10), "é".repeat(50), "z".repeat(10));
        let out = clip(&text, 11, 11);
        assert!(out.starts_with("aaaaaaaaaa"));
        assert!(out.ends_with("zzzzzzzzzz"));
        assert!(out.contains("bytes cut"));
        let head_only = clip(&"x".repeat(100), 10, 0);
        assert!(head_only.starts_with(&"x".repeat(10)));
        assert!(head_only.len() < 40);
    }
}
