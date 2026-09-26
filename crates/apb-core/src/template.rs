//! The playbook template syntax as the static side reads it: every
//! `{{ ... }}` reference in a text. One scanner for the validator and for the
//! schema helpers that need to know what a template reads (the run
//! `worktree`), kept out of `validate` so `schema` can use it without a
//! dependency cycle.

/// Every `{{ ... }}` reference in `text`, trimmed, in order of appearance.
pub fn refs(text: &str) -> Vec<String> {
    // no regex dependency: manual scan for {{ ... }}
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if &bytes[i..i + 2] == b"{{"
            && let Some(end) = text[i + 2..].find("}}")
        {
            out.push(text[i + 2..i + 2 + end].trim().to_string());
            i += 2 + end + 2;
            continue;
        }
        i += 1;
    }
    out
}
