//! Redaction and clipping of text apb hands to someone outside the run: a
//! decision request, a notification, the HTML run report, a journaled probe
//! failure.
//!
//! The redactor is the workspace scrubber of core
//! ([`apb_core::scrub::Scrubber`]), re-exported under its engine name so
//! every engine user keeps its call; this module adds the clip helpers.

pub(crate) use apb_core::scrub::Scrubber as Redactor;

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
