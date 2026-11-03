//! Deferred retries (`retry_delay`): the delay of an ordinary retry, when a
//! deferral is due, the continuation the delayed attempt runs on, and the
//! drive's rule for holding a deferred node back.
//!
//! An `agent_task` with a `retry_delay` does not start its next ordinary
//! retry at once: the attempt loop returns `AttemptOutcome::Deferred` with a
//! `retry_deferred` after the failed attempt's events, and the drive keeps
//! the node out of admission until the delay passed. The delay counts
//! active run time from that event (the paused-interval clock in
//! [`crate::pause`]): a pause freezes it, a released driver does not. The
//! delayed attempt continues the execution on the recorded chain step,
//! retry number and binding snapshot, never on the current overlay.
//!
//! The open deferrals come from the journal ([`RunState::deferred`]), so a
//! resume, a released run's wake and the live drive all read the same
//! state.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use apb_core::schema::RetryDelay;

use crate::error::EngineError;
use crate::event::{Event, EventLog, EventPayload, read_all};
use crate::state::{DeferredRetry, NodeStatus, RunState, RunStatus};

/// How long a drive with nothing but deferred retries left sleeps itself
/// before it releases the run instead: 60 s.
pub const RELEASE_AFTER: Duration = Duration::from_secs(60);

/// Overrides [`RELEASE_AFTER`] in milliseconds, for tests only: a release
/// build ignores it ([`crate::env_duration::test_override`]).
pub const RELEASE_AFTER_ENV: &str = "APB_RELEASE_AFTER_MS";

/// Whether a drive with nothing to run but deferred retries, the earliest
/// due at `due_at_ms`, releases the run instead of sleeping: only a
/// top-level autonomous run (a child's parent drives it, a supervisor or an
/// eval deadline lives in the driver), never an unattended one (a CI job's
/// foreground `apb run` must end with the run's own outcome, not leave a
/// waker its container kills), and only for a wait longer than
/// [`release_after`].
pub(crate) fn may_release(
    cfg: &crate::run_config::RunConfig,
    mode: crate::run_config::RunMode,
    due_at_ms: u64,
    now_ms: u128,
) -> bool {
    let wait = u128::from(due_at_ms).saturating_sub(now_ms);
    cfg.parent_run.is_none()
        && cfg.eval.is_none()
        && !cfg.unattended
        && !mode.parks_on_failure()
        && wait > release_after().as_millis()
}

/// The longest single sleep of a drive waiting for a deferral: the control
/// scan runs at least this often.
pub(crate) const WAIT_TICK: Duration = Duration::from_millis(200);

/// The release threshold in force: [`RELEASE_AFTER`], shortened by
/// [`RELEASE_AFTER_ENV`] in a debug build only.
pub fn release_after() -> Duration {
    release_after_from(std::env::var(RELEASE_AFTER_ENV).ok().as_deref())
}

fn release_after_from(raw: Option<&str>) -> Duration {
    crate::env_duration::test_override(raw).unwrap_or(RELEASE_AFTER)
}

/// The delay before retry `retry` (1-based within its chain step):
/// `min(initial * factor^(retry - 1), max)`, raised to a failure's
/// `Retry-After` when that is longer, still capped by `max`. An unreadable
/// duration reads as the 1 s floor and the 7-day ceiling (the validator
/// refuses both).
pub fn delay_ms(policy: &RetryDelay, retry: u32, retry_after: Option<Duration>) -> u64 {
    let initial_ms = policy.initial_secs().unwrap_or(1).saturating_mul(1000) as f64;
    let max_ms = policy
        .max_secs()
        .unwrap_or(RetryDelay::CEILING_SECS)
        .saturating_mul(1000);
    let exponent = i32::try_from(retry.saturating_sub(1)).unwrap_or(i32::MAX);
    let grown = initial_ms * policy.factor.max(1.0).powi(exponent);
    let base = if grown.is_finite() && grown < max_ms as f64 {
        grown as u64
    } else {
        max_ms
    };
    let asked = retry_after.map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    base.max(asked).min(max_ms)
}

