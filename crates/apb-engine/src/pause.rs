//! Operator pause (issue #200): who paused a run and how, the pause as every
//! read-only surface reports it, and the paused-interval clock helper.
//!
//! A pause is journaled as `run_paused` (opening a paused interval) and ended
//! by `run_resumed` (closing it). Paused time never counts against a deadline,
//! a timeout, a budget or a statistic: every clock that measures run time
//! goes through [`active_elapsed_ms`] or [`paused_ms_between`], the one place
//! that knows where the paused intervals are.

use serde::{Deserialize, Serialize};

pub mod billing;
pub mod budget;
pub mod cause;
pub mod clock;
pub mod display;
pub(crate) mod interrupt;
pub mod kind;
pub mod operator;
pub mod probe;
pub mod quota;
pub mod request;
pub mod resume_all;
pub mod waker;

pub use crate::resume_kind::WakeHold;
pub use cause::cause_of;
use clock::closes_pause;
pub use clock::{deadline_elapsed_ms, ended_intervals, paused_intervals};
pub(crate) use clock::{holds, release_wake};
pub use interrupt::{DEFAULT_GRACE, GRACE_ENV};
pub use kind::{PauseBy, PauseCause, PauseMode};
pub use probe::{WakeProbe, probe_before_wake};
pub use request::{PauseOutcome, request_pause};
pub use resume_all::{
    LaunchAsk, ResumeAllAction, ResumeAllEntry, ResumeCause, resume_all, resume_all_as,
};
pub use waker::{WakeOutcome, run_release_waker, run_waker, spawn_release_waker, spawn_waker};

use crate::event::{Event, EventPayload};
use crate::state::{RunStatus, is_legacy_resume_marker};

/// The payload of a `pause` control command. Every field is optional, so the
/// historical `{"cmd":"pause"}` line still parses (as a supervisor-style drain
/// pause with no reason).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PauseRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<PauseBy>,
    pub mode: PauseMode,
    /// When the run should resume on its own, epoch milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<u64>,
    /// Keep the workdir busy lock while paused.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub keep_lock: bool,
}

impl PauseRequest {
    /// The pause `supervisor_run_pause` posts.
    pub fn supervisor() -> Self {
        PauseRequest {
            by: Some(PauseBy::Supervisor),
            ..PauseRequest::default()
        }
    }

    /// The reason journaled on `run_paused`: the request's own, else the
    /// historical label of whoever asked.
    pub fn journal_reason(&self) -> String {
        match (&self.reason, self.by) {
            (Some(r), _) if !r.trim().is_empty() => r.clone(),
            (_, Some(PauseBy::Operator)) => "operator pause".into(),
            (_, Some(PauseBy::Engine)) => "engine pause".into(),
            _ => "supervisor pause".into(),
        }
    }

    /// The `run_paused` event this request journals.
    pub fn to_event(&self) -> EventPayload {
        EventPayload::RunPaused {
            reason: self.journal_reason(),
            by: self.by,
            mode: Some(self.mode),
            until: self.until,
            keep_lock: self.keep_lock,
        }
    }
}

/// `run_paused` written by the engine itself (an interrupted node, exhausted
/// patches): drain semantics, no `until`.
pub fn engine_paused(reason: impl Into<String>) -> EventPayload {
    EventPayload::RunPaused {
        reason: reason.into(),
        by: Some(PauseBy::Engine),
        mode: None,
        until: None,
        keep_lock: false,
    }
}

/// A run's pause as every surface reports it (`apb runs`, `apb wait`, MCP
/// `run_status`, `run_wait` and `runs_list`, the dashboard).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PauseInfo {
    pub reason: String,
    pub by: PauseBy,
    pub mode: PauseMode,
    /// When the run went paused (the `run_paused` that opened the current
    /// paused interval); absent while it is still `pausing`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub since: Option<u128>,
    /// When the run resumes on its own, epoch milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub until: Option<u64>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub keep_lock: bool,
    /// The run still reads `running`: a pause is queued and a live driver is
    /// draining toward it. A view overlay, never a journal status.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub pausing: bool,
    /// Why the engine paused the run (a quota, the budget), derived from
    /// the reason; absent for every other pause.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub cause: Option<PauseCause>,
}

