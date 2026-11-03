//! The structural rules of one citation:
//!
//! - an outline entry routes a reader and never proves;
//! - a citation names a record that exists (no invented ids), in a view of
//!   a kind the criterion reads;
//! - the record may be cited at all (not one the redactor changed);
//! - the record was read whole, not only seen in a listing or a search;
//! - a span `[from, to)` stays inside the record as the reader showed it.
//!
//! A problem names references and counts only, never source text.

use apb_core::quality::grade::Cite;

use super::views::{ReadState, ViewSet, is_outline_ref};

/// The problem of one citation for a criterion that reads `kinds`, if
/// any.
pub fn cite_problem<K: PartialEq>(
    cite: &Cite,
    kinds: &[K],
    views: &ViewSet<K>,
    state: &ReadState,
) -> Option<String> {
    let r = cite.r#ref.as_str();
    if is_outline_ref(r) {
        return Some(format!(
            "`{r}` is an outline entry: outline entries never prove"
        ));
    }
    let Some(rec) = views.record(r) else {
        return Some(format!("`{r}` names no record of this run"));
    };
    if !views.holding(r).any(|v| kinds.contains(&v.kind)) {
        return Some(format!("`{r}` is outside the views this criterion reads"));
    }
    if !views.citable(r) {
        return Some(format!(
            "`{r}` was served redacted: a secret in it was found after capture, so it cannot be cited"
        ));
    }
    if !state.full.contains(r) {
        return Some(if state.routed.contains(r) {
            format!("`{r}` was only seen in a listing or a search: read it before citing it")
        } else {
            format!("`{r}` was not read in full")
        });
    }
    match cite.span {
        Some([a, b]) if a >= b || b > rec.text.chars().count() as u64 => {
            Some(format!("the span of `{r}` is outside the record"))
        }
        _ => None,
    }
}
