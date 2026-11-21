//! The hidden send marker: a short, invisible tag apb appends to the text of
//! a send whose connector function declares `idempotency: { mode: marker }`,
//! so that a later attempt can find the item the earlier one created and
//! tell "it went through" from "it never left".
//!
//! The functions are pure and shared by the engine's call path and the
//! offline contract runner, so a contract test renders exactly the marker a
//! live send carries.
//!
//! Matching is byte-exact. Every write apb sends has the marker keyword
//! neutralized in its agent-written text first ([`neutralize_value`]), so
//! nothing sent through apb can carry a forged marker; the marker apb itself
//! appends afterwards is the only one in the text.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The keyword every marker starts with. Matched ASCII-case-insensitively by
/// [`neutralize`].
const KEYWORD: &str = "apb-intent";

/// What [`neutralize`] rewrites the keyword to: still readable, never a
/// marker.
const NEUTRALIZED: &str = "apb-quoted-intent";

/// How a marker is written into the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerFormat {
    /// An HTML comment, invisible wherever the text renders as Markdown or
    /// HTML (GitHub issue and comment bodies).
    HtmlComment,
}

/// The fields one marker carries: which run, node and attempt made the send,
/// and the nonce that makes it unique.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerFields {
    pub run: String,
    pub node: String,
    pub attempt: Option<u32>,
    pub nonce: String,
}

/// Renders the marker. Run ids and node ids are slugs, so they can never
/// close the comment early.
pub fn render(format: MarkerFormat, fields: &MarkerFields) -> String {
    let attempt = fields
        .attempt
        .map_or_else(|| "-".to_string(), |n| n.to_string());
    match format {
        MarkerFormat::HtmlComment => format!(
            "<!-- {KEYWORD} v1 run={} node={} attempt={attempt} nonce={} -->",
            fields.run, fields.node, fields.nonce
        ),
    }
}

/// Appends `marker` to `text`: trailing whitespace trimmed, a blank line, the
/// marker. An absent or empty text becomes the marker alone.
pub fn append(text: Option<&str>, marker: &str) -> String {
    match text.map(str::trim_end) {
        Some(body) if !body.is_empty() => format!("{body}\n\n{marker}"),
        _ => marker.to_string(),
    }
}

/// Rewrites every ASCII-case-insensitive occurrence of the marker keyword to
/// its neutral form. Borrows when there is none.
pub fn neutralize(text: &str) -> Cow<'_, str> {
    let lower = text.to_ascii_lowercase();
    if !lower.contains(KEYWORD) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut rest = 0;
    // ASCII lowering keeps byte offsets, so a hit in `lower` is a hit at the
    // same offsets in `text`.
    for (at, _) in lower.match_indices(KEYWORD) {
        out.push_str(&text[rest..at]);
        out.push_str(NEUTRALIZED);
        rest = at + KEYWORD.len();
    }
    out.push_str(&text[rest..]);
    Cow::Owned(out)
}

/// [`neutralize`] over every string leaf of a JSON value (object keys are
/// left alone: they are field names, not text). Borrows when nothing
/// changes.
pub fn neutralize_value(value: &Value) -> Cow<'_, Value> {
    match value {
        Value::String(s) => match neutralize(s) {
            Cow::Borrowed(_) => Cow::Borrowed(value),
            Cow::Owned(s) => Cow::Owned(Value::String(s)),
        },
        Value::Array(items) => {
            let mapped: Vec<Cow<'_, Value>> = items.iter().map(neutralize_value).collect();
            if mapped.iter().all(|v| matches!(v, Cow::Borrowed(_))) {
                return Cow::Borrowed(value);
            }
            Cow::Owned(Value::Array(
                mapped.into_iter().map(Cow::into_owned).collect(),
            ))
        }
        Value::Object(map) => {
            let mapped: Vec<(&String, Cow<'_, Value>)> =
                map.iter().map(|(k, v)| (k, neutralize_value(v))).collect();
            if mapped.iter().all(|(_, v)| matches!(v, Cow::Borrowed(_))) {
                return Cow::Borrowed(value);
            }
            Cow::Owned(Value::Object(
                mapped
                    .into_iter()
                    .map(|(k, v)| (k.clone(), v.into_owned()))
                    .collect(),
            ))
        }
        _ => Cow::Borrowed(value),
    }
}

