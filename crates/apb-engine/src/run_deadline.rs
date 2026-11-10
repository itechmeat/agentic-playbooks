//! A run's deadline (routine operations, D2): the wall-clock limit of a
//! top-level production run, from the playbook's `deadline:` or a run
//! argument, measured on the deadline clock
//! ([`crate::pause::deadline_elapsed_ms`]): wall time since the start
//! without the time a person paused it or it sat ended.
//!
//! The deadline in force is the latest of `run_provenance.deadline` and
//! every `run_resumed.deadline` (a person's grant at a resume). At expiry
//! the run ends through the one stop path ([`crate::stop::stop_run_with`]):
//! a graceful abort decided on the journal under the run's event lock, so
//! a grant a resume committed a moment before counts. A drive checks a
//! [`DeadlineLatch`] wherever work starts; a parked run is ended by its
//! deadline waker; `apb resume --all` ends or re-arms them after a reboot.

use std::path::Path;

use apb_core::schema::Playbook;

use crate::error::EngineError;
use crate::event::Event;
use crate::pause::kind::PauseBy;
use crate::resume_kind::WakeHold;
use crate::stop::{StopOutcome, StopRequest};

pub use crate::event::{DeadlineSource, RunDeadline};

/// The reason prefix of a run the deadline ended.
pub const DEADLINE_EXCEEDED: &str = "deadline_exceeded";

/// The refusal code of a plain resume past the deadline.
pub const DEADLINE_PASSED: &str = "deadline_passed";

/// The deadline a start records: the run argument wins, else the
/// playbook's (unparsable is none: V200 refuses it before any start).
pub fn at_start(playbook: &Playbook, run_arg_ms: Option<u64>) -> Option<RunDeadline> {
    if let Some(total_ms) = run_arg_ms {
        return Some(RunDeadline {
            total_ms,
            source: DeadlineSource::Run,
        });
    }
    let raw = playbook.deadline.as_deref()?;
    let secs = apb_core::duration::parse_duration_str(raw)?;
    (secs > 0).then_some(RunDeadline {
        total_ms: secs.saturating_mul(1000),
        source: DeadlineSource::Playbook,
    })
}

/// The refusal code of a deadline argument that does not parse.
pub const DEADLINE_INVALID: &str = "deadline_invalid";

/// A run argument (`--deadline`, `deadline`) as milliseconds: a duration
/// greater than zero (`45m`, `2h`, `90`). Every launch and resume surface
/// parses it here, before anything else; the error starts with
/// [`DEADLINE_INVALID`].
pub fn parse_arg(raw: &str) -> Result<u64, String> {
    match apb_core::duration::parse_duration_str(raw) {
        Some(secs) if secs > 0 => Ok(secs.saturating_mul(1000)),
        _ => Err(format!(
            "{DEADLINE_INVALID}: `{raw}` is not a duration greater than zero (use units such as `45m`, `2h`, `1d`)"
        )),
    }
}

pub use crate::pause::clock::{expired, in_force, remaining_ms};

/// The `run_aborted` reason of a run its deadline ended.
pub fn reason(d: &RunDeadline) -> String {
    format!(
        "{DEADLINE_EXCEEDED}: the run passed its deadline of {}",
        apb_core::duration::format_duration_ms(d.total_ms)
    )
}

/// A drive's expiry as a wall-clock instant: a drive's deadline clock
/// cannot stop while it drives (a person's pause ends the drive), so it is
/// computed once. The one admission check where work starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeadlineLatch {
    pub expires_at_ms: u128,
}

impl DeadlineLatch {
    /// The latch of a drive starting now on `events`, when a deadline is in
    /// force.
    pub fn of(events: &[Event], now_ms: u128) -> Option<Self> {
        remaining_ms(events, now_ms).map(|left| DeadlineLatch {
            expires_at_ms: now_ms + u128::from(left),
        })
    }

    /// Whether the deadline passed.
    pub fn expired(&self) -> bool {
        apb_core::clock::now_ms() >= self.expires_at_ms
    }

    /// Whole seconds left, rounded up, at least 1 (a bound for one call).
    pub fn left_secs(&self) -> u64 {
        let left = self.expires_at_ms.saturating_sub(apb_core::clock::now_ms());
        u64::try_from(left.div_ceil(1000))
            .unwrap_or(u64::MAX)
            .max(1)
    }
}

