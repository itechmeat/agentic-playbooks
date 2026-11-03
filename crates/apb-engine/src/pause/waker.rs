//! The waker of a paused run with an `until` (issue #200, design 4.2).
//!
//! A paused run holds no driver, so nothing would ever resume it at `until`.
//! The driver that journals such a pause (or `request_pause`, for a run
//! nothing drives) spawns a small detached `apb __pause-waker` process. It
//! holds no lock and runs no node: it sleeps until `until`, and then resumes
//! the run only when the run is still paused by the very `run_paused` it was
//! spawned for (journal sequence match). An earlier `apb resume`, a newer
//! pause or a stop makes it exit without doing anything. A reboot kills it;
//! `apb resume --all` covers that case ([`super::resume_all`]).
//!
//! The same waker serves a released run (`drive_released`, a drive that had
//! nothing left but deferred retries): with `--release <seq>` it sleeps
//! until the release's `wake_at_ms` and resumes the run only while that
//! release still holds it. It probes nothing.
//!
//! Spawning, and the lease that keeps one waker per wait, are shared with
//! the notify waker in [`crate::waker`].

use std::path::Path;
use std::time::Duration;

use super::WakeHold;
use crate::error::EngineError;
use crate::event::read_all;
use crate::state::{RunState, RunStatus};
use crate::waker::{Lease, WakerFor};

/// Milliseconds between the waker's journal reads while it sleeps: how fast
/// it notices that the run was resumed, paused again or stopped. Tests
/// shorten it.
pub const POLL_ENV: &str = "APB_WAKER_POLL_MS";

const DEFAULT_POLL: Duration = Duration::from_secs(5);

/// How a waker ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeOutcome {
    /// `until` passed with the same pause in place: the run was resumed in a
    /// detached driver with this pid.
    Resumed { pid: u32 },
    /// The pause it was spawned for no longer holds the run (resumed early,
    /// paused again, stopped): nothing to do.
    Superseded,
    /// `until` passed but the resume was refused (the run gate, a missing
    /// consent, environment drift): the run stays paused.
    Refused(String),
}

