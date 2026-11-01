//! When a wait resolves without anyone: the auto progress of a question
//! with a timeout, a `wait` node, a host task with a deadline and a pause
//! with `until`.
//!
//! Every deadline runs on the clock the drive itself uses for it, the
//! paused-interval clock ([`crate::pause::active_elapsed_ms`]): what is left
//! is the timeout minus the time the run was active since the wait opened.
//! The run-duration clock that also leaves out ended spans is never used
//! here. While the run is paused the timer does not run, so the progress is
//! [`AutoProgress::Frozen`] with what is left, never an absolute time.

use super::types::{AutoOutcome, AutoProgress};
use crate::event::{Event, EventPayload};

/// The state of a run's clock at one observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    pub now_ms: u64,
    /// The run is paused: no deadline runs.
    pub paused: bool,
    /// A live driver will act when the deadline comes.
    pub driver_alive: bool,
}

impl Clock {
    /// The auto progress of a wait with `remaining` milliseconds left and
    /// `outcome` at its end. Assured only while a live driver runs the
    /// clock; frozen while the run is paused.
    pub fn progress(&self, remaining: u64, outcome: AutoOutcome) -> AutoProgress {
        if self.paused {
            AutoProgress::Frozen {
                remaining_ms: remaining,
                outcome,
            }
        } else {
            AutoProgress::At {
                at_ms: self.now_ms.saturating_add(remaining),
                outcome,
                assured: self.driver_alive,
            }
        }
    }
}

/// A question of `node`: `AnsweredDefault` with a default answer,
/// `AttemptFails` without one, nothing without a timeout.
pub fn question(
    clock: Clock,
    events: &[Event],
    node: &str,
    timeout_seconds: Option<u64>,
    has_default: bool,
) -> Option<AutoProgress> {
    let secs = timeout_seconds?;
    let open = active_since(
        events,
        clock,
        |p| matches!(p, EventPayload::QuestionAsked { node: n, .. } if n == node),
    );
    let left = left_of(u128::from(secs) * 1000, open);
    let outcome = if has_default {
        AutoOutcome::AnsweredDefault
    } else {
        AutoOutcome::AttemptFails
    };
    Some(clock.progress(left, outcome))
}

/// A `wait` node: a timer elapses (success) unless its timeout comes
/// first; a webhook wait times out (failure).
pub fn wait_node(
    clock: Clock,
    events: &[Event],
    node: &str,
    wait_for: &apb_core::schema::WaitFor,
    timeout_seconds: u64,
) -> AutoProgress {
    use apb_core::schema::WaitFor;
    let open = active_since(
        events,
        clock,
        |p| matches!(p, EventPayload::WaitStarted { node: n, .. } if n == node),
    );
    let timeout = u128::from(timeout_seconds) * 1000;
    let (limit, outcome) = match wait_for {
        WaitFor::Timer { seconds } if u128::from(*seconds) * 1000 <= timeout => {
            (u128::from(*seconds) * 1000, AutoOutcome::TimerElapses)
        }
        WaitFor::Timer { .. } | WaitFor::Webhook { .. } => (timeout, AutoOutcome::WaitTimesOut),
    };
    clock.progress(left_of(limit, open), outcome)
}

/// A host task with a deadline (already moved by the run's paused time, see
/// [`crate::host_task::shifted_deadline_ms`]): the attempt fails when it
/// passes.
pub fn host_task(clock: Clock, deadline_ms: Option<u64>) -> Option<AutoProgress> {
    let deadline = deadline_ms?;
    Some(clock.progress(
        deadline.saturating_sub(clock.now_ms),
        AutoOutcome::AttemptFails,
    ))
}

/// A pause with `until`: a resume attempt, never assured (the waker may
/// probe, pause again or wait for the lock, and its process is not
/// observed). The time is absolute: a pause does not freeze its own end.
pub fn pause_until(until_ms: Option<u64>) -> Option<AutoProgress> {
    until_ms.map(|at_ms| AutoProgress::At {
        at_ms,
        outcome: AutoOutcome::ResumeAttempt,
        assured: false,
    })
}