/// Where a delayed attempt continues its execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RetryContinuation {
    /// The chain step (index into the binding's chain).
    pub step: u32,
    /// The retry number the attempt runs as (`try_i`).
    pub retry: u32,
    /// The binding snapshot's digest.
    pub binding: String,
    /// The display number of the failed attempt; the next one follows it.
    pub attempt: u32,
}

impl RetryContinuation {
    pub(crate) fn of(d: &DeferredRetry) -> Self {
        Self {
            step: d.step,
            retry: d.retry,
            binding: d.binding.clone(),
            attempt: d.attempt,
        }
    }
}

/// When an open deferral is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// Its delay passed, or it was expedited.
    Now,
    /// At this epoch millisecond, while the run stays active.
    At(u64),
    /// The run is paused: this much of the delay is left, frozen.
    Frozen { remaining_ms: u64 },
}

/// When deferral `d` is due at `now_ms`, on the paused-interval clock of
/// `events` (the run's journal).
pub fn due(events: &[Event], d: &DeferredRetry, now_ms: u128) -> Due {
    due_in(
        events,
        d,
        now_ms,
        RunState::fold(events).run_status == RunStatus::Paused,
    )
}

/// [`due`] for a caller that folded the run already: `paused` is its
/// status, so a drive turn with several deferrals folds the journal once.
fn due_in(events: &[Event], d: &DeferredRetry, now_ms: u128, paused: bool) -> Due {
    if d.expedited {
        return Due::Now;
    }
    let elapsed = crate::pause::active_elapsed_ms(events, d.ts, now_ms, now_ms);
    let remaining = u128::from(d.delay_ms).saturating_sub(elapsed);
    if remaining == 0 {
        return Due::Now;
    }
    let remaining = u64::try_from(remaining).unwrap_or(u64::MAX);
    if paused {
        return Due::Frozen {
            remaining_ms: remaining,
        };
    }
    Due::At(
        u64::try_from(now_ms)
            .unwrap_or(u64::MAX)
            .saturating_add(remaining),
    )
}

/// The open deferrals of `state` not due yet at `now_ms`, with the epoch
/// millisecond each is due at.
pub(crate) fn held(state: &RunState, events: &[Event], now_ms: u128) -> BTreeMap<String, u64> {
    let paused = state.run_status == RunStatus::Paused;
    state
        .deferred
        .iter()
        .filter_map(|(node, d)| match due_in(events, d, now_ms, paused) {
            Due::Now => None,
            Due::At(at) => Some((node.clone(), at)),
            // A live drive is never paused; read a frozen one as due later.
            Due::Frozen { remaining_ms } => Some((
                node.clone(),
                u64::try_from(now_ms)
                    .unwrap_or(u64::MAX)
                    .saturating_add(remaining_ms),
            )),
        })
        .collect()
}

pub use crate::resume_kind::ExpediteBy;

/// The waiting retries of `node` (every one when `None`) in the run whose
/// journal is `events` that nobody expedited yet.
pub(crate) fn waiting(events: &[Event], node: Option<&str>) -> Vec<String> {
    RunState::fold(events)
        .deferred
        .iter()
        .filter(|(n, d)| node.is_none_or(|want| want == n.as_str()) && !d.expedited)
        .map(|(n, _)| n.clone())
        .collect()
}

/// Journals `retry_expedited` for the open deferral of `node`, or for every
/// open one when `node` is `None`, so each is due at once. Returns the nodes
/// expedited; a node with no open deferral is left alone.
pub fn expedite(
    log: &mut EventLog,
    node: Option<&str>,
    by: Option<ExpediteBy>,
) -> Result<Vec<String>, EngineError> {
    let Some(run_dir) = log.run_dir() else {
        return Ok(Vec::new());
    };
    let nodes = waiting(&read_all(&run_dir)?, node);
    for n in &nodes {
        log.append(EventPayload::RetryExpedited {
            node: n.clone(),
            by,
        })?;
    }
    Ok(nodes)
}