/// The pause to report for a run with reported status `status`.
///
/// A paused run reports its latest `run_paused` (details) and the start of
/// the current paused interval (`since`). A running run with a live driver and
/// a queued `pending` pause reports that pause as `pausing`. Anything else has
/// no pause. A `run_paused` without `by` (an older journal) reads as
/// `supervisor`, only here: the journal is never rewritten.
pub fn pause_info(
    events: &[Event],
    status: RunStatus,
    pending: Option<&PauseRequest>,
    driver_alive: Option<bool>,
) -> Option<PauseInfo> {
    match status {
        RunStatus::Paused => {
            let (last, since) = last_pause(events)?;
            let EventPayload::RunPaused {
                reason,
                by,
                mode,
                until,
                keep_lock,
            } = &last.payload
            else {
                return None;
            };
            let by = by.unwrap_or(PauseBy::Supervisor);
            Some(PauseInfo {
                cause: cause_of(reason, by),
                reason: reason.clone(),
                by,
                mode: mode.unwrap_or_default(),
                since: Some(since),
                until: *until,
                keep_lock: *keep_lock,
                pausing: false,
            })
        }
        RunStatus::Running if driver_alive == Some(true) => pending.map(|p| {
            let reason = p.journal_reason();
            let by = p.by.unwrap_or(PauseBy::Supervisor);
            PauseInfo {
                cause: cause_of(&reason, by),
                reason,
                by,
                mode: p.mode,
                since: None,
                until: p.until,
                keep_lock: p.keep_lock,
                pausing: true,
            }
        }),
        _ => None,
    }
}

/// The latest `run_paused` event and the ts of the one that opened the
/// current paused interval (a pause of an already paused run updates the
/// details, not the start).
fn last_pause(events: &[Event]) -> Option<(&Event, u128)> {
    let mut last = None;
    let mut since = None;
    for e in events {
        if is_legacy_resume_marker(&e.payload) {
            continue;
        }
        if matches!(e.payload, EventPayload::RunPaused { .. }) {
            last = Some(e);
            since.get_or_insert(e.ts);
        } else if closes_pause(&e.payload) {
            since = None;
        }
    }
    Some((last?, since?))
}

/// Milliseconds of `[from, to)` the run was not active: paused, or ended
/// before a resume ([`ended_intervals`]). The run-time clock of the run
/// listing and `apb stats`.
pub fn inactive_ms_between(events: &[Event], from: u128, to: u128, now_ms: u128) -> u128 {
    paused_ms_in(&inactive_intervals(events, now_ms), from, to)
}

/// The paused intervals and the [`ended_intervals`] of a run: every span it
/// was not active, for a fold that measures many spans of one journal.
pub fn inactive_intervals(events: &[Event], now_ms: u128) -> Vec<(u128, u128)> {
    let mut spans = paused_intervals(events, now_ms);
    spans.extend(ended_intervals(events));
    spans
}

/// Active run time of `[from, to)`: the wall time minus
/// [`inactive_ms_between`].
pub fn active_run_ms(events: &[Event], from: u128, to: u128, now_ms: u128) -> u128 {
    to.saturating_sub(from)
        .saturating_sub(inactive_ms_between(events, from, to, now_ms))
}

/// Milliseconds of `[from, to)` the run spent paused.
pub fn paused_ms_between(events: &[Event], from: u128, to: u128, now_ms: u128) -> u128 {
    paused_ms_in(&paused_intervals(events, now_ms), from, to)
}

/// [`paused_ms_between`] over intervals already computed by
/// [`paused_intervals`], for a fold that measures many spans of one journal.
pub fn paused_ms_in(intervals: &[(u128, u128)], from: u128, to: u128) -> u128 {
    if to <= from {
        return 0;
    }
    intervals
        .iter()
        .map(|&(s, e)| e.min(to).saturating_sub(s.max(from)))
        .sum()
}

/// Milliseconds of `[from, to)` the run was not paused: the wall time minus
/// [`paused_ms_between`]. The clock every deadline, timeout and duration uses.
pub fn active_elapsed_ms(events: &[Event], from: u128, to: u128, now_ms: u128) -> u128 {
    to.saturating_sub(from)
        .saturating_sub(paused_ms_between(events, from, to, now_ms))
}

/// An absolute deadline (epoch ms) moved later by every paused interval the
/// run went through before `now_ms`: the eval `DeadlineWatch` of a resumed
/// run, a listed host-task deadline.
pub fn shift_by_paused(deadline_ms: u64, events: &[Event], now_ms: u128) -> u64 {
    let paused: u128 = paused_intervals(events, now_ms)
        .into_iter()
        .map(|(s, e)| e - s)
        .sum();
    deadline_ms.saturating_add(u64::try_from(paused).unwrap_or(u64::MAX))
}

