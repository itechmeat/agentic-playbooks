//! Durations the engine reads from the environment: tunables that tests
//! shorten (grace periods, drain budgets). One parser, so every such knob
//! accepts the same spelling: a whole number of milliseconds, surrounding
//! whitespace allowed.

use std::time::Duration;

/// The duration in milliseconds named by env var `key`, `None` when it is
/// unset or not a whole number.
pub(crate) fn millis(key: &str) -> Option<Duration> {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
}

/// Whether this build honours the test-only knobs of [`test_override`]:
/// debug builds only (every test binary, and the `apb` the CLI tests run),
/// never a release build, the one apb ships.
pub(crate) const TEST_KNOBS: bool = cfg!(debug_assertions);

// A release build that honoured the knobs does not compile: `apb` ships
// as a release build (`[profile.dist]` inherits `release`).
const _: () = assert!(
    cfg!(debug_assertions) || !TEST_KNOBS,
    "a release build never honours the test-only env knobs"
);

/// A test-only shortening of a wait the product fixes: the milliseconds in
/// `raw` (the env var's value) in a build that honours [`TEST_KNOBS`], else
/// `None`, so a value left exported in a shell never changes how a shipped
/// binary runs.
pub(crate) fn test_override(raw: Option<&str>) -> Option<Duration> {
    if !TEST_KNOBS {
        return None;
    }
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Built in either profile: a debug build takes a whole number of
    /// milliseconds and ignores anything else; a release build (`cargo
    /// test --release`) ignores every value.
    #[test]
    fn a_test_knob_is_honoured_by_a_debug_build_only() {
        let honoured = |ms| cfg!(debug_assertions).then(|| Duration::from_millis(ms));
        assert_eq!(test_override(Some("0")), honoured(0));
        assert_eq!(test_override(Some(" 300 ")), honoured(300));
        assert_eq!(test_override(Some("soon")), None);
        assert_eq!(test_override(None), None);
    }
}