/// The drive's admission rule for held deferrals. A held node stays in the
/// frontier, so joins downstream count it as in flight and a fork policy or
/// a won race cancels it like any waiting branch, but it is never admitted
/// before it is due. When `current` is held, the first frontier node that is
/// not takes its place and `current` goes back to the frontier's tail.
/// Returns the epoch millisecond the drive must wait for when nothing else
/// can run.
pub(crate) fn hold(
    held: &BTreeMap<String, u64>,
    current: &mut String,
    frontier: &mut Vec<String>,
) -> Option<u64> {
    if !held.contains_key(current.as_str()) {
        return None;
    }
    match frontier.iter().position(|n| !held.contains_key(n)) {
        Some(i) => {
            let next = frontier.remove(i);
            let waiting = std::mem::replace(current, next);
            if !frontier.contains(&waiting) {
                frontier.push(waiting);
            }
            None
        }
        None => held.values().min().copied(),
    }
}

/// The drive's rule for a node that parks the sequential arm (a review, a
/// publish consent, a wait, a pending question) while a deferred retry is
/// due: the retry takes `current` and the parked node goes back to the
/// frontier's head, so the drive returns to it right after. A parked node
/// spins the loop with `current` fixed, so without this a due retry would
/// wait for a person or a signal. Every parking arm re-enters from the
/// journal (a request or a wait is declared once), so stepping away from
/// one and back is safe. `due` holds the open deferrals due now. Returns
/// whether `current` changed.
pub(crate) fn give_way(
    due: &BTreeSet<String>,
    parks: bool,
    current: &mut String,
    frontier: &mut Vec<String>,
) -> bool {
    if !parks || due.contains(current.as_str()) {
        return false;
    }
    let Some(i) = frontier.iter().position(|n| due.contains(n)) else {
        return false;
    };
    let next = frontier.remove(i);
    let parked = std::mem::replace(current, next);
    if !frontier.contains(&parked) {
        frontier.insert(0, parked);
    }
    true
}

/// [`expedite`] from outside the drive: journals `retry_expedited` into the
/// run at `run_dir` directly. A live driver sees it at its next look and
/// admits the node at once; a released run's waker wakes it at once; a
/// paused run's next drive finds it due.
pub fn expedite_run(
    run_dir: &std::path::Path,
    node: Option<&str>,
    by: Option<ExpediteBy>,
) -> Result<Vec<String>, EngineError> {
    let mut log = EventLog::open(run_dir)?;
    expedite(&mut log, node, by)
}

/// [`expedite_run`] for a run a live driver holds, decided under the run's
/// event lock (the lock the drive journals its release under, and a pause
/// or a stop journal under): `None`, with nothing journaled, when the run
/// is no longer driven as it is now (released, paused, ended, or its driver
/// gone), so the caller resumes it instead. Expedited before the release,
/// the drive sees the retry due and does not let go.
pub fn expedite_live(
    run_dir: &std::path::Path,
    run_id: &str,
    node: Option<&str>,
    by: ExpediteBy,
) -> Result<Option<Vec<String>>, EngineError> {
    let _lock = apb_core::fsutil::lock_dir(run_dir, crate::stop::EVENT_LOCK)?;
    let state = RunState::fold(&read_all(run_dir)?);
    if state.run_status != RunStatus::Running
        || state.released.is_some()
        || crate::liveness::driver_alive(run_dir, run_id) != Some(true)
    {
        return Ok(None);
    }
    expedite_run(run_dir, node, Some(by)).map(Some)
}