/// Seconds the run at `run_dir` has been active since it started, paused time
/// left out: the template value `{{run.active_elapsed_seconds}}`, so a
/// playbook can compute its own budget without paused time. `None` when the
/// journal cannot be read or has no `run_started`.
pub fn active_elapsed_seconds(run_dir: &std::path::Path, now_ms: u128) -> Option<u64> {
    let events = crate::event::read_all_lossy_tail(run_dir).ok()?;
    let start = events.iter().find_map(|e| match e.payload {
        EventPayload::RunStarted { .. } => Some(e.ts),
        _ => None,
    })?;
    let active = active_elapsed_ms(&events, start, now_ms, now_ms);
    u64::try_from(active / 1000).ok()
}

impl From<WakeHold> for crate::waker::WakerFor {
    fn from(hold: WakeHold) -> Self {
        match hold {
            WakeHold::Pause(seq) => crate::waker::WakerFor::Pause(seq),
            WakeHold::Release(seq) => crate::waker::WakerFor::Release(seq),
        }
    }
}

/// How long past its wake time a release may wait for its waker before a
/// surface stops promising that one resumes it.
pub(crate) const RELEASE_GRACE_MS: u64 = 60_000;

/// Whether `released`, the release of the run at `run_dir`, was lost: its
/// wake time passed more than [`RELEASE_GRACE_MS`] ago and no waker holds
/// its lease (it never started, died, or the machine rebooted). Nothing
/// resumes such a run until a person does.
pub(crate) fn release_lost(
    run_dir: &std::path::Path,
    released: &crate::state::Released,
    now_ms: u64,
) -> bool {
    now_ms > released.wake_at_ms.saturating_add(RELEASE_GRACE_MS)
        && !crate::waker::Lease::held(run_dir, crate::waker::WakerFor::Release(released.seq))
}

#[cfg(test)]
mod tests {
    use super::clock::parked;
    use super::*;

    fn ev(ts: u128, payload: EventPayload) -> Event {
        Event {
            seq: ts as u64,
            ts,
            payload,
        }
    }

    fn paused(ts: u128) -> Event {
        ev(ts, engine_paused("x"))
    }

    fn resumed(ts: u128) -> Event {
        ev(ts, EventPayload::resumed("a", None, false))
    }

    fn started(ts: u128) -> Event {
        ev(
            ts,
            EventPayload::NodeStarted {
                node: "a".into(),
                attempt: 1,
            },
        )
    }

    const MIN: u128 = 60_000;

    fn run_started(ts: u128) -> Event {
        ev(
            ts,
            EventPayload::RunStarted {
                playbook: "p".into(),
                version: "1.0.0".into(),
                definition: None,
                external_data: false,
            },
        )
    }

    fn paused_by_at(ts: u128, by: PauseBy) -> Event {
        ev(
            ts,
            EventPayload::RunPaused {
                reason: "x".into(),
                by: Some(by),
                mode: None,
                until: None,
                keep_lock: false,
            },
        )
    }

    fn finished(ts: u128) -> Event {
        ev(
            ts,
            EventPayload::RunFinished {
                outcome: "failed".into(),
            },
        )
    }

    /// An engine pause counts, a person's does not: 10 min each inside 30.
    #[test]
    fn the_deadline_clock_leaves_out_only_a_persons_pause() {
        let events = vec![
            run_started(0),
            paused_by_at(MIN, PauseBy::Engine),
            resumed(11 * MIN),
            paused_by_at(15 * MIN, PauseBy::Operator),
            resumed(25 * MIN),
        ];
        assert_eq!(deadline_elapsed_ms(&events, 30 * MIN), 20 * MIN);
    }

    /// An ended span before a resume does not count, and an end with no
    /// resume does not age: a day later the clock reads the end.
    #[test]
    fn an_ended_run_does_not_age() {
        let ended = vec![run_started(0), finished(10 * MIN)];
        assert_eq!(deadline_elapsed_ms(&ended, 24 * 60 * MIN), 10 * MIN);
        let resumed_later = vec![run_started(0), finished(10 * MIN), resumed(70 * MIN)];
        assert_eq!(deadline_elapsed_ms(&resumed_later, 75 * MIN), 15 * MIN);
    }

