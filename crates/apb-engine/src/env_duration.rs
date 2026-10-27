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