/// The latches of the drives of this process, by run directory: every
/// place a drive starts work reads its run's latch here, so the check needs
/// no extra parameter through the node and list layers.
static LATCHES: std::sync::Mutex<std::collections::BTreeMap<std::path::PathBuf, DriveDeadline>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

#[derive(Clone)]
struct DriveDeadline {
    root: std::path::PathBuf,
    run_id: String,
    latch: DeadlineLatch,
}

/// A drive's registration of its latch; dropping it (the drive ended)
/// removes it.
pub(crate) struct LatchGuard(std::path::PathBuf);

impl Drop for LatchGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = LATCHES.lock() {
            map.remove(&self.0);
        }
    }
}

/// Registers the latch of the drive of `run_id` at `run_dir`.
pub(crate) fn register(
    root: &Path,
    run_dir: &Path,
    run_id: &str,
    latch: DeadlineLatch,
) -> LatchGuard {
    if let Ok(mut map) = LATCHES.lock() {
        map.insert(
            run_dir.to_path_buf(),
            DriveDeadline {
                root: root.to_path_buf(),
                run_id: run_id.to_string(),
                latch,
            },
        );
    }
    LatchGuard(run_dir.to_path_buf())
}

/// The latch of the drive of the run at `run_dir` in this process.
pub(crate) fn latch(run_dir: &Path) -> Option<DeadlineLatch> {
    LATCHES
        .lock()
        .ok()
        .and_then(|map| map.get(run_dir).map(|d| d.latch))
}

/// The one admission check where work starts: `true` when the drive's
/// deadline passed, after the stop was posted (idempotent: the stop is
/// decided under the event lock and the drive applies it once). Work that
/// was about to start does not start.
pub(crate) fn refuses_work(run_dir: &Path) -> bool {
    let Some(d) = LATCHES
        .lock()
        .ok()
        .and_then(|map| map.get(run_dir).cloned())
    else {
        return false;
    };
    if !d.latch.expired() {
        return false;
    }
    let _ = stop_if_expired(&d.root, &d.run_id, Expect::Driving);
    true
}

/// Which state the caller saw the run in; rechecked under the event lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// A drive of this process holds the run: no person's pause since.
    Driving,
    /// The run was parked by this hold, which must still hold it.
    Parked(WakeHold),
    /// Whatever state it is in (a queued start, a resume's refusal,
    /// `apb resume --all`), except paused by a person.
    Any,
}

impl Expect {
    fn holds(self, events: &[Event]) -> bool {
        // A person's pause freezes the clock: never ended by the engine.
        if crate::pause::clock::paused_by(events) == Some(PauseBy::Operator) {
            return false;
        }
        match self {
            Expect::Driving => crate::pause::clock::parked(events).is_none(),
            Expect::Parked(hold) => hold.holds(events),
            Expect::Any => true,
        }
    }
}

/// Ends the run through the one stop path when its deadline passed, decided
/// on the journal read under the event lock: not terminal, expired (so a
/// grant a resume committed a moment ago counts) and `expect` still holds.
/// `Ok(true)` when it stopped the run, `Ok(false)` when it was not due.
pub fn stop_if_expired(root: &Path, run_id: &str, expect: Expect) -> Result<bool, EngineError> {
    let run_dir = root.join(".apb/runs").join(run_id);
    let events = crate::event::read_all_for_stop(&run_dir, false)?;
    let Some(d) = in_force(&events) else {
        return Ok(false);
    };
    let reason = reason(&d);
    // Idempotent: an abort already pending for the run (posted by another
    // check of this drive a moment ago) is not posted again, so a resume
    // never meets a stale one.
    let pending_abort = || {
        let cursor = crate::control::read_control_cursor(&run_dir).ok().flatten();
        crate::control::read_control_after(&run_dir, cursor).is_ok_and(|entries| {
            entries
                .iter()
                .any(|e| matches!(e.cmd, crate::control::Control::Abort { .. }))
        })
    };
    let due = |events: &[Event]| {
        expired(events, apb_core::clock::now_ms()) && expect.holds(events) && !pending_abort()
    };
    let outcome = crate::stop::stop_run_with(
        root,
        run_id,
        &StopRequest {
            by: PauseBy::Engine,
            reason: &reason,
            graceful: true,
            due: Some(&due),
        },
    )?;
    Ok(matches!(
        outcome,
        StopOutcome::SignaledLiveDriver | StopOutcome::FinalizedDeadRun
    ))
}

