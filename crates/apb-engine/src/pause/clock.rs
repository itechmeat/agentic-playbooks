//! The pause clock's fold and the park in force (routine operations):
//! the paused intervals of a run cut into segments by the owner of each
//! instant, the deadline clock over them, and which pause or release holds
//! a run. A leaf of the pause module, so the run deadline reads it without
//! depending on the pause module's waker and resume paths.

use crate::event::{Event, EventPayload};
use crate::resume_kind::WakeHold;
use crate::state::{RunStatus, is_legacy_resume_marker};

use super::kind::PauseBy;
use crate::event::RunDeadline;

/// An event that ends a paused interval. A paused run has no driver, so
/// apart from `run_resumed` only a stop (`run_aborted`) or a resume older
/// than `run_resumed` (the next `node_started`) can follow a pause.
pub(super) fn closes_pause(p: &EventPayload) -> bool {
    matches!(
        p,
        EventPayload::RunResumed { .. }
            | EventPayload::RunAborted { .. }
            | EventPayload::RunFinished { .. }
            | EventPayload::NodeStarted { .. }
    )
}

/// One paused interval of a run, `[start, end)` in epoch milliseconds, cut
/// into segments by the owner of each instant: the `by` of the latest
/// `run_paused` in force then (a later `run_paused` replaces the pause in
/// force without a resume, and its owner takes over from its `ts`).
struct PausedInterval {
    start: u128,
    end: u128,
    /// `(start, end, by)` in order, covering `[start, end)` exactly.
    segments: Vec<(u128, u128, Option<PauseBy>)>,
}

/// The one fold of a run's pauses both public clocks read: the paused
/// intervals with their owned segments. An interval still open at the end
/// of the journal runs to `now_ms`.
fn paused_fold(events: &[Event], now_ms: u128) -> Vec<PausedInterval> {
    let mut out = Vec::new();
    let mut open: Option<PausedInterval> = None;
    let close_last = |iv: &mut PausedInterval, at: u128| {
        if let Some(last) = iv.segments.last_mut() {
            last.1 = at.max(last.0);
        }
        iv.end = at.max(iv.start);
    };
    for e in events {
        if is_legacy_resume_marker(&e.payload) {
            continue;
        }
        if let EventPayload::RunPaused { by, .. } = &e.payload {
            match open.as_mut() {
                Some(iv) => {
                    close_last(iv, e.ts);
                    iv.segments
                        .push((e.ts.max(iv.start), e.ts.max(iv.start), *by));
                }
                None => {
                    open = Some(PausedInterval {
                        start: e.ts,
                        end: e.ts,
                        segments: vec![(e.ts, e.ts, *by)],
                    });
                }
            }
        } else if closes_pause(&e.payload)
            && let Some(mut iv) = open.take()
        {
            close_last(&mut iv, e.ts);
            out.push(iv);
        }
    }
    if let Some(mut iv) = open {
        close_last(&mut iv, now_ms);
        out.push(iv);
    }
    out
}

/// The paused intervals of a run, `[start, end)` in epoch milliseconds, in
/// journal order. An interval still open at the end of the journal runs to
/// `now_ms`.
pub fn paused_intervals(events: &[Event], now_ms: u128) -> Vec<(u128, u128)> {
    paused_fold(events, now_ms)
        .into_iter()
        .map(|iv| (iv.start, iv.end))
        .collect()
}

/// The deadline clock of a run: wall time since `run_started`, minus the
/// spans a person paused it (`run_paused.by == operator`) and the spans it
/// sat ended before a resume ([`ended_intervals`]). While the run is ended
/// (the fold is terminal), `now` is clamped to its latest terminal event:
/// an ended run does not age. Engine and supervisor pauses, waits on gates
/// and questions, and the workdir queue all count.
pub fn deadline_elapsed_ms(events: &[Event], now_ms: u128) -> u128 {
    let Some(start) = events.iter().find_map(|e| match e.payload {
        EventPayload::RunStarted { .. } => Some(e.ts),
        _ => None,
    }) else {
        return 0;
    };
    let now = if crate::state::RunState::fold(events)
        .run_status
        .is_terminal()
    {
        events
            .iter()
            .rev()
            .find(|e| {
                matches!(
                    e.payload,
                    EventPayload::RunFinished { .. } | EventPayload::RunAborted { .. }
                )
            })
            .map_or(now_ms, |e| e.ts.min(now_ms))
    } else {
        now_ms
    };
    let mut spans: Vec<(u128, u128)> = paused_fold(events, now)
        .into_iter()
        .flat_map(|iv| iv.segments)
        .filter(|(_, _, by)| *by == Some(PauseBy::Operator))
        .map(|(s, e, _)| (s, e))
        .chain(ended_intervals(events))
        .map(|(s, e)| (s.max(start), e.min(now)))
        .filter(|(s, e)| e > s)
        .collect();
    spans.sort_unstable();
    // Merged, so an overlap is subtracted once.
    let mut left_out = 0u128;
    let mut cur: Option<(u128, u128)> = None;
    for (s, e) in spans {
        match cur {
            Some((cs, ce)) if s <= ce => cur = Some((cs, ce.max(e))),
            Some((cs, ce)) => {
                left_out += ce - cs;
                cur = Some((s, e));
            }
            None => cur = Some((s, e)),
        }
    }
    if let Some((cs, ce)) = cur {
        left_out += ce - cs;
    }
    now.saturating_sub(start).saturating_sub(left_out)
}

