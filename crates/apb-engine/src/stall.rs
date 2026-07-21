//! Attempt stall detection (spec 2026-07-21 run-reliability).
//!
//! A node that declares `expected_duration` and then runs far past it is either
//! genuinely slow or wedged, and before this a wedge sat silent: one incident
//! burned 90 minutes at 0 CPU with no `timeout` set and nothing in the log to
//! show for it. The adapter's per-attempt poll loop (the same loop that enforces
//! cancellation and `timeout`) drives a [`StallWatch`], which raises a one-shot
//! anomaly the first time the attempt's active elapsed crosses the threshold.
//! It is a SIGNAL, not an enforcement mechanism: the kill clock stays `timeout`.

use std::time::Duration;

/// Grace floor added to a node's estimate before an attempt is called stalled,
/// so a tiny estimate still gets a generous window rather than a 2x that would
/// fire almost immediately.
const STALL_GRACE_SECS: u64 = 600;

/// The `SupervisorAction` `action` recorded for a stall anomaly, and the marker
/// `node_times`/`run_status` reads back to flag a past-estimate open attempt.
pub(crate) const STALL_ACTION: &str = "attempt_stalled";

/// Wall time an attempt may run before it is anomalously slow, given its node's
/// `expected_duration`: `max(2 * expected, expected + 10min)`. The floor keeps a
/// tiny estimate from firing almost at once; the 2x keeps a large estimate from
/// waiting an extra flat 10 minutes on top of an already long doubling.
pub(crate) fn stall_threshold(expected: Duration) -> Duration {
    std::cmp::max(
        expected.saturating_mul(2),
        expected.saturating_add(Duration::from_secs(STALL_GRACE_SECS)),
    )
}

/// One attempt's stall watch. Constructed once per attempt; [`tick`] is called
/// on each poll with the attempt's active (non-pending) elapsed. It raises
/// `on_stall` EXACTLY once - the first tick at or past the threshold - and is a
/// no-op forever after, and a no-op throughout when the node declared no
/// `expected_duration` (`expected` is `None`).
///
/// [`tick`]: StallWatch::tick
pub(crate) struct StallWatch<'a> {
    threshold: Option<Duration>,
    on_stall: Option<&'a dyn Fn(Duration)>,
    fired: bool,
}

impl<'a> StallWatch<'a> {
    pub(crate) fn new(expected: Option<Duration>, on_stall: Option<&'a dyn Fn(Duration)>) -> Self {
        Self {
            threshold: expected.map(stall_threshold),
            on_stall,
            fired: false,
        }
    }

    /// Reports the attempt's active elapsed for this poll. Fires the anomaly the
    /// first time it reaches the threshold; a no-op once fired, and a no-op when
    /// there is no threshold or no callback.
    pub(crate) fn tick(&mut self, active_elapsed: Duration) {
        if self.fired {
            return;
        }
        let (Some(threshold), Some(on_stall)) = (self.threshold, self.on_stall) else {
            return;
        };
        if active_elapsed >= threshold {
            self.fired = true;
            on_stall(active_elapsed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn threshold_uses_the_larger_of_two_x_and_the_ten_minute_grace() {
        // Small estimate: the 10-minute grace floor dominates the 2x (120s).
        assert_eq!(
            stall_threshold(Duration::from_secs(60)),
            Duration::from_secs(660)
        );
        // Large estimate: the 2x factor dominates the flat grace.
        assert_eq!(
            stall_threshold(Duration::from_secs(3600)),
            Duration::from_secs(7200)
        );
    }

    #[test]
    fn fires_exactly_once_when_the_threshold_is_crossed() {
        let count = Cell::new(0u32);
        let cb = |_elapsed: Duration| count.set(count.get() + 1);
        // expected 60s -> threshold 660s.
        let mut watch = StallWatch::new(Some(Duration::from_secs(60)), Some(&cb));
        // Below threshold: nothing.
        watch.tick(Duration::from_secs(100));
        assert_eq!(count.get(), 0);
        // Crosses the threshold: raises exactly one wake.
        watch.tick(Duration::from_secs(700));
        assert_eq!(count.get(), 1);
        // Still past it on every later poll: never raised again.
        watch.tick(Duration::from_secs(5000));
        assert_eq!(count.get(), 1);
    }

    #[test]
    fn no_expected_duration_never_raises_an_anomaly() {
        let count = Cell::new(0u32);
        let cb = |_elapsed: Duration| count.set(count.get() + 1);
        let mut watch = StallWatch::new(None, Some(&cb));
        watch.tick(Duration::from_secs(10_000_000));
        assert_eq!(count.get(), 0);
    }

    #[test]
    fn a_fast_attempt_raises_nothing() {
        let count = Cell::new(0u32);
        let cb = |_elapsed: Duration| count.set(count.get() + 1);
        // expected 120s -> threshold 720s; a 5s attempt is nowhere near it.
        let mut watch = StallWatch::new(Some(Duration::from_secs(120)), Some(&cb));
        watch.tick(Duration::from_secs(5));
        assert_eq!(count.get(), 0);
    }
}