/// Whether `text` carries exactly `marker`: it ends with the marker (trailing
/// whitespace ignored) and the marker occurs nowhere else.
pub fn carries_exactly(text: &str, marker: &str) -> bool {
    !marker.is_empty() && text.trim_end().ends_with(marker) && text.matches(marker).count() == 1
}

/// The fixed frame [`render`] writes around the four fields. Kept beside
/// [`render`], and pinned to it by the round-trip cases of
/// `trailing_marker_reads_only_the_exact_trailing_marker` below: a change to
/// the rendered form must change these with it.
const FRAME_PREFIX: &str = "<!-- apb-intent v1 run=";
const FRAME_NODE: &str = " node=";
const FRAME_ATTEMPT: &str = " attempt=";
const FRAME_NONCE: &str = " nonce=";
const FRAME_SUFFIX: &str = " -->";

/// The inverse of [`render`]: the fields of the well-formed marker `text`
/// ends with, or `None`. Well-formed means the exact bytes [`render`] writes
/// (trailing whitespace ignored) and the rule [`carries_exactly`] applies:
/// that one marker occurs nowhere else in the text. apb neutralizes the
/// keyword in every text it stamps before it appends its one marker, so a
/// second marker of any fields anywhere in the text is a quoted or tampered
/// text, never a send of apb's own, and reads as `None` too.
pub fn trailing_marker(text: &str) -> Option<MarkerFields> {
    let body = text.trim_end();
    // One keyword occurrence in the whole text: the marker's own. Every
    // frame holds the keyword, so this makes the frame below unique before
    // it is looked for.
    if body.matches(KEYWORD).count() != 1 {
        return None;
    }
    let rest = body.strip_suffix(FRAME_SUFFIX)?;
    let at = rest.find(FRAME_PREFIX)?;
    let rest = &rest[at + FRAME_PREFIX.len()..];
    let (run, rest) = rest.split_once(FRAME_NODE)?;
    let (node, rest) = rest.split_once(FRAME_ATTEMPT)?;
    let (attempt, nonce) = rest.split_once(FRAME_NONCE)?;
    let attempt = match attempt {
        "-" => None,
        digits => Some(digits.parse::<u32>().ok()?),
    };
    let fields = MarkerFields {
        run: run.to_string(),
        node: node.to_string(),
        attempt,
        nonce: nonce.to_string(),
    };
    // The parse must render back to the exact trailing bytes (a non-canonical
    // number such as `attempt=02` reads as None), ending the text.
    let rendered = render(MarkerFormat::HtmlComment, &fields);
    carries_exactly(text, &rendered).then_some(fields)
}

