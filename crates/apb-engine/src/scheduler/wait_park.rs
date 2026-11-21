//! The wait park (routine operations): whether a long timer or inbox wait
//! posts the engine's own pause instead of holding a driver. The decision
//! is a pure function of the check below; the drive posts the request it
//! returns through `control::post_control_unless_busy` (a numbering lock
//! busy past its bound posts nothing, and the wait decides again at its
//! next poll), and everything after that (the
//! `run_paused`, the driver's exit, the parked lock, the waker) is the
//! ordinary engine pause with `until`. No event, lock or waker of its own.

use crate::event::Event;
use crate::pause::{PauseBy, PauseMode, PauseRequest};

use super::journal;

/// How long the wait still has: the earlier of the timer and the node
/// timeout, minus the time already waited (saturating at 0).
pub(crate) fn left_ms(seconds: u64, timeout_seconds: u64, elapsed_ms: u128) -> u128 {
    (u128::from(seconds.min(timeout_seconds)) * 1000).saturating_sub(elapsed_ms)
}

/// When the wait ends, epoch milliseconds: `now` plus the time left over
/// the wait's active time (`journal::wait_active_ms`, the paused-interval
/// clock with the park exemption). `None` before the wait started. The
/// wait branch (D1) and the control scan (D6 rule 3.1) both call it, each
/// with one `now`, so a recomputation over the same journal is the same
/// value.
pub(crate) fn due_at_ms(
    events: &[Event],
    node: &str,
    seconds: u64,
    timeout_seconds: u64,
    now: u128,
) -> Option<u64> {
    let elapsed = journal::wait_active_ms(events, node, now)?;
    u64::try_from(now + left_ms(seconds, timeout_seconds, elapsed)).ok()
}

/// The park's own seconds of a wait kind (inbox-waits D4 "Parking" item 2):
/// the timer's seconds, the inbox wait's `timeout_seconds` (with no match
/// the timeout is the only end left, so the wake time is a pure function
/// of the journal), 0 for a webhook wait (which never parks).
pub(crate) fn park_seconds(wait_for: &apb_core::schema::WaitFor, timeout_seconds: u64) -> u64 {
    match wait_for {
        apb_core::schema::WaitFor::Timer { seconds } => *seconds,
        apb_core::schema::WaitFor::Inbox(_) => timeout_seconds,
        apb_core::schema::WaitFor::Webhook { .. } => 0,
    }
}

/// How long a drive waits before it posts a park again after the scan
/// discarded one while nothing changed.
pub(crate) const PARK_RETRY: std::time::Duration = std::time::Duration::from_secs(1);

/// The last park a drive posted: the journal length the wait read and the
/// park's control entry. A drive back at the wait with the same journal
/// length and its cursor at that entry knows the scan discarded the park
/// and nothing new arrived since (a discard journals nothing), so it posts
/// no new park until [`PARK_RETRY`] passed or something changed: a new
/// event, or a control entry the scan consumed.
#[derive(Default)]
pub(crate) struct ParkRetry {
    posted: Option<(usize, u64, std::time::Instant)>,
}

impl ParkRetry {
    /// Records the park posted as entry `seq` over a journal of `events_len`.
    pub(crate) fn posted(&mut self, events_len: usize, seq: u64) {
        self.posted = Some((events_len, seq, std::time::Instant::now()));
    }

    /// Whether the drive holds off a new park now.
    pub(crate) fn holds(&self, events_len: usize, cursor: Option<u64>) -> bool {
        self.holds_at(events_len, cursor, std::time::Instant::now())
    }

    fn holds_at(&self, events_len: usize, cursor: Option<u64>, now: std::time::Instant) -> bool {
        self.posted.is_some_and(|(len, seq, at)| {
            len == events_len && cursor == Some(seq) && now.duration_since(at) < PARK_RETRY
        })
    }
}

/// Whether the drive may post a park. Every field is an observation of the
/// drive; the decision is pure.
pub(crate) struct ParkCheck<'a> {
    /// The wait's kind may park now: true for a timer wait, true for an
    /// inbox wait whose scan is `caught_up` (its last read succeeded, its
    /// gap is journaled, no event matched) and whose `Inbox::log_stamp`
    /// still equals `scan.last_stamp` (nothing stored since its last
    /// read), false otherwise (a webhook wait, an inbox wait whose last
    /// read failed or whose gap line is not written).
    pub parks: bool,
    /// A scenario's scripted wait answers the attempt in-process.
    pub scripted: bool,
    /// A scenario run's wait is due at once (`engine_pause` journals a
    /// scenario pause in-process; the park must never take that path).
    pub scenario: bool,
    /// Nothing else runnable: no open deferred retry, every frontier entry
    /// is the current node.
    pub alone: bool,
    /// A control entry the scan left pending: posting behind it would
    /// leave the park unapplied until the entry is consumed.
    pub controls_pending: bool,
    /// `deferred::may_release` for the wake time: top level, no eval, not
    /// unattended, not supervised, more than `release_after` left.
    pub may_release: bool,
    /// The run's latest `run_guard` names `wait_park`.
    pub guarded: bool,
    /// The node the run waits on.
    pub node: &'a str,
    /// The computed wake time.
    pub wake_at_ms: u64,
    /// The drive holds the tree's busy lock.
    pub keep_lock: bool,
}