/// How long the run has been active since the latest event `opened`
/// picks, paused time left out: the same reading the drive makes for the
/// deadline. Zero before the wait opened.
fn active_since(events: &[Event], clock: Clock, opened: impl Fn(&EventPayload) -> bool) -> u128 {
    let now = u128::from(clock.now_ms);
    events
        .iter()
        .rev()
        .find(|e| opened(&e.payload))
        .map_or(0, |e| {
            crate::pause::active_elapsed_ms(events, e.ts, now, now)
        })
}

fn left_of(limit: u128, elapsed: u128) -> u64 {
    u64::try_from(limit.saturating_sub(elapsed)).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, ts: u128, payload: EventPayload) -> Event {
        Event { seq, ts, payload }
    }

    fn asked(seq: u64, ts: u128) -> Event {
        ev(
            seq,
            ts,
            serde_json::from_value(serde_json::json!({
                "type": "question_asked", "node": "ask", "attempt": 1,
                "question": "go?", "options": [],
            }))
            .unwrap(),
        )
    }

    /// A question asked at 1 000 with a 60 s timeout, then a pause from
    /// 11 000: 10 s ran before the pause, so 50 s are left however long the
    /// pause lasts, and the progress is frozen.
    #[test]
    fn a_paused_question_deadline_is_frozen_on_the_active_clock() {
        let events = vec![
            ev(
                0,
                0,
                EventPayload::RunStarted {
                    playbook: "p".into(),
                    version: "1.0.0".into(),
                    definition: None,
                },
            ),
            asked(1, 1_000),
            ev(2, 11_000, crate::pause::engine_paused("x")),
        ];
        for now in [20_000u64, 3_600_000] {
            let clock = Clock {
                now_ms: now,
                paused: true,
                driver_alive: false,
            };
            assert_eq!(
                question(clock, &events, "ask", Some(60), true),
                Some(AutoProgress::Frozen {
                    remaining_ms: 50_000,
                    outcome: AutoOutcome::AnsweredDefault,
                })
            );
        }
    }

    /// Under a live driver the deadline is absolute and assured; without a
    /// default the outcome is a failed attempt; without a timeout there is
    /// no auto progress.
    #[test]
    fn a_running_question_deadline_is_at_an_absolute_time() {
        let events = vec![asked(0, 1_000)];
        let clock = Clock {
            now_ms: 21_000,
            paused: false,
            driver_alive: true,
        };
        assert_eq!(
            question(clock, &events, "ask", Some(60), false),
            Some(AutoProgress::At {
                at_ms: 61_000,
                outcome: AutoOutcome::AttemptFails,
                assured: true,
            })
        );
        assert_eq!(question(clock, &events, "ask", None, true), None);
    }

    /// A timer longer than its node's timeout times out instead.
    #[test]
    fn a_timer_behind_its_timeout_times_out() {
        use apb_core::schema::WaitFor;
        let events = vec![ev(
            0,
            0,
            EventPayload::WaitStarted {
                node: "w".into(),
                kind: "timer".into(),
            },
        )];
        let clock = Clock {
            now_ms: 1_000,
            paused: false,
            driver_alive: true,
        };
        let timer = |seconds| WaitFor::Timer { seconds };
        assert!(matches!(
            wait_node(clock, &events, "w", &timer(5), 10),
            AutoProgress::At {
                at_ms: 5_000,
                outcome: AutoOutcome::TimerElapses,
                ..
            }
        ));
        assert!(matches!(
            wait_node(clock, &events, "w", &timer(50), 10),
            AutoProgress::At {
                at_ms: 10_000,
                outcome: AutoOutcome::WaitTimesOut,
                ..
            }
        ));
    }
}
