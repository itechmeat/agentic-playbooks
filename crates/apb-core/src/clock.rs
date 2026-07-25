//! Wall-clock readings, in one place. Every timestamp apb persists (event
//! `ts`, trust approvals, project last-seen, cache stamps) and every unique
//! temp-name suffix comes from here, so the epoch convention and the
//! before-epoch fallback are defined once instead of per call site.
//!
//! Milliseconds are `u128` throughout apb, matching the persisted state
//! fields; seconds are `u64`. A clock that reports a time before the Unix
//! epoch yields 0 rather than an error: these values stamp records and name
//! temp files, and neither has a meaningful failure path.

use std::time::{SystemTime, UNIX_EPOCH};

fn since_epoch() -> std::time::Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u128 {
    since_epoch().as_millis()
}

/// Milliseconds since the Unix epoch, narrowed to `u64` for the few state
/// types that cannot carry `u128`: serde_json has no `u128` arm when it
/// buffers through `Content`, which is what an internally-tagged enum does
/// (see [`crate::projects::State`]). A `u64` of milliseconds still spans
/// centuries, so the narrowing is not observable.
pub fn now_ms_u64() -> u64 {
    since_epoch().as_millis() as u64
}

/// Whole seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    since_epoch().as_secs()
}

/// Nanoseconds since the Unix epoch. Used to make temp names unpredictable,
/// never as a persisted timestamp.
pub fn now_nanos() -> u128 {
    since_epoch().as_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_are_consistent_across_units() {
        let ms = now_ms();
        let secs = now_secs();
        let nanos = now_nanos();
        // A plausible epoch reading: after 2020-01-01, before 2100-01-01.
        assert!((1_577_836_800_000..4_102_444_800_000).contains(&ms));
        assert_eq!(secs, (ms / 1000) as u64);
        assert!(nanos / 1_000_000 >= ms - 1);
    }

    #[test]
    fn readings_do_not_go_backwards() {
        let first = now_ms();
        let second = now_ms();
        assert!(second >= first);
    }
}