/// The spans a run sat ended before a `run_resumed` brought it back to
/// life, `[run_finished | run_aborted, run_resumed)`: a run resumed days
/// after it failed was not running in between. An end no resume follows is
/// the run's end, not a span.
pub fn ended_intervals(events: &[Event]) -> Vec<(u128, u128)> {
    let mut out = Vec::new();
    let mut ended: Option<u128> = None;
    for e in events {
        match e.payload {
            EventPayload::RunFinished { .. } | EventPayload::RunAborted { .. } => {
                ended.get_or_insert(e.ts);
            }
            EventPayload::RunResumed { .. } => {
                if let Some(start) = ended.take() {
                    out.push((start, e.ts.max(start)));
                }
            }
            _ => {}
        }
    }
    out
}

/// Whether the run is paused by the `run_paused` at journal sequence `seq`.
pub(crate) fn holds(events: &[Event], seq: u64) -> bool {
    crate::state::RunState::fold(events).run_status == RunStatus::Paused
        && events
            .iter()
            .rev()
            .find(|e| matches!(e.payload, EventPayload::RunPaused { .. }))
            .map(|e| e.seq)
            == Some(seq)
}

impl WakeHold {
    /// Whether this pause or release still holds the run whose journal is
    /// `events`.
    pub(crate) fn holds(self, events: &[Event]) -> bool {
        match self {
            WakeHold::Pause(seq) => holds(events, seq),
            WakeHold::Release(seq) => release_wake(events, seq).is_some(),
        }
    }
}

/// The park in force on the run whose journal is `events`: the typed
/// [`WakeHold`] of the latest `run_paused` or `drive_released` that still
/// holds it ([`WakeHold::holds`]); `None` for a run that is driving,
/// resumed or ended.
pub(crate) fn parked(events: &[Event]) -> Option<WakeHold> {
    let latest = events.iter().rev().find(|e| {
        matches!(
            e.payload,
            EventPayload::RunPaused { .. } | EventPayload::DriveReleased { .. }
        )
    })?;
    let hold = match latest.payload {
        EventPayload::RunPaused { .. } => WakeHold::Pause(latest.seq),
        _ => WakeHold::Release(latest.seq),
    };
    hold.holds(events).then_some(hold)
}

/// The owner of the pause in force, when the run is paused: the `by` of
/// the latest `run_paused` (an older journal's missing `by` reads as
/// `supervisor`, as [`pause_info`] reads it).
pub(crate) fn paused_by(events: &[Event]) -> Option<PauseBy> {
    match parked(events)? {
        WakeHold::Pause(seq) => {
            events
                .iter()
                .find(|e| e.seq == seq)
                .and_then(|e| match e.payload {
                    EventPayload::RunPaused { by, .. } => Some(by.unwrap_or(PauseBy::Supervisor)),
                    _ => None,
                })
        }
        WakeHold::Release(_) => None,
    }
}

/// When the release at journal sequence `seq` wakes the run, while that
/// release still holds it: its `wake_at_ms`, or 0 (at once) when one of
/// its waiting retries was expedited since. `None` once anything else
/// (a resume, a pause, a stop, another release) replaced it.
pub(crate) fn release_wake(events: &[Event], seq: u64) -> Option<u64> {
    let state = crate::state::RunState::fold(events);
    if state.run_status != RunStatus::Running {
        return None;
    }
    let released = state.released.filter(|r| r.seq == seq)?;
    if state.deferred.values().any(|d| d.expedited) {
        return Some(0);
    }
    Some(released.wake_at_ms)
}

/// The latest of `run_provenance.deadline` and every
/// `run_resumed.deadline`.
pub fn in_force(events: &[Event]) -> Option<RunDeadline> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::RunProvenance { deadline, .. }
        | EventPayload::RunResumed { deadline, .. } => *deadline,
        _ => None,
    })
}

/// `total - elapsed`, saturating at 0; `None` without a deadline.
pub fn remaining_ms(events: &[Event], now_ms: u128) -> Option<u64> {
    let d = in_force(events)?;
    let elapsed = deadline_elapsed_ms(events, now_ms);
    Some(u64::try_from(u128::from(d.total_ms).saturating_sub(elapsed)).unwrap_or(u64::MAX))
}

/// Whether a deadline is in force and nothing is left.
pub fn expired(events: &[Event], now_ms: u128) -> bool {
    remaining_ms(events, now_ms) == Some(0)
}