    /// A person's pause overlapping an ended span is left out once.
    #[test]
    fn overlaps_are_left_out_once() {
        let events = vec![
            run_started(0),
            paused_by_at(5 * MIN, PauseBy::Operator),
            ev(
                10 * MIN,
                EventPayload::RunAborted {
                    reason: "x".into(),
                    by: None,
                },
            ),
            resumed(20 * MIN),
        ];
        // Paused [5, 10) by a person, ended [10, 20): 15 min left out of 30.
        assert_eq!(deadline_elapsed_ms(&events, 30 * MIN), 15 * MIN);
    }

    /// A pause whose owner changes without a resume splits the clock at the
    /// change, and the paused intervals stay one interval.
    #[test]
    fn a_replaced_pause_counts_only_the_engine_part() {
        let person_then_engine = vec![
            run_started(0),
            paused_by_at(MIN, PauseBy::Operator),
            paused_by_at(11 * MIN, PauseBy::Engine),
            resumed(21 * MIN),
        ];
        let engine_then_person = vec![
            run_started(0),
            paused_by_at(MIN, PauseBy::Engine),
            paused_by_at(11 * MIN, PauseBy::Operator),
            resumed(21 * MIN),
        ];
        for events in [&person_then_engine, &engine_then_person] {
            // 1 min before, 10 min of engine pause, 0 after: 11 min.
            assert_eq!(deadline_elapsed_ms(events, 21 * MIN), 11 * MIN);
            assert_eq!(
                paused_intervals(events, 21 * MIN),
                vec![(MIN, 21 * MIN)],
                "the paused intervals changed"
            );
        }
    }

    /// The park in force: a pause, a release and a replaced pause each give
    /// their hold; a resumed or ended run gives none.
    #[test]
    fn parked_names_the_hold_in_force() {
        let paused_run = vec![run_started(0), paused_by_at(1, PauseBy::Engine)];
        assert_eq!(parked(&paused_run), Some(WakeHold::Pause(1)));
        let replaced = vec![
            run_started(0),
            paused_by_at(1, PauseBy::Engine),
            paused_by_at(2, PauseBy::Supervisor),
        ];
        assert_eq!(parked(&replaced), Some(WakeHold::Pause(2)));
        let released = vec![
            run_started(0),
            ev(
                3,
                EventPayload::DriveReleased {
                    wake_at_ms: 10_000,
                    nodes: vec!["a".into()],
                },
            ),
        ];
        assert_eq!(parked(&released), Some(WakeHold::Release(3)));
        let mut resumed_run = paused_run.clone();
        resumed_run.push(resumed(4));
        assert_eq!(parked(&resumed_run), None);
        let ended = vec![
            run_started(0),
            paused_by_at(1, PauseBy::Engine),
            finished(5),
        ];
        assert_eq!(parked(&ended), None);
    }

    #[test]
    fn no_pause_means_wall_time() {
        let events = vec![started(0), started(50)];
        assert!(paused_intervals(&events, 1000).is_empty());
        assert_eq!(paused_ms_between(&events, 0, 1000, 1000), 0);
        assert_eq!(active_elapsed_ms(&events, 0, 1000, 1000), 1000);
    }

    #[test]
    fn one_closed_pause_is_excluded() {
        let events = vec![started(0), paused(100), resumed(400)];
        assert_eq!(paused_intervals(&events, 1000), vec![(100, 400)]);
        assert_eq!(active_elapsed_ms(&events, 0, 1000, 1000), 700);
        // Only the overlap with the window counts.
        assert_eq!(paused_ms_between(&events, 200, 300, 1000), 100);
        assert_eq!(paused_ms_between(&events, 0, 150, 1000), 50);
        assert_eq!(paused_ms_between(&events, 500, 900, 1000), 0);
    }

    #[test]
    fn several_pauses_add_up_and_a_repeat_pause_keeps_the_start() {
        let events = vec![
            started(0),
            paused(100),
            paused(150), // an update of the same pause
            resumed(200),
            started(210),
            paused(500),
            resumed(800),
        ];
        assert_eq!(
            paused_intervals(&events, 1000),
            vec![(100, 200), (500, 800)]
        );
        assert_eq!(active_elapsed_ms(&events, 0, 1000, 1000), 600);
    }

    #[test]
    fn an_open_pause_runs_to_now() {
        let events = vec![started(0), paused(300)];
        assert_eq!(paused_intervals(&events, 1000), vec![(300, 1000)]);
        assert_eq!(active_elapsed_ms(&events, 0, 1000, 1000), 300);
        // A `to` past now still stops counting the pause at now.
        assert_eq!(paused_ms_between(&events, 0, 2000, 1000), 700);
    }

