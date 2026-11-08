//! The grouping key of a correction: which sightings of a playbook are the
//! same correction, told by what was said rather than by where.
//!
//! A token key (`t:`) hashes the correction's words without their order,
//! ids and counts, so two wordings of one request land together while two
//! different requests stay apart. A fingerprint key (`f:`) carries the
//! engine's normalized hash of a rejection as it is. The two prefixes keep
//! the key spaces apart: the same rejection seen in an older run (no
//! fingerprint) and a newer one may form two entries, which is accepted.
//!
//! The key is only ever computed over text the engine stored (already
//! scrubbed and not flagged), so no secret value and nothing derived from
//! one reaches it.

use sha2::{Digest, Sha256};

use super::CorrectionKind;

/// The distinct tokens a key is built from (the first in text order).
const TOKENS_KEPT: usize = 24;

/// The words of `text` a token key is built from: lowercased, split on
/// every character that is not alphanumeric (Unicode, so non-English text
/// works), without tokens that hold an ASCII digit (ids, counts, line
/// numbers, hashes) or are a single character, the first
/// [`TOKENS_KEPT`] distinct in text order, sorted.
pub fn tokens(text: &str) -> Vec<String> {
    let mut distinct: Vec<String> = Vec::new();
    for word in text.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
        if word.chars().count() <= 1 || word.chars().any(|c| c.is_ascii_digit()) {
            continue;
        }
        if !distinct.iter().any(|t| t == word) {
            distinct.push(word.to_string());
            if distinct.len() >= TOKENS_KEPT {
                break;
            }
        }
    }
    distinct.sort();
    distinct
}

/// The token key of a correction: `t:` and the first 16 hex of
/// `sha256("<kind>|<node or empty>|<tokens joined by one space>")`. With
/// fewer than two tokens the joined text is the whole trimmed text instead,
/// so a one-word correction groups only with itself. `node` is the node a
/// draft fix would change (a correction of another node is another key).
pub fn key_of(kind: CorrectionKind, node: Option<&str>, text: &str) -> String {
    let toks = tokens(text);
    let joined = if toks.len() >= 2 {
        toks.join(" ")
    } else {
        text.trim().to_string()
    };
    let payload = format!("{}|{}|{}", kind.as_word(), node.unwrap_or(""), joined);
    let digest = Sha256::digest(payload.as_bytes());
    format!("t:{}", &crate::content::hex_lower(&digest)[..16])
}

/// The fingerprint key of an engine hash: `f:<fingerprint>` when the
/// fingerprint is 8 to 128 characters of `[0-9a-f]`, else `None`.
pub fn fingerprint_key(fingerprint: &str) -> Option<String> {
    let shaped = (8..=128).contains(&fingerprint.len())
        && fingerprint
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    shaped.then(|| format!("f:{fingerprint}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_parser_sentences_share_a_key() {
        // Word order and punctuation do not matter, the words do: only
        // digit-holding and single-character tokens are dropped, so a
        // wording with one more word (`for`) is another key.
        let a = key_of(
            CorrectionKind::ReviewNote,
            None,
            "please add the parser tests",
        );
        let b = key_of(
            CorrectionKind::ReviewNote,
            None,
            "add the parser tests, please",
        );
        assert_eq!(a, b);
        assert_ne!(
            a,
            key_of(
                CorrectionKind::ReviewNote,
                None,
                "Please add tests for the parser"
            )
        );
        let hex = a.strip_prefix("t:").expect("a token key");
        assert_eq!(hex.len(), 16);
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );
    }

    #[test]
    fn line_numbers_do_not_change_the_key() {
        assert_eq!(
            key_of(CorrectionKind::Rejection, None, "fix line 12"),
            key_of(CorrectionKind::Rejection, None, "fix line 40")
        );
    }

    #[test]
    fn a_one_word_text_keys_on_its_whole_text() {
        assert_eq!(
            key_of(CorrectionKind::ReviewNote, None, "reshape"),
            key_of(CorrectionKind::ReviewNote, None, "  reshape  ")
        );
        assert_ne!(
            key_of(CorrectionKind::ReviewNote, None, "reshape"),
            key_of(CorrectionKind::ReviewNote, None, "renamed")
        );
    }

    #[test]
    fn kind_and_node_are_part_of_the_key() {
        let k = |kind: CorrectionKind, node: Option<&str>| key_of(kind, node, "word one two");
        assert_ne!(
            k(CorrectionKind::ReviewNote, None),
            k(CorrectionKind::NodeNote, None)
        );
        assert_ne!(
            k(CorrectionKind::ReviewNote, Some("a")),
            k(CorrectionKind::ReviewNote, Some("b"))
        );
        assert_ne!(
            k(CorrectionKind::ReviewNote, None),
            k(CorrectionKind::ReviewNote, Some("a"))
        );
    }

    #[test]
    fn non_ascii_words_are_tokens() {
        assert_eq!(tokens("café naïve"), ["café", "naïve"]);
        assert_eq!(tokens("привет мир"), ["мир", "привет"]);
        assert_eq!(
            key_of(CorrectionKind::HumanAnswer, None, "привет мир"),
            key_of(CorrectionKind::HumanAnswer, None, "мир привет")
        );
    }

    #[test]
    fn tokens_drop_digits_single_chars_and_duplicates_and_are_capped() {
        // `the2` and `b3` hold a digit, `a` is one character, `the` repeats.
        assert_eq!(
            tokens("The THE a b3 four the2 five"),
            ["five", "four", "the"]
        );
        assert!(tokens("").is_empty());
        assert!(tokens("... ,,,").is_empty());
        let many: String = (b'a'..=b'z')
            .map(|c| format!("w{}", c as char))
            .collect::<Vec<_>>()
            .join(" ");
        let toks = tokens(&many);
        assert_eq!(
            toks.len(),
            24,
            "the first 24 distinct in text order, sorted"
        );
        assert!(!toks.iter().any(|t| t == "wy" || t == "wz"));
    }

    #[test]
    fn fingerprint_keys_accept_lowercase_hex_of_the_right_length() {
        let fp = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(fingerprint_key(fp), Some(format!("f:{fp}")));
        assert_eq!(fingerprint_key("12345678"), Some("f:12345678".into()));
        assert!(fingerprint_key(&"a".repeat(128)).is_some());
        assert_eq!(fingerprint_key("1234567"), None, "7 characters");
        assert_eq!(fingerprint_key(&"a".repeat(129)), None, "129 characters");
        assert_eq!(fingerprint_key("ABCDEF01"), None, "uppercase");
        assert_eq!(fingerprint_key("1234567g"), None, "not hex");
        assert_eq!(fingerprint_key(""), None);
    }
}