/// A fresh nonce: 24 lowercase base32 characters from the first 120 bits of
/// a random (v4) uuid. Six of those bits are the uuid's fixed version and
/// variant, so a nonce carries 114 random bits, ample for telling sends
/// apart. It fits the shortest nonce limit apb meets (25 characters).
pub fn new_nonce() -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let mut bits: u128 = u128::from_be_bytes(bytes) >> 8;
    let mut out = [0u8; 24];
    for slot in out.iter_mut().rev() {
        *slot = ALPHABET[(bits & 31) as usize];
        bits >>= 5;
    }
    out.iter().map(|&b| b as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fields() -> MarkerFields {
        MarkerFields {
            run: "r-1".into(),
            node: "triage".into(),
            attempt: Some(2),
            nonce: "abc".into(),
        }
    }

    #[test]
    fn renders_the_v1_html_comment() {
        assert_eq!(
            render(MarkerFormat::HtmlComment, &fields()),
            "<!-- apb-intent v1 run=r-1 node=triage attempt=2 nonce=abc -->"
        );
        let mut script = fields();
        script.attempt = None;
        assert!(render(MarkerFormat::HtmlComment, &script).contains("attempt=- "));
    }

    #[test]
    fn append_trims_and_handles_an_absent_text() {
        assert_eq!(append(Some("hi  \n"), "M"), "hi\n\nM");
        assert_eq!(append(Some("  "), "M"), "M");
        assert_eq!(append(None, "M"), "M");
    }

    #[test]
    fn neutralize_rewrites_any_case_and_keeps_the_rest() {
        assert_eq!(neutralize("plain"), Cow::Borrowed("plain"));
        assert_eq!(
            neutralize("a <!-- APB-Intent v1 --> b apb-intent"),
            "a <!-- apb-quoted-intent v1 --> b apb-quoted-intent"
        );
    }

    #[test]
    fn neutralize_value_reaches_nested_string_leaves_only() {
        let v = json!({ "apb-intent": ["x apb-intent", { "k": "apb-intent" }], "n": 1 });
        let out = neutralize_value(&v).into_owned();
        assert_eq!(
            out,
            json!({ "apb-intent": ["x apb-quoted-intent", { "k": "apb-quoted-intent" }], "n": 1 })
        );
        let clean = json!({ "a": ["b"] });
        assert!(matches!(neutralize_value(&clean), Cow::Borrowed(_)));
    }

    #[test]
    fn carries_exactly_wants_one_trailing_marker() {
        let m = render(MarkerFormat::HtmlComment, &fields());
        assert!(carries_exactly(&append(Some("body"), &m), &m));
        assert!(carries_exactly(&format!("{m}\n"), &m));
        assert!(!carries_exactly(&format!("{m} then more"), &m));
        assert!(!carries_exactly(&format!("{m}\n\n{m}"), &m));
        assert!(!carries_exactly("body", &m));
    }

    /// `trailing_marker` reads only a text that ends with one well-formed
    /// marker: the exact bytes `render` wrote, trailing whitespace ignored,
    /// and that one occurrence in the whole text.
    #[test]
    fn trailing_marker_reads_only_the_exact_trailing_marker() {
        let fields = fields();
        let m = render(MarkerFormat::HtmlComment, &fields);

        // (a) The exact bytes render wrote parse back to the same fields,
        // with and without an attempt, and on a stamped body.
        assert_eq!(trailing_marker(&m), Some(fields.clone()));
        let mut quiet = fields.clone();
        quiet.attempt = None;
        assert_eq!(
            trailing_marker(&render(MarkerFormat::HtmlComment, &quiet)),
            Some(quiet)
        );
        assert_eq!(
            trailing_marker(&append(Some("Summary."), &m)),
            Some(fields.clone())
        );

        // (b) Trailing whitespace is ignored.
        assert_eq!(trailing_marker(&format!("{m}\n\n  ")), Some(fields.clone()));

        // (c) A marker earlier in the text but not at the end is not read.
        assert_eq!(trailing_marker(&format!("{m}\nand more text")), None);

        // (d) A marker mid-text plus other trailing text is not read.
        assert_eq!(trailing_marker(&format!("head {m} tail")), None);

        // (e) Two markers with one at the end: exactly one occurrence.
        assert_eq!(trailing_marker(&format!("{m}\n\n{m}")), None);
        let other = MarkerFields {
            nonce: "zzz".into(),
            ..fields.clone()
        };
        assert_eq!(
            trailing_marker(&format!(
                "{}\n{m}",
                render(MarkerFormat::HtmlComment, &other)
            )),
            None
        );

        // (f) A malformed or tampered marker at the end is not read.
        assert_eq!(trailing_marker(&m.replace("-->", "--&gt;")), None);
        assert_eq!(trailing_marker(&neutralize(&m)), None);
        assert_eq!(
            trailing_marker("<!-- apb-intent v1 run=r node=n attempt=x nonce=abc -->"),
            None
        );
        assert_eq!(trailing_marker(&m.replace("attempt=2", "attempt=02")), None);

        // (g) No marker at all.
        assert_eq!(trailing_marker("plain text"), None);
        assert_eq!(trailing_marker(""), None);
    }

    #[test]
    fn nonces_are_24_base32_characters_and_differ() {
        let a = new_nonce();
        let b = new_nonce();
        assert_eq!(a.len(), 24);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_lowercase() || ('2'..='7').contains(&c))
        );
        assert_ne!(a, b);
    }
}
