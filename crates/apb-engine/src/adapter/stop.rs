//! Stopping a running attempt from outside: a cancellation (a stop or a
//! `--now` pause) or the node's own timeout.
//!
//! Both go through one stop path, [`crate::proc::stop_process_tree`]: a stop
//! kills at once, while a `--now` pause and a timeout send SIGTERM, wait a
//! grace period, then SIGKILL. An agent stopped gracefully may print its
//! final record on the way out (claude does, with the usage it spent), so
//! after a graceful stop the attempt's output is drained within the usual
//! drain budget and the usage of a COMPLETE final record is kept. The
//! attempt still ends cancelled or timed out whatever it printed; a partial
//! record is ignored, and nothing printed means the usage stays unknown,
//! never zero.

use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::failure::{AgentFailure, ErrorClass};
use crate::capture::{OutputCapture, StreamCapture};
use crate::proc::{StopMode, stop_process_tree};

/// Why an attempt was stopped, and how.
#[derive(Debug, Clone, Copy)]
pub(super) struct Stopped {
    /// `Some(limit)` for a node timeout, `None` for a cancellation.
    timed_out: Option<Duration>,
    mode: StopMode,
}

impl Stopped {
    /// Whether the agent had a grace period to finish printing, so its
    /// output is worth draining.
    pub(super) fn is_graceful(&self) -> bool {
        self.mode.is_graceful()
    }

    /// The attempt's failure. `stdout` is what was drained after a graceful
    /// stop: its complete final record, if any, gives the usage.
    pub(super) fn failure(
        self,
        agent: &str,
        stdout: Option<&StreamCapture>,
        capture: OutputCapture,
    ) -> AgentFailure {
        let (class, message) = match self.timed_out {
            Some(limit) => (
                ErrorClass::Timeout,
                format!("agent timed out after {}s", limit.as_secs()),
            ),
            None => (ErrorClass::Transport, "cancelled".to_string()),
        };
        let usage = stdout.and_then(|s| apb_core::agent_output::usage(agent, &s.text()));
        AgentFailure::new(class, message)
            .with_usage(usage)
            .with_capture(capture)
    }
}

/// Stops the agent when the run cancelled it or its active time (wall time
/// minus `pending_ms` of pending-question time) reached `timeout`.
///
/// `pending_ms` (spec 2026-07-20, Task 5): a node's `timeout_seconds`
/// budgets the agent's own work, not a human's or a supervisor's answer
/// time, so a node that budgets 300s of agent work is not killed because a
/// question sat unanswered for an hour. `0` for the reprompt transport (a
/// park always spans a fresh attempt boundary); recomputed each poll tick for
/// the live `ask_user` transport, whose single attempt spans the question.
pub(super) fn check(
    child: &mut Child,
    cancel: &AtomicBool,
    started: Instant,
    timeout: Option<Duration>,
    pending_ms: u128,
) -> Option<Stopped> {
    let stopped = if cancel.load(Ordering::Relaxed) {
        Stopped {
            timed_out: None,
            mode: StopMode::on_cancel(cancel),
        }
    } else {
        let limit = timeout?;
        let pending = Duration::from_millis(u64::try_from(pending_ms).unwrap_or(u64::MAX));
        if started.elapsed().saturating_sub(pending) < limit {
            return None;
        }
        Stopped {
            timed_out: Some(limit),
            mode: StopMode::on_timeout(),
        }
    };
    stop_process_tree(child, stopped.mode);
    Some(stopped)
}
