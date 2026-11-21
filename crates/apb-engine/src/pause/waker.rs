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

pub(crate) fn poll() -> Duration {
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
    // Whether the pause this waker waits on is a park (design D2 5), read
    // from the journal the loop reads; computed again whenever the loop
    // reads the journal, so a quota pause's re-pause under a new seq is
    // judged by its own cause.
    let mut read_error_logged = false;
    // What the waker's last read of the parked inbox wait's account log
    // left behind (inbox-waits D4 "Parking" item 6): a failed read is
    // never cached, and a successful read of an absent log is cached like
    // any other. The first poll always reads.
    let mut inbox_state = crate::inbox_wait::ReadState::NotRead;
    loop {
        if !run_dir.is_dir() {
            return Ok(WakeOutcome::Superseded);
        }
        let events = read_all(&run_dir)?;
        let Some(until) = same_pause_until(&events, seq) else {
            return Ok(WakeOutcome::Superseded);
        };
        // The waker of a park (design D7): on every poll, a pending control
        // entry wakes the run at once, once. The check only asks whether an
        // entry is pending; it interprets, applies and moves nothing, and
        // the resumed drive's first scan applies the entries in order. A
        // read that fails (a line still being written, an I/O error) counts
        // as nothing pending for this poll, is retried at the next one, and
        // is logged once.
        if pause_at_is_wait(&events, seq) {
            let pending = match crate::control::read_control_cursor(&run_dir)
                .and_then(|cursor| crate::control::read_control_after(&run_dir, cursor))
            {
                Ok(entries) => !entries.is_empty(),
                Err(e) => {
                    if !read_error_logged {
                        eprintln!(
                            "apb: warning: the park's waker could not read the run's control file: {e}"
                        );
                        read_error_logged = true;
                    }
                    false
                }
            };
            if pending {
                let woke = resume_while(root, run_id, WakeHold::Pause(seq));
                drop(lease);
                return woke;
            }
            // A parked inbox wait wakes on a delivery, too (inbox-waits D4
            // "Parking" item 6): an event of a listed type above the run's
            // journaled position, or a gap, resumes the run at once,
            // through the same resume the pending control entry takes. The
            // check reads and judges; the resumed drive authorizes, reads,
            // decides, and parks again when nothing matches.
            if crate::inbox_wait::pending::delivery_pending(
                root,
                &run_dir,
                &events,
                seq,
                &mut inbox_state,
            ) {
                let woke = resume_while(root, run_id, WakeHold::Pause(seq));
                drop(lease);
                return woke;
            }
        }
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
            // A run past its deadline is ended, not probed: no paid probe
            // runs for a run that is over.
            if crate::run_deadline::stop_if_expired(
                root,
                run_id,
                crate::run_deadline::Expect::Parked(WakeHold::Pause(seq)),
            )? {
                return Ok(WakeOutcome::Superseded);
            }
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
        #[cfg(any(test, feature = "testing"))]
        polls::completed(&run_dir);
        sleep_toward(until, now);
    }
}

/// The polls a pause waker completed per run directory, for the tests to
/// wait on one instead of sleeping.
#[cfg(any(test, feature = "testing"))]
pub mod polls {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};

    fn counts() -> &'static Mutex<HashMap<PathBuf, u64>> {
        static COUNTS: OnceLock<Mutex<HashMap<PathBuf, u64>>> = OnceLock::new();
        COUNTS.get_or_init(Mutex::default)
    }

    pub(super) fn completed(run_dir: &Path) {
        let mut counts = counts().lock().unwrap_or_else(|e| e.into_inner());
        *counts.entry(run_dir.to_path_buf()).or_default() += 1;
    }

    /// The polls the waker of the run at `run_dir` completed so far.
    pub fn count(run_dir: &Path) -> u64 {
        counts()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(run_dir)
            .copied()
            .unwrap_or(0)
    }
}