/// The park's pause request, when every condition holds.
pub(crate) fn park_request(check: ParkCheck) -> Option<PauseRequest> {
    let allowed = check.parks
        && !check.scripted
        && !check.scenario
        && check.alone
        && !check.controls_pending
        && check.may_release
        && check.guarded
        // Only a node whose reason reads back as a park may park: any other
        // id (`deploy/prod`, `a..b`) would be applied as an ordinary engine
        // pause, counted as paused time and parked again forever (D1).
        && crate::pause::cause::parse_wait_reason(&crate::pause::cause::wait_reason(check.node))
            == Some(check.node);
    allowed.then(|| PauseRequest {
        reason: Some(crate::pause::cause::wait_reason(check.node)),
        by: Some(PauseBy::Engine),
        mode: PauseMode::Drain,
        until: Some(check.wake_at_ms),
        keep_lock: check.keep_lock,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventPayload;
    use crate::pause::kind::PauseBy;

    fn ev(seq: u64, ts: u128, payload: EventPayload) -> Event {
        Event { seq, ts, payload }
    }

    fn wait_started(seq: u64, ts: u128, node: &str) -> Event {
        ev(
            seq,
            ts,
            EventPayload::WaitStarted {
                node: node.into(),
                kind: "timer".into(),
            },
        )
    }

    fn paused(seq: u64, ts: u128, reason: &str, by: PauseBy) -> Event {
        ev(
            seq,
            ts,
            EventPayload::RunPaused {
                reason: reason.into(),
                by: Some(by),
                mode: Some(PauseMode::Drain),
                until: None,
                keep_lock: false,
            },
        )
    }

    fn resumed(seq: u64, ts: u128, by: PauseBy) -> Event {
        ev(
            seq,
            ts,
            EventPayload::RunResumed {
                from_node: "pause".into(),
                by: Some(by),
                explicit: false,
                launch: None,
                definition: None,
                apb: None,
                deadline: None,
            },
        )
    }

    /// An inbox wait's `wait_for`, for [`park_seconds`].
    fn inbox_wait() -> apb_core::schema::WaitFor {
        use apb_core::schema::WaitFor;
        WaitFor::Inbox(Box::new(apb_core::inbox_wait::InboxWait {
            connector: "echo-hooks".into(),
            account: "main".into(),
            conditions: Vec::new(),
            types: Vec::new(),
            take: Vec::new(),
        }))
    }

    /// The park's own seconds per wait kind (D4 "Parking" item 2): the
    /// timer's seconds, the inbox wait's `timeout_seconds` (with no match
    /// the timeout is the only end left), 0 for a webhook wait.
    #[test]
    fn park_seconds_by_kind() {
        use apb_core::schema::WaitFor;
        assert_eq!(park_seconds(&WaitFor::Timer { seconds: 30 }, 3600), 30);
        assert_eq!(park_seconds(&inbox_wait(), 120), 120);
        assert_eq!(park_seconds(&WaitFor::Webhook { key: "ci".into() }, 60), 0);
    }

    /// The check of the happy case; each refusing test overrides one field.
    fn check() -> ParkCheck<'static> {
        ParkCheck {
            parks: true,
            scripted: false,
            scenario: false,
            alone: true,
            controls_pending: false,
            may_release: true,
            guarded: true,
            node: "pause",
            wake_at_ms: 1_000_000,
            keep_lock: true,
        }
    }

    #[test]
    fn left_ms_takes_the_earlier_of_timer_and_timeout() {
        // The timer is the shorter side.
        assert_eq!(left_ms(100, 200, 30_000), 70_000);
        // The timeout is the shorter side.
        assert_eq!(left_ms(200, 100, 30_000), 70_000);
        // Equal sides.
        assert_eq!(left_ms(100, 100, 0), 100_000);
    }

    #[test]
    fn left_ms_saturates() {
        assert_eq!(left_ms(100, 200, 300_000), 0);
        assert_eq!(left_ms(100, 200, u128::MAX), 0);
    }

    #[test]
    fn the_due_time_moves_only_with_paused_time() {
        // The wait started at 1_000 and a 100 s timer: with no pause
        // between, two reads at different `now` give the same due time.
        let events = vec![wait_started(1, 1_000, "pause")];
        let first = due_at_ms(&events, "pause", 100, 3600, 5_000).unwrap();
        let second = due_at_ms(&events, "pause", 100, 3600, 25_000).unwrap();
        assert_eq!(first, second);
        assert_eq!(first, 1_000 + 100_000);
        // The same journal with a 90 s operator pause between the two reads:
        // the later read is 90 s later.
        let paused_journal = vec![
            wait_started(1, 1_000, "pause"),
            paused(2, 10_000, "looking", PauseBy::Operator),
            resumed(3, 100_000, PauseBy::Operator),
        ];
        let before = due_at_ms(&paused_journal, "pause", 100, 3600, 5_000).unwrap();
        let after = due_at_ms(&paused_journal, "pause", 100, 3600, 150_000).unwrap();
        assert_eq!(after - before, 90_000);
        // No wait_started: no due time.
        assert_eq!(due_at_ms(&[], "pause", 100, 3600, 5_000), None);
    }

    #[test]
    fn a_long_timer_wait_asks_for_a_park() {
        let pause = park_request(check()).unwrap();
        assert_eq!(pause.reason.as_deref(), Some("wait (pause)"));
        assert_eq!(pause.by, Some(PauseBy::Engine));
        assert_eq!(pause.mode, PauseMode::Drain);
        assert_eq!(pause.until, Some(1_000_000));
        assert!(pause.keep_lock);
    }

    #[test]
    fn a_webhook_wait_never_parks() {
        let mut c = check();
        c.parks = false;
        assert!(park_request(c).is_none());
    }

    /// An inbox wait parks through the same check as a timer (D4 "Parking"
    /// item 1): `parks` true (the scan is caught up, nothing stored since
    /// its last read) gives a request shaped like the timer's; false (a
    /// webhook wait, a failed read, a gap line not written) gives none.
    #[test]
    fn an_inbox_wait_parks_only_caught_up() {
        let pause = park_request(check()).unwrap();
        assert_eq!(pause.reason.as_deref(), Some("wait (pause)"));
        assert_eq!(pause.by, Some(PauseBy::Engine));
        assert_eq!(pause.mode, PauseMode::Drain);
        assert_eq!(pause.until, Some(1_000_000));
        assert!(pause.keep_lock);
        for why in ["a webhook wait", "a failed read", "a gap not written"] {
            let mut c = check();
            c.parks = false;
            assert!(park_request(c).is_none(), "{why} parked");
        }
    }

    #[test]
    fn a_scenario_wait_never_parks() {
        let mut c = check();
        c.scenario = true;
        assert!(park_request(c).is_none());
        let mut s = check();
        s.scripted = true;
        assert!(park_request(s).is_none());
    }

    #[test]
    fn a_wait_beside_other_work_never_parks() {
        let mut c = check();
        c.alone = false;
        assert!(park_request(c).is_none());
    }

    #[test]
    fn a_pending_control_blocks_the_park() {
        let mut c = check();
        c.controls_pending = true;
        assert!(park_request(c).is_none());
    }

    #[test]
    fn a_run_that_may_not_release_never_parks() {
        let mut c = check();
        c.may_release = false;
        assert!(park_request(c).is_none());
    }

    #[test]
    fn a_discarded_park_is_posted_again_only_after_a_change_or_the_backoff() {
        let mut retry = ParkRetry::default();
        assert!(!retry.holds(10, None), "nothing posted yet");
        retry.posted(10, 4);
        let at = retry.posted.unwrap().2;
        // Discarded, nothing new: held.
        assert!(retry.holds_at(10, Some(4), at));
        // A new event or a consumed control entry: posted again.
        assert!(!retry.holds_at(11, Some(4), at));
        assert!(!retry.holds_at(10, Some(5), at));
        // The backoff passed: posted again.
        assert!(!retry.holds_at(10, Some(4), at + PARK_RETRY));
    }

    #[test]
    fn a_wait_on_an_unsafe_node_never_parks() {
        for node in ["deploy/prod", "a..b", "a\\b"] {
            let mut c = check();
            c.node = node;
            assert!(park_request(c).is_none(), "{node} parked");
        }
    }

    #[test]
    fn an_unguarded_run_never_parks() {
        let mut c = check();
        c.guarded = false;
        assert!(park_request(c).is_none());
    }
}
