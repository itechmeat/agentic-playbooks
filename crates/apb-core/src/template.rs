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

/// Whether `text` places recorded run context itself: it reads
/// `{{run.context}}` or ANY `{{nodes.<id>.<field>}}` reference (spec
/// 2026-08-05 section 2.7).
///
/// Used by the engine's finish composer to decide whether the terminal
/// context has to be appended for it. Every `nodes.*` field counts, not
/// just `output`/`report`: a template that deliberately quotes one node's
/// review note or rejected output has chosen what the composer should
/// see, and appending the whole run on top of that would override the
/// author's choice.
pub fn reads_recorded_context(text: &str) -> bool {
    refs(text).into_iter().any(|key| {
        matches!(
            key.split('.').collect::<Vec<&str>>().as_slice(),
            ["run", "context"] | ["nodes", _, ..]
        )
    })
}