/// Whether the `run_paused` at journal sequence `seq` is a park (design
/// D2 5): an engine pause whose reason is a wait reason, read from the
/// journal the waker already read.
fn pause_at_is_wait(events: &[crate::event::Event], seq: u64) -> bool {
    events
        .iter()
        .find(|e| e.seq == seq)
        .is_some_and(|e| match &e.payload {
            crate::event::EventPayload::RunPaused { reason, by, .. } => {
                super::cause::is_park_event(reason, *by)
            }
            _ => false,
        })
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

/// The deadline waker's body (`apb __pause-waker --deadline`): holds the
/// lease `deadline` and polls the journal. The park is the next value of
/// [`super::clock::parked`], so a park replaced while it waits (a quota
/// probe's re-pause, a supervisor's pause over the engine's) needs no new
/// waker. It ends when the run is terminal, when a live driver holds it
/// (that drive has its own watch), when the park is a person's pause (the
/// clock is frozen), or once it ended the run at its deadline.
pub fn run_deadline_waker(root: &Path, run_id: &str) {
    let run_dir = root.join(".apb/runs").join(run_id);
    let Ok(Some(_lease)) =
        crate::waker::Lease::try_take(&run_dir, crate::waker::WakerFor::Deadline)
    else {
        return;
    };
    crate::waker::acknowledge_held();
    let poll = poll();
    loop {
        let Ok(events) = crate::event::read_all_lossy_tail(&run_dir) else {
            return;
        };
        if crate::state::RunState::fold(&events)
            .run_status
            .is_terminal()
            || crate::run_deadline::in_force(&events).is_none()
            || crate::liveness::driver_alive(&run_dir, run_id) == Some(true)
            || super::clock::paused_by(&events) == Some(super::kind::PauseBy::Operator)
        {
            return;
        }
        let Some(hold) = super::clock::parked(&events) else {
            // Neither driven nor parked (a resume about to start its
            // driver): look again.
            std::thread::sleep(poll);
            continue;
        };
        let now = apb_core::clock::now_ms();
        // Not due under the lock (replaced meanwhile) or the lock was busy:
        // look again.
        if crate::run_deadline::expired(&events, now)
            && matches!(
                crate::run_deadline::stop_if_expired(
                    root,
                    run_id,
                    crate::run_deadline::Expect::Parked(hold),
                ),
                Ok(true)
            )
        {
            return;
        }
        let left = crate::run_deadline::remaining_ms(&events, now).unwrap_or(0);
        std::thread::sleep(poll.min(std::time::Duration::from_millis(left.saturating_add(20))));
    }
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
                external_data: false,
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

    /// A parked inbox wait wakes on a delivery before `until` (inbox-waits
    /// D4 "Parking" item 6): an event stored above the run's journaled
    /// position makes the waker's next poll resume the run. (The resumed
    /// drive itself runs in the driving process: a test binary cannot
    /// re-exec `__drive-run`, so the waker's spawn is a child that exits
    /// at once; the wake here is the engine's own in-process resume, as in
    /// the suite's park tests.)
    #[test]
    fn an_inbox_park_waker_resumes_on_a_new_event() {
        crate::inbox_wait::park_fixture::in_child(
            crate::inbox_wait::park_fixture::WAKER_PREFIX,
            "an_inbox_park_waker_resumes_on_a_new_event",
            || {
                // SAFETY: the fixture runs alone in a plain child process.
                unsafe { std::env::set_var(POLL_ENV, "50") };
                let fx = crate::inbox_wait::park_fixture::Fx::parked("", 0);
                let seq = crate::inbox_wait::park_fixture::pause_seq(&fx);
                let events = crate::inbox_wait::park_fixture::events(&fx);
                let until = events
                    .iter()
                    .find_map(|e| match &e.payload {
                        EventPayload::RunPaused { until, .. } => *until,
                        _ => None,
                    })
                    .expect("the fixture parks with an until");
                // The waker waits for a poll that began after this call.
                let target = polls::count(&fx.run_dir) + 2;
                let woke = std::thread::spawn({
                    let root = fx.root.clone();
                    let run_id = crate::inbox_wait::park_fixture::RUN.to_string();
                    move || run_waker(&root, &run_id, seq)
                });
                // The waker's first poll finds nothing (the log is empty).
                // Bounded, and a waker that ended early reports its own
                // result instead of hiding behind an outer timeout.
                let deadline = std::time::Instant::now() + Duration::from_secs(30);
                while polls::count(&fx.run_dir) < target {
                    if woke.is_finished() {
                        panic!(
                            "the waker ended before its polls: {:?}",
                            woke.join().expect("the waker thread ends")
                        );
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the waker did not poll twice in 30 s"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                // A delivery lands while the run is parked: the waker
                // resumes at once, well before `until`.
                fx.store(serde_json::json!({ "from": "x" }));
                let outcome = woke
                    .join()
                    .expect("the waker thread ends")
                    .expect("the waker returns");
                assert!(
                    matches!(outcome, WakeOutcome::Resumed { .. }),
                    "the delivery resumed the run: {outcome:?}"
                );
                assert!(
                    apb_core::clock::now_ms() < u128::from(until),
                    "well before the park's until"
                );
            },
        );
    }
}