    #[test]
    fn an_eval_deadline_moves_by_the_paused_time() {
        let events = vec![
            started(0),
            paused(100),
            resumed(400),
            paused(500),
            resumed(600),
        ];
        assert_eq!(shift_by_paused(1_000, &events, 700), 1_400);
        assert_eq!(shift_by_paused(1_000, &[], 700), 1_000);
    }

    #[test]
    fn the_active_elapsed_template_value_leaves_out_a_two_hour_pause() {
        let dir = tempfile::tempdir().unwrap();
        let hour: u128 = 3_600_000;
        let journal = format!(
            "{}\n{}\n{}\n",
            r#"{"seq":0,"ts":1000,"type":"run_started","playbook":"w","version":"1.0.0"}"#,
            r#"{"seq":1,"ts":11000,"type":"run_paused","reason":"x","by":"operator"}"#,
            format_args!(
                r#"{{"seq":2,"ts":{},"type":"run_resumed","from_node":"a"}}"#,
                11_000 + 2 * hour
            ),
        );
        std::fs::write(dir.path().join("events.jsonl"), journal).unwrap();
        assert_eq!(
            active_elapsed_seconds(dir.path(), 71_000 + 2 * hour),
            Some(70)
        );
        // No journal, no value.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(active_elapsed_seconds(empty.path(), 5_000), None);
    }

    #[test]
    fn legacy_resume_marker_is_not_a_pause() {
        let events = vec![
            started(0),
            ev(
                100,
                EventPayload::RunPaused {
                    reason: "resume from `a`".into(),
                    by: None,
                    mode: None,
                    until: None,
                    keep_lock: false,
                },
            ),
            started(110),
        ];
        assert!(paused_intervals(&events, 1000).is_empty());
    }

    #[test]
    fn a_stop_closes_an_open_pause() {
        let events = vec![
            started(0),
            paused(100),
            ev(
                600,
                EventPayload::RunAborted {
                    reason: "stop".into(),
                    by: None,
                },
            ),
        ];
        assert_eq!(paused_intervals(&events, 5000), vec![(100, 600)]);
    }

    #[test]
    fn pause_info_reads_the_latest_pause_and_its_start() {
        let events = vec![
            started(0),
            paused(100),
            ev(
                150,
                PauseRequest {
                    reason: Some("lunch".into()),
                    by: Some(PauseBy::Operator),
                    mode: PauseMode::Now,
                    until: Some(9000),
                    keep_lock: true,
                }
                .to_event(),
            ),
        ];
        let info = pause_info(&events, RunStatus::Paused, None, None).unwrap();
        assert_eq!(info.reason, "lunch");
        assert_eq!(info.by, PauseBy::Operator);
        assert_eq!(info.mode, PauseMode::Now);
        assert_eq!(info.since, Some(100));
        assert_eq!(info.until, Some(9000));
        assert!(info.keep_lock);
        assert!(!info.pausing);
    }

    #[test]
    fn pause_info_without_by_reads_supervisor() {
        let events = vec![
            started(0),
            ev(
                100,
                EventPayload::RunPaused {
                    reason: "supervisor pause".into(),
                    by: None,
                    mode: None,
                    until: None,
                    keep_lock: false,
                },
            ),
        ];
        let info = pause_info(&events, RunStatus::Paused, None, None).unwrap();
        assert_eq!(info.by, PauseBy::Supervisor);
        assert_eq!(info.mode, PauseMode::Drain);
    }

    #[test]
    fn pausing_overlay_needs_a_live_driver_and_a_pending_pause() {
        let events = vec![started(0)];
        let req = PauseRequest {
            reason: Some("later".into()),
            by: Some(PauseBy::Operator),
            ..PauseRequest::default()
        };
        let info = pause_info(&events, RunStatus::Running, Some(&req), Some(true)).unwrap();
        assert!(info.pausing);
        assert_eq!(info.since, None);
        assert_eq!(info.reason, "later");
        assert!(pause_info(&events, RunStatus::Running, Some(&req), Some(false)).is_none());
        assert!(pause_info(&events, RunStatus::Running, None, Some(true)).is_none());
        assert!(pause_info(&events, RunStatus::Succeeded, Some(&req), Some(true)).is_none());
    }
}