/// Journals `drive_released` for the run at `run_dir`, under the run's
/// event lock, only while nothing came in since the drive last looked: no
/// control its scan applies is pending after `control_cursor` (a pause, a
/// stop, a patch) and every open deferral is still not due
/// (none was expedited). Returns the release journaled, `None` when the
/// drive goes on (it applies what came in at its next turn). A lock it
/// cannot take is a `None` too: the drive looks again.
///
/// Once the release is journaled, `let_go` runs before the event lock is
/// given back: the driver gives up `driver.pid` and the busy lock there, so
/// whoever decides under that lock next (a stop, a pause, a retry-now, a
/// resume) finds a run nothing drives, never a departing driver.
pub(crate) fn try_release(
    run_dir: &std::path::Path,
    log: &mut EventLog,
    control_cursor: Option<u64>,
    now_ms: u128,
    let_go: impl FnOnce(),
) -> Result<Option<u64>, EngineError> {
    let Ok(_lock) = apb_core::fsutil::lock_dir(run_dir, crate::stop::EVENT_LOCK) else {
        return Ok(None);
    };
    let pending = crate::control::read_control_after(run_dir, control_cursor)?
        .into_iter()
        .any(|e| {
            !matches!(
                e.cmd,
                crate::control::Control::Retry { .. }
                    | crate::control::Control::ContinueFrom { .. }
            )
        });
    if pending {
        return Ok(None);
    }
    let events = read_all(run_dir)?;
    let state = RunState::fold(&events);
    let held = held(&state, &events, now_ms);
    if held.is_empty() || held.len() != state.deferred.len() {
        return Ok(None);
    }
    let Some(wake_at_ms) = held.values().min().copied() else {
        return Ok(None);
    };
    log.append(EventPayload::DriveReleased {
        wake_at_ms,
        nodes: held.keys().cloned().collect(),
    })?;
    let_go();
    Ok(Some(wake_at_ms))
}

/// The open deferrals of the run at `run_dir` not due yet, read fresh from
/// its journal: what a batch waits for after a member deferred or a wait
/// ran out.
pub(crate) fn held_now(run_dir: &std::path::Path) -> Result<BTreeMap<String, u64>, EngineError> {
    let events = read_all(run_dir)?;
    Ok(held_in(
        run_dir,
        &RunState::fold(&events),
        &events,
        apb_core::clock::now_ms(),
    ))
}

/// [`held`] for the run in `run_dir`: a scenario run's delays are due at
/// once (journaled as in a run, nothing waits, nothing is released).
pub(crate) fn held_in(
    run_dir: &std::path::Path,
    state: &RunState,
    events: &[Event],
    now_ms: u128,
) -> BTreeMap<String, u64> {
    if crate::scenario::session::skips(run_dir, crate::scenario::BoundaryKind::RetryDelay) {
        return BTreeMap::new();
    }
    held(state, events, now_ms)
}

/// Writes off every deferred retry still open as cancelled, before the run
/// ends otherwise: a run never finishes with a deferral open. The node's
/// visit is still open (a deferral journals no `node_finished`), so its
/// `node_finished` alone closes it, with the attempt it reached.
pub(crate) fn cancel_open(log: &mut EventLog) -> Result<(), EngineError> {
    let Some(run_dir) = log.run_dir() else {
        return Ok(());
    };
    let state = RunState::fold(&read_all(&run_dir)?);
    for (node, d) in &state.deferred {
        log.append(EventPayload::node_finished(
            node.clone(),
            NodeStatus::Cancelled.as_str(),
            d.attempt.max(1),
            "cancelled",
        ))?;
    }
    Ok(())
}