/// What a resume may do with a run whose deadline passed, decided on
/// `events`: `None` lets it go on (a grant, `deadline_ms` set, always
/// does; so does a run with time left). A person's plain resume is refused
/// `deadline_passed` with nothing journaled; an engine or supervisor resume
/// is refused `deadline_exceeded` and the caller ends the run
/// ([`stop_if_expired`] with [`Expect::Any`]) once it holds no lock.
pub fn resume_refusal(
    events: &[Event],
    run_id: &str,
    deadline_ms: Option<u64>,
    by: PauseBy,
    now_ms: u128,
) -> Option<EngineError> {
    if deadline_ms.is_some() || !expired(events, now_ms) {
        return None;
    }
    let d = in_force(events)?;
    let total = apb_core::duration::format_duration_ms(d.total_ms);
    Some(EngineError::Conflict(match by {
        PauseBy::Operator => format!(
            "{DEADLINE_PASSED}: run `{run_id}` passed its deadline of {total}; resume it with --deadline <duration> to give it more time"
        ),
        _ => format!("{}; the engine does not resume it", reason(&d)),
    }))
}

/// Whether `e` is the refusal of an engine resume [`resume_refusal`] gave,
/// after which the run is ended.
fn ends_the_run(e: &EngineError) -> bool {
    matches!(e, EngineError::Conflict(m) if m.starts_with(DEADLINE_EXCEEDED))
}

/// What follows a refused resume once the event lock is released: an
/// engine resume past the deadline ends the run; a person's refused resume
/// leaves it parked, so its deadline waker is armed (a no-op while one
/// waits) to end it there. Any other refusal changes nothing.
pub fn after_refusal(root: &Path, run_id: &str, e: &EngineError) {
    if ends_the_run(e) {
        let _ = stop_if_expired(root, run_id, Expect::Any);
    } else if matches!(e, EngineError::Conflict(m) if m.starts_with(DEADLINE_PASSED)) {
        arm_waker(root, run_id);
    }
}

/// The deadline a resume grants: `grant_ms` more than the clock reads now.
pub fn grant(events: &[Event], grant_ms: u64, now_ms: u128) -> RunDeadline {
    let elapsed =
        u64::try_from(crate::pause::clock::deadline_elapsed_ms(events, now_ms)).unwrap_or(u64::MAX);
    RunDeadline {
        total_ms: elapsed.saturating_add(grant_ms),
        source: DeadlineSource::Resume,
    }
}

/// Arms the deadline waker of a run that parks with a deadline in force
/// (a no-op while its lease is held, or without a deadline, or for a
/// person's pause, which freezes the clock). Every park site calls it.
pub fn arm_waker(root: &Path, run_id: &str) {
    let run_dir = root.join(".apb/runs").join(run_id);
    let Ok(events) = crate::event::read_all_lossy_tail(&run_dir) else {
        return;
    };
    if in_force(&events).is_some()
        && !crate::state::RunState::fold(&events)
            .run_status
            .is_terminal()
        && crate::pause::clock::paused_by(&events) != Some(PauseBy::Operator)
    {
        crate::waker::spawn(root, run_id, crate::waker::WakerFor::Deadline);
    }
}

/// The supervisor-patch rule: a patch may not add, drop or change
/// `deadline` (the run's contract; only a person changes it, at a resume).
pub fn patch_refusal(old: &Playbook, new: &Playbook) -> Option<String> {
    (old.deadline != new.deadline).then(|| {
        "deadline_changed: the run's `deadline` is not a supervisor's to change; a person grants more time at a resume (`apb resume --deadline`)".to_string()
    })
}

