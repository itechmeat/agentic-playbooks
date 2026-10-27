//! The `--now` pause interrupt (issue #200, design 3.2 and 3.3).
//!
//! A `--now` pause kills the attempts in flight, like a stop, but it is not
//! a stop: the run-level `cancel` flag stays Abort-only (see `stop.rs`). The
//! stop watcher fires a separate pause fanout instead, which latches the
//! per-node kill flags it knows and marks each of them graceful here. A kill
//! site that sees its flag set asks [`is_graceful`]: a graceful kill sends
//! SIGTERM to the process group, waits [`grace`], then SIGKILLs it, and a
//! host task is left open for the resume to adopt.
//!
//! The registry is keyed by the flag's address. A flag is marked while it is
//! registered with the pause fanout and unmarked when its registration ends,
//! so a later flag allocated at the same address never inherits the mark.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// Milliseconds a `--now` pause waits between SIGTERM and SIGKILL. Tests
/// shorten it.
pub const GRACE_ENV: &str = "APB_PAUSE_GRACE_MS";

/// The default grace period of a `--now` pause.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(10);

static GRACEFUL: Mutex<BTreeSet<usize>> = Mutex::new(BTreeSet::new());

fn key(flag: &AtomicBool) -> usize {
    std::ptr::from_ref(flag) as usize
}

/// Marks `flag` as one a `--now` pause latched.
pub(crate) fn mark_graceful(flag: &AtomicBool) {
    if let Ok(mut set) = GRACEFUL.lock() {
        set.insert(key(flag));
    }
}

/// Ends the mark of `flag` (its pause-fanout registration ended).
pub(crate) fn unmark_graceful(flag: &AtomicBool) {
    if let Ok(mut set) = GRACEFUL.lock() {
        set.remove(&key(flag));
    }
}

/// Whether `flag` was latched by a `--now` pause: its kill is graceful.
pub(crate) fn is_graceful(flag: &AtomicBool) -> bool {
    GRACEFUL.lock().is_ok_and(|set| set.contains(&key(flag)))
}

/// The grace period between SIGTERM and SIGKILL: [`GRACE_ENV`] when set to a
/// number of milliseconds, else [`DEFAULT_GRACE`].
pub(crate) fn grace() -> Duration {
    crate::env_duration::millis(GRACE_ENV).unwrap_or(DEFAULT_GRACE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_is_graceful_only_while_marked() {
        let flag = AtomicBool::new(false);
        let other = AtomicBool::new(false);
        assert!(!is_graceful(&flag));
        mark_graceful(&flag);
        assert!(is_graceful(&flag));
        assert!(!is_graceful(&other));
        unmark_graceful(&flag);
        assert!(!is_graceful(&flag));
    }
}