fn poll() -> Duration {
    std::env::var(POLL_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or(DEFAULT_POLL, Duration::from_millis)
}

/// The `until` of the pause that holds the run now, when that pause is the
/// `run_paused` at journal sequence `seq`. `None` when the run is not paused,
/// or paused by another event, or that pause has no `until`.
fn same_pause_until(events: &[crate::event::Event], seq: u64) -> Option<u64> {
    if RunState::fold(events).run_status != RunStatus::Paused {
        return None;
    }
    match latest_pause(events)? {
        (s, _, until) if s == seq => until,
        _ => None,
    }
}

/// The run's latest `run_paused`: its journal sequence, `keep_lock` and
/// `until`.
pub(crate) fn latest_pause(events: &[crate::event::Event]) -> Option<(u64, bool, Option<u64>)> {
    events.iter().rev().find_map(|e| match &e.payload {
        crate::event::EventPayload::RunPaused {
            keep_lock, until, ..
        } => Some((e.seq, *keep_lock, *until)),
        _ => None,
    })
}

/// The engine-side resume of the waker and `apb resume --all`: the run gate
/// a person's `apb resume` stands for (a run directory this machine created,
/// the consent a resume needs), then a detached driver whose `run_resumed`
/// names `engine`. With `hold`, that driver journals `run_resumed` only
/// while that pause or release still holds the run. A gate refusal is
/// `Conflict`; a busy lock is `WorkdirBusy`, so a caller can wait for it
/// instead of giving up.
pub(crate) fn try_engine_resume(
    root: &Path,
    run_id: &str,
    hold: Option<WakeHold>,
    launch: Option<&crate::gate::launch::LaunchGrant>,
) -> Result<u32, EngineError> {
    // Acknowledged: like `apb resume`, the operator who set up the pause or
    // the boot unit is the confirmation; what still refuses is a run
    // directory this machine did not create. The origin stamp binds the run
    // id and the manifest, not the playbook snapshot. Recording the
    // snapshot digest in the journal would add nothing: the journal is a
    // plain file in the same run directory, so whoever can rewrite the
    // snapshot can rewrite that digest too. A person's `apb resume` makes
    // the same check; binding the snapshot belongs in the stamp itself.
    crate::gate::check_resume(root, run_id, true)
        .map_err(|v| EngineError::Conflict(v.to_string()))?;
    match hold {
        Some(hold) => crate::scheduler::resume_detached_held(root, run_id, hold, launch),
        None => crate::scheduler::resume_detached_req(
            root,
            run_id,
            &crate::scheduler::ResumeRequest {
                by: super::PauseBy::Engine,
                launch,
                ..crate::scheduler::ResumeRequest::operator()
            },
        ),
    }
}

/// A refusal of [`try_engine_resume`] as a line for the operator.
pub(crate) fn refusal_text(e: EngineError) -> String {
    match e {
        EngineError::Conflict(why) => why,
        other => other.to_string(),
    }
}

/// The waker's body: sleeps until the pause at journal sequence `seq` of run
/// `run_id` reaches its `until`, then resumes the run (`run_resumed.by` is
/// `engine`) if that pause still holds it. Blocks for as long as the pause
/// lasts. A second waker of the same pause finds its lease held and ends
/// at once (see [`crate::waker`]).
pub fn run_waker(root: &Path, run_id: &str, seq: u64) -> Result<WakeOutcome, EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    let mut seq = seq;
    let Some(mut lease) = take_lease(&run_dir, WakerFor::Pause(seq))? else {
        return Ok(WakeOutcome::Superseded);
    };
    loop {
        if !run_dir.is_dir() {
            return Ok(WakeOutcome::Superseded);
        }
        let Some(until) = same_pause_until(&read_all(&run_dir)?, seq) else {
            return Ok(WakeOutcome::Superseded);
        };
        let now = apb_core::clock::now_ms();
        if now >= u128::from(until) {
            // A quota pause probes its executor first (design 6.6); a probe
            // that hits the quota again re-pauses the run, and this waker
            // goes on waiting for the new pause. A probe whose outcome is not
            // known never leads to a resume: one refused before it ran (the
            // run gate, environment drift) or one whose outcome could not be
            // journaled is the waker's refusal. The paid probe runs once per
            // pause, never again for a journal write, and only under this
            // pause's lease.
            match super::probe::probe_before_wake(root, run_id, seq) {
                Ok(super::WakeProbe::Repaused { seq: next, .. }) => {
                    seq = next;
                    // The new pause is this waker's: its lease moves on.
                    match take_lease(&run_dir, WakerFor::Pause(seq))? {
                        Some(l) => lease = l,
                        None => return Ok(WakeOutcome::Superseded),
                    }
                    continue;
                }
                Ok(super::WakeProbe::Superseded) => return Ok(WakeOutcome::Superseded),
                Ok(
                    super::WakeProbe::NotQuota
                    | super::WakeProbe::Skipped(_)
                    | super::WakeProbe::Passed,
                ) => {}
                Err(e) => return Ok(WakeOutcome::Refused(refusal_text(e))),
            }
            let woke = resume_while(root, run_id, WakeHold::Pause(seq));
            drop(lease);
            return woke;
        }
        sleep_toward(until, now);
    }
}

/// The lease of `wait`, as an engine error when its file cannot be opened.
fn take_lease(run_dir: &Path, wait: WakerFor) -> Result<Option<Lease>, EngineError> {
    if !run_dir.is_dir() {
        return Ok(None);
    }
    let lease = Lease::try_take(run_dir, wait)?;
    if lease.is_some() {
        crate::waker::acknowledge_held();
    }
    Ok(lease)
}

/// The waker of a released run: sleeps until the release at journal
/// sequence `seq` reaches its `wake_at_ms` (at once when one of its waiting
/// retries was expedited), then resumes the run (`run_resumed.by` is
/// `engine`) if that release still holds it. A later drive event (a resume,
/// a pause, a stop, another release) supersedes it.
pub fn run_release_waker(root: &Path, run_id: &str, seq: u64) -> Result<WakeOutcome, EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    let Some(_lease) = take_lease(&run_dir, WakerFor::Release(seq))? else {
        return Ok(WakeOutcome::Superseded);
    };
    loop {
        if !run_dir.is_dir() {
            return Ok(WakeOutcome::Superseded);
        }
        let Some(wake_at) = super::release_wake(&read_all(&run_dir)?, seq) else {
            return Ok(WakeOutcome::Superseded);
        };
        let now = apb_core::clock::now_ms();
        if now >= u128::from(wake_at) {
            return resume_while(root, run_id, WakeHold::Release(seq));
        }
        sleep_toward(wake_at, now);
    }
}

/// Sleeps one poll, or less when `at` comes first.
fn sleep_toward(at: u64, now: u128) {
    let left = Duration::from_millis(u64::try_from(u128::from(at) - now).unwrap_or(u64::MAX));
    std::thread::sleep(left.min(poll()));
}

/// How long a waker that started a driver waits for it to take the run
/// over (journal its `run_resumed`) before it lets its lease go.
const TAKEOVER_WAIT: Duration = Duration::from_secs(60);