/// Barriers the deadline race tests hold the engine at. Test builds only:
/// without the `testing` feature none of this exists.
#[cfg(any(test, feature = "testing"))]
pub use crate::deadline_hooks as test_hooks;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventPayload;

    const MIN: u128 = 60_000;

    fn ev(seq: u64, ts: u128, payload: EventPayload) -> Event {
        Event { seq, ts, payload }
    }

    fn started(ts: u128) -> Event {
        ev(
            0,
            ts,
            EventPayload::RunStarted {
                playbook: "p".into(),
                version: "1.0.0".into(),
                definition: None,
                external_data: false,
            },
        )
    }

    fn provenance(d: Option<RunDeadline>) -> Event {
        let mut payload: serde_json::Value = serde_json::json!({"type": "run_provenance"});
        if let Some(d) = d {
            payload["deadline"] = serde_json::to_value(d).unwrap();
        }
        ev(1, 1, serde_json::from_value(payload).unwrap())
    }

    fn resumed_with(seq: u64, ts: u128, d: Option<RunDeadline>) -> Event {
        let mut p = EventPayload::resumed("a", Some(PauseBy::Operator), false);
        if let EventPayload::RunResumed { deadline, .. } = &mut p {
            *deadline = d;
        }
        ev(seq, ts, p)
    }

    fn d(total_ms: u64, source: DeadlineSource) -> RunDeadline {
        RunDeadline { total_ms, source }
    }

    #[test]
    fn the_latest_deadline_is_in_force() {
        let at_start = d(30 * 60_000, DeadlineSource::Playbook);
        let grant = d(50 * 60_000, DeadlineSource::Resume);
        let mut events = vec![started(0), provenance(Some(at_start))];
        assert_eq!(in_force(&events), Some(at_start));
        events.push(resumed_with(2, 5, None));
        assert_eq!(in_force(&events), Some(at_start), "a plain resume keeps it");
        events.push(resumed_with(3, 6, Some(grant)));
        assert_eq!(in_force(&events), Some(grant));
        assert_eq!(in_force(&[started(0), provenance(None)]), None);
    }

    #[test]
    fn remaining_and_expired_follow_the_clock() {
        let events = vec![
            started(0),
            provenance(Some(d(30 * 60_000, DeadlineSource::Run))),
        ];
        assert_eq!(remaining_ms(&events, 10 * MIN), Some(20 * 60_000));
        assert!(!expired(&events, 10 * MIN));
        assert_eq!(remaining_ms(&events, 40 * MIN), Some(0));
        assert!(expired(&events, 30 * MIN));
        let none = vec![started(0), provenance(None)];
        assert_eq!(remaining_ms(&none, 40 * MIN), None);
        assert!(!expired(&none, 40 * MIN));
    }

    /// A grant is the time to add to the elapsed time: deadline 30 min,
    /// elapsed 40, `--deadline 10m` gives a total of 50 and 10 left.
    #[test]
    fn a_grant_adds_to_the_elapsed_time() {
        let mut events = vec![
            started(0),
            provenance(Some(d(30 * 60_000, DeadlineSource::Run))),
        ];
        let elapsed = crate::pause::clock::deadline_elapsed_ms(&events, 40 * MIN);
        let total = u64::try_from(elapsed).unwrap() + 10 * 60_000;
        assert_eq!(total, 50 * 60_000);
        events.push(resumed_with(
            2,
            40 * MIN,
            Some(d(total, DeadlineSource::Resume)),
        ));
        assert_eq!(remaining_ms(&events, 40 * MIN), Some(10 * 60_000));
    }

    #[test]
    fn the_reason_names_the_deadline() {
        assert_eq!(
            reason(&d(45 * 60_000, DeadlineSource::Playbook)),
            "deadline_exceeded: the run passed its deadline of 45m"
        );
    }

    #[test]
    fn a_patch_may_not_touch_the_deadline() {
        let pb = |dl: &str| {
            Playbook::from_yaml(&format!(
                "schema: 2\nid: p\nname: p\nversion: 1.0.0\n{dl}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n"
            ))
            .unwrap()
        };
        let (none, a, b) = (pb(""), pb("deadline: 45m\n"), pb("deadline: 2h\n"));
        assert_eq!(patch_refusal(&a, &a), None);
        assert_eq!(patch_refusal(&none, &none), None);
        for (x, y) in [(&none, &a), (&a, &none), (&a, &b)] {
            assert!(patch_refusal(x, y).unwrap().starts_with("deadline_changed"));
        }
    }

    #[test]
    fn the_playbook_deadline_yields_to_a_run_argument() {
        let pb = Playbook::from_yaml(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndeadline: 45m\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
        )
        .unwrap();
        assert_eq!(
            at_start(&pb, None),
            Some(d(45 * 60_000, DeadlineSource::Playbook))
        );
        assert_eq!(
            at_start(&pb, Some(5_000)),
            Some(d(5_000, DeadlineSource::Run))
        );
    }
}
