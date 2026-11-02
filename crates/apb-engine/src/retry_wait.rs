//! Deferred retries as every run surface reports them, and the one check
//! of a request to start one now.
//!
//! A node whose ordinary retry waits out its `retry_delay` reads
//! `retry_waiting`; [`RetryWait`] says when it is due: at an epoch
//! millisecond while the run is active, frozen with what is left while the
//! run is paused, or now once its delay passed or someone expedited it. A
//! run whose driver let go while nothing but such waits was left is
//! released ([`crate::state::Released`]): it holds no driver on purpose and
//! a waker resumes it at its wake time, which surfaces say instead of
//! "driver dead".
//!
//! `retry_now` (MCP `run_resume { retry_now }`, `apb resume --retry-now`,
//! the dashboard's button) is checked by [`plan_retry_now`] on every
//! surface: a live driver gets the `retry_expedited` in its journal and
//! starts the attempt at its next look; a run nobody drives (paused,
//! released, driverless) is resumed through the ordinary resume path, which
//! journals the expedite with its `run_resumed` once every check passed.

use std::path::Path;

use serde::Serialize;

use crate::event::Event;
use crate::run_view::RunView;
use crate::scheduler::deferred::{self, Due};
use crate::state::{Released, RunState, RunStatus};

/// When an open deferred retry is due.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RetryDue {
    /// Its delay passed, or someone asked for it now: the next drive starts
    /// it.
    Now,
    /// At this epoch millisecond, while the run stays active.
    At { at_ms: u64 },
    /// The run is paused: the delay does not run, this much is left.
    Frozen { remaining_ms: u64 },
}

/// One node whose retry waits out its delay.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetryWait {
    pub node: String,
    /// The display number of the failed attempt; the delayed one follows it.
    pub attempt: u32,
    /// The retry number the delayed attempt runs as.
    pub retry: u32,
    /// The whole delay, in active run time.
    pub delay_ms: u64,
    pub due: RetryDue,
    /// The failure kind of the attempt that failed, when it was classified.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub failure_kind: Option<String>,
    /// Someone asked for it to start now (`retry_expedited`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub expedited: bool,
}

/// The open deferred retries of a run whose journal is `events` and fold
/// `state`, at `now_ms`, by node.
pub fn retry_waits(events: &[Event], state: &RunState, now_ms: u64) -> Vec<RetryWait> {
    state
        .deferred
        .iter()
        .map(|(node, d)| RetryWait {
            node: node.clone(),
            attempt: d.attempt,
            retry: d.retry,
            delay_ms: d.delay_ms,
            due: match deferred::due(events, d, u128::from(now_ms)) {
                Due::Now => RetryDue::Now,
                Due::At(at_ms) => RetryDue::At { at_ms },
                Due::Frozen { remaining_ms } => RetryDue::Frozen { remaining_ms },
            },
            failure_kind: d.failure_kind.clone(),
            expedited: d.expedited,
        })
        .collect()
}

/// The open deferred retries of the run `view` read, at `now_ms`.
pub fn of_view(view: &RunView, now_ms: u64) -> Vec<RetryWait> {
    retry_waits(&view.events, &view.state, now_ms)
}

/// The nodes as a text: `` `a` `` or `` `a`, `b` ``.
fn node_list(nodes: &[String]) -> String {
    nodes
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The one line every surface prints for a released run, instead of
/// "driver dead": `waiting to retry `<node>` at <T>; a waker resumes the
/// run then`.
pub fn released_line(released: &Released) -> String {
    format!(
        "waiting to retry {} at {}; a waker resumes the run then",
        node_list(&released.nodes),
        apb_core::clock::iso_utc(released.wake_at_ms)
    )
}

/// The one line of a node's waiting retry: `retry 2 at <T>`, `retry 2
/// frozen while paused, 1m 30s left`, `retry 2 due now`.
pub fn wait_line(w: &RetryWait) -> String {
    match w.due {
        RetryDue::Now => format!("retry {} due now", w.retry),
        RetryDue::At { at_ms } => {
            format!("retry {} at {}", w.retry, apb_core::clock::iso_utc(at_ms))
        }
        RetryDue::Frozen { remaining_ms } => format!(
            "retry {} frozen while paused, {} left",
            w.retry,
            seconds_words(remaining_ms)
        ),
    }
}

/// `1h 2m 3s`, `45s`.
fn seconds_words(ms: u64) -> String {
    let s = ms.div_ceil(1000);
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    match (h, m) {
        (0, 0) => format!("{s}s"),
        (0, _) => format!("{m}m {s}s"),
        _ => format!("{h}h {m}m {s}s"),
    }
}

/// A request to start a deferred retry now that the run takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryNowPlan {
    /// The nodes it expedites.
    pub nodes: Vec<String>,
    /// A live driver holds the run: the expedite alone starts the attempt,
    /// no resume follows.
    pub live: bool,
}

/// Why a run does not take a retry-now request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetryNowRefusal {
    /// `not_found`, `run_finished` or `no_deferred_retry`.
    pub error: &'static str,
    pub detail: String,
}

/// Whether run `run_id` takes a request to start the deferred retry of
/// `node` (every open one when `None`) now, and how: the one check of every
/// surface.
pub fn plan_retry_now(
    run_dir: &Path,
    run_id: &str,
    node: Option<&str>,
) -> Result<RetryNowPlan, RetryNowRefusal> {
    let view = RunView::load(run_dir, run_id).map_err(|e| RetryNowRefusal {
        error: "not_found",
        detail: e.to_string(),
    })?;
    if view.run_status.is_terminal() {
        return Err(RetryNowRefusal {
            error: "run_finished",
            detail: format!(
                "run `{run_id}` has ended ({}); nothing waits to retry",
                view.run_status.as_str()
            ),
        });
    }
    let nodes: Vec<String> = view
        .state
        .deferred
        .keys()
        .filter(|n| node.is_none_or(|want| want == n.as_str()))
        .cloned()
        .collect();
    if nodes.is_empty() {
        return Err(RetryNowRefusal {
            error: "no_deferred_retry",
            detail: match node {
                Some(n) => format!("node `{n}` of run `{run_id}` has no retry waiting"),
                None => format!("run `{run_id}` has no retry waiting"),
            },
        });
    }
    // A released run's driver is gone or on its way out: the retry goes
    // through a resume.
    Ok(RetryNowPlan {
        nodes,
        live: view.driver_alive == Some(true)
            && view.run_status != RunStatus::Paused
            && view.state.released.is_none(),
    })
}