/// How long to sleep before looking again, for a wait until `due_at_ms`.
pub(crate) fn sleep_for(due_at_ms: u64, now_ms: u128) -> Duration {
    let left = u128::from(due_at_ms).saturating_sub(now_ms);
    Duration::from_millis(u64::try_from(left).unwrap_or(u64::MAX)).min(WAIT_TICK)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(initial: &str, factor: f64, max: Option<&str>) -> RetryDelay {
        RetryDelay {
            initial: initial.into(),
            factor,
            max: max.map(str::to_string),
        }
    }

    #[test]
    fn the_delay_grows_by_its_factor_up_to_max() {
        let p = policy("1m", 2.0, Some("5m"));
        assert_eq!(delay_ms(&p, 1, None), 60_000);
        assert_eq!(delay_ms(&p, 2, None), 120_000);
        assert_eq!(delay_ms(&p, 3, None), 240_000);
        assert_eq!(delay_ms(&p, 4, None), 300_000);
        assert_eq!(delay_ms(&p, 40, None), 300_000);
        let flat = policy("10s", 1.0, None);
        assert_eq!(delay_ms(&flat, 7, None), 10_000);
    }

    #[test]
    fn retry_after_raises_the_delay_but_never_past_max() {
        let p = policy("10s", 2.0, Some("1m"));
        assert_eq!(delay_ms(&p, 1, Some(Duration::from_secs(30))), 30_000);
        assert_eq!(delay_ms(&p, 1, Some(Duration::from_secs(5))), 10_000);
        assert_eq!(delay_ms(&p, 1, Some(Duration::from_secs(600))), 60_000);
    }

    fn deferred_run() -> (tempfile::TempDir, EventLog) {
        let dir = tempfile::tempdir().unwrap();
        let mut log = EventLog::create(dir.path()).unwrap();
        log.append(EventPayload::RunStarted {
            playbook: "p".into(),
            version: "1.0.0".into(),
            definition: None,
        })
        .unwrap();
        log.append(EventPayload::RetryDeferred {
            node: "work".into(),
            after_attempt: 0,
            retry: 1,
            step: 0,
            binding: "b".repeat(64),
            delay_ms: 3_600_000,
            failure_kind: None,
        })
        .unwrap();
        (dir, log)
    }

    fn released(dir: &std::path::Path) -> bool {
        RunState::fold(&read_all(dir).unwrap()).released.is_some()
    }

    /// The drive lets go of the run only while nothing came in since its
    /// last look: a control its scan applies (a pause here) or an expedite
    /// keeps it, and with neither the release is journaled.
    #[test]
    fn a_release_is_refused_when_a_control_or_an_expedite_came_in() {
        let now = apb_core::clock::now_ms();
        let (dir, mut log) = deferred_run();
        let seq = crate::control::post_control(
            dir.path(),
            crate::control::Control::Pause(crate::pause::PauseRequest::default()),
        )
        .unwrap();
        assert_eq!(
            try_release(dir.path(), &mut log, None, now, || ()).unwrap(),
            None
        );
        assert!(!released(dir.path()));
        // Once the scan applied it, the release goes through.
        let at = try_release(dir.path(), &mut log, Some(seq), now, || ()).unwrap();
        assert!(at.is_some_and(|at| u128::from(at) > now));
        assert!(released(dir.path()));

        let (dir, mut log) = deferred_run();
        expedite(&mut log, Some("work"), Some(ExpediteBy::Host)).unwrap();
        assert_eq!(
            try_release(dir.path(), &mut log, None, now, || ()).unwrap(),
            None
        );
        assert!(!released(dir.path()));
    }

    /// The documented default (docs and spec say "60 s"), and a shipped
    /// (release) build never shortened by a value left in the environment.
    #[test]
    fn a_drive_releases_after_a_minute_by_default() {
        assert_eq!(release_after_from(None), Duration::from_secs(60));
        // The knob shortens it in a debug build only.
        let want = if cfg!(debug_assertions) {
            Duration::from_millis(300)
        } else {
            Duration::from_secs(60)
        };
        assert_eq!(release_after_from(Some("300")), want);
    }

    #[test]
    fn a_held_current_gives_way_to_a_runnable_sibling() {
        let held: BTreeMap<String, u64> = [("a".to_string(), 500)].into();
        let mut current = "a".to_string();
        let mut frontier = vec!["b".to_string()];
        assert_eq!(hold(&held, &mut current, &mut frontier), None);
        assert_eq!(current, "b");
        assert_eq!(frontier, vec!["a".to_string()]);
        // Nothing else to run: wait for the earliest due time.
        let mut current = "a".to_string();
        let mut frontier = vec![];
        assert_eq!(hold(&held, &mut current, &mut frontier), Some(500));
        assert_eq!(current, "a");
    }
}