/// A waker's resume once its time came: the launch check of the engine's
/// resume, then a detached driver. Another run holding the busy lock of
/// this run's tree is not a refusal: the waker retries until the lock frees
/// or `hold` no longer holds the run (another pause, resume, release or
/// stop superseded it). The caller keeps its lease until the driver took
/// the run over, so a second waker never starts a second driver.
fn resume_while(root: &Path, run_id: &str, hold: WakeHold) -> Result<WakeOutcome, EngineError> {
    let run_dir = root.join(".apb/runs").join(run_id);
    let grant = match crate::resume_launch::check_engine_resume(
        root,
        run_id,
        crate::gate::launch::EngineResume::PauseWaker,
    ) {
        Ok(g) => g,
        Err(e) => return Ok(WakeOutcome::Refused(refusal_text(e))),
    };
    loop {
        if !run_dir.is_dir() || !hold.holds(&read_all(&run_dir)?) {
            return Ok(WakeOutcome::Superseded);
        }
        match try_engine_resume(root, run_id, Some(hold), grant.as_ref()) {
            Ok(pid) => {
                await_takeover(&run_dir, pid, hold);
                return Ok(WakeOutcome::Resumed { pid });
            }
            Err(EngineError::WorkdirBusy(_)) => std::thread::sleep(poll()),
            Err(EngineError::Conflict(why)) => return Ok(WakeOutcome::Refused(why)),
            Err(other) => return Ok(WakeOutcome::Refused(other.to_string())),
        }
    }
}

/// Waits until the driver `pid` took the run over (`hold` no longer holds
/// it), the driver ended, or [`TAKEOVER_WAIT`] passed.
pub(crate) fn await_takeover(run_dir: &Path, pid: u32, hold: WakeHold) {
    let deadline = std::time::Instant::now() + TAKEOVER_WAIT;
    while std::time::Instant::now() < deadline {
        match read_all(run_dir) {
            Ok(events) if hold.holds(&events) => {}
            _ => return,
        }
        if !crate::liveness::pid_alive(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Spawns the detached waker for the pause at journal sequence `seq` of run
/// `run_id`, unless one already waits for it (see [`crate::waker::spawn`]).
pub fn spawn_waker(root: &Path, run_id: &str, seq: u64) -> Option<u32> {
    crate::waker::spawn(root, run_id, WakerFor::Pause(seq))
}

/// Spawns the detached waker of the release at journal sequence `seq` of
/// run `run_id` (`apb __pause-waker --release <seq>`), unless one already
/// waits for it.
pub fn spawn_release_waker(root: &Path, run_id: &str, seq: u64) -> Option<u32> {
    crate::waker::spawn(root, run_id, WakerFor::Release(seq))
}

/// The waker of the wait `hold` of run `run_id`, spawned unless one already
/// holds it; `Ok` only once a waker holds its lease
/// ([`crate::waker::spawn_held`]).
pub fn spawn_held_waker(root: &Path, run_id: &str, hold: WakeHold) -> Result<(), String> {
    crate::waker::spawn_held(root, run_id, hold.into())
}

/// [`spawn_held_waker`] without waiting for the acknowledgment: the caller
/// settles it ([`crate::waker::HeldWaker::settle`]).
pub fn start_held_waker(root: &Path, run_id: &str, hold: WakeHold) -> crate::waker::HeldWaker {
    crate::waker::start_held(root, run_id, hold.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, EventPayload};
    use crate::pause::PauseBy;

    fn ev(seq: u64, payload: EventPayload) -> Event {
        Event {
            seq,
            ts: u128::from(seq),
            payload,
        }
    }

    fn paused(until: Option<u64>) -> EventPayload {
        super::super::PauseRequest {
            until,
            by: Some(PauseBy::Operator),
            ..Default::default()
        }
        .to_event()
    }

    #[test]
    fn the_waker_acts_only_on_its_own_pause() {
        let started = ev(
            0,
            EventPayload::RunStarted {
                playbook: "p".into(),
                version: "1.0.0".into(),
                definition: None,
            },
        );
        let events = vec![started.clone(), ev(1, paused(Some(500)))];
        assert_eq!(same_pause_until(&events, 1), Some(500));
        // Another pause replaced it.
        let newer = vec![
            started.clone(),
            ev(1, paused(Some(500))),
            ev(2, paused(Some(900))),
        ];
        assert_eq!(same_pause_until(&newer, 1), None);
        assert_eq!(same_pause_until(&newer, 2), Some(900));
        // Resumed early.
        let resumed = vec![
            started,
            ev(1, paused(Some(500))),
            ev(
                2,
                EventPayload::resumed("a", Some(PauseBy::Operator), false),
            ),
        ];
        assert_eq!(same_pause_until(&resumed, 1), None);
    }
}
