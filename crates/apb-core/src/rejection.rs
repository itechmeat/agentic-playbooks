//! The stall fingerprint of a rejection reason (spec node-contracts D5).
//!
//! Every non-fatal rejection records `attempt_finished.rejection_fingerprint`:
//! the normalized reason's SHA-256, first 16 hex characters. The
//! normalization replaces what changes between otherwise identical
//! rejections (the run directory and id, timestamps, UUIDs, hex ids, digit
//! runs) and collapses whitespace, so the same rejection normalizes to the
//! same text and a different one does not.

use regex::Regex;
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

static TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(:\d{2}(\.\d+)?)?(Z|[+-]\d{2}:?\d{2})?").unwrap()
});
static UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
        .unwrap()
});
// A maximal run of 7 or more hex characters bounded by non-word characters
// that holds at least one digit (`deadbee` stays, `abc123de` goes).
static HEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[0-9a-fA-F]{7,}\b").unwrap());
static DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+").unwrap());
static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

/// The normalized text (public for tests and for later grouping).
pub fn normalize(reason: &str, run_id: &str, run_dir: &str) -> String {
    let mut out = reason.to_string();
    if !run_dir.is_empty() {
        out = out.replace(run_dir, "<run>");
    }
    if !run_id.is_empty() {
        out = out.replace(run_id, "<run>");
    }
    let out = TIMESTAMP.replace_all(&out, "<time>").into_owned();
    let out = UUID.replace_all(&out, "<id>").into_owned();
    // A hex run only counts while it holds at least one digit: check each
    // candidate rather than build one regex for both conditions.
    let mut no_hex = String::with_capacity(out.len());
    let mut last = 0;
    for m in HEX.find_iter(&out) {
        if m.as_str().chars().any(|c| c.is_ascii_digit()) {
            no_hex.push_str(&out[last..m.start()]);
            no_hex.push_str("<hex>");
            last = m.end();
        }
    }
    no_hex.push_str(&out[last..]);
    let out = DIGITS.replace_all(&no_hex, "#").into_owned();
    let out = WHITESPACE.replace_all(&out, " ").into_owned();
    out.trim().to_string()
}

/// The stall fingerprint of a rejection reason: the normalized text's
/// SHA-256, first 16 hex characters.
pub fn fingerprint(reason: &str, run_id: &str, run_dir: &str) -> String {
    let hash = Sha256::digest(normalize(reason, run_id, run_dir).as_bytes());
    hash.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
        .chars()
        .take(16)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_normalization_step_alone() {
        // Timestamps; a digit count outside them normalizes too.
        assert_eq!(
            normalize("3 failed at 2026-11-06T10:00:00Z", "", ""),
            "# failed at <time>"
        );
        assert_eq!(
            normalize("at 2026-11-06 10:00:12.5+02:00", "", ""),
            "at <time>"
        );
        // UUIDs.
        assert_eq!(
            normalize("run 123e4567-e89b-12d3-a456-426614174000 broke", "", ""),
            "run <id> broke"
        );
        // Hex ids with a digit.
        assert_eq!(normalize("rev ab12cd34", "", ""), "rev <hex>");
        // Remaining digit runs.
        assert_eq!(normalize("4 of 12 tests", "", ""), "# of # tests");
        // Whitespace.
        assert_eq!(normalize("  a \t b\n", "", ""), "a b");
    }

    #[test]
    fn the_spec_scenario_two_counts_two_timestamps_one_fingerprint() {
        let a = fingerprint("3 tests failed at 2026-11-06T10:00:00Z", "", "");
        let b = fingerprint("5 tests failed at 2026-11-06T10:05:12Z", "", "");
        assert_eq!(a, b);
    }

    #[test]
    fn a_run_dir_and_a_run_id_are_replaced() {
        let n = normalize(
            "failed in /tmp/runs/r1 and run 9f2c7a1d",
            "9f2c7a1d",
            "/tmp/runs/r1",
        );
        assert_eq!(n, "failed in <run> and run <run>");
    }

    #[test]
    fn a_word_of_seven_hex_letters_without_a_digit_is_kept() {
        assert_eq!(normalize("deadbee found", "", ""), "deadbee found");
    }

    #[test]
    fn an_eight_character_hex_id_with_a_digit_is_replaced() {
        assert_eq!(normalize("rev abc123de", "", ""), "rev <hex>");
    }

    #[test]
    fn the_fingerprint_is_16_lowercase_hex_characters_and_stable() {
        let f = fingerprint("success_check `scripts/x.sh` failed", "rid", "/rd");
        assert_eq!(f.len(), 16);
        assert!(
            f.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_eq!(
            f,
            fingerprint("success_check `scripts/x.sh` failed", "rid", "/rd")
        );
    }
}
