//! Stopping a run for real.
//!
//! Posting `Control::Abort` to `runs/<id>/control.jsonl` used to be the whole
//! story, and it was not enough in two ways:
//!
//!   * The drive loop only reads control at the boundary BETWEEN nodes, so an
//!     abort could not touch an agent that was already running. A supervisor
//!     watching an agent burn through a doomed retry loop had no way to stop
//!     it short of killing the driver process. `StopWatcher` fixes that: every
//!     drive spawns one, it polls control.jsonl a few times a second, and on a
//!     pending Abort it sets the run-level cancel flag that `run_cancellable`
//!     already honors - which kills the in-flight agent's process tree. The
//!     drive loop then applies the Abort at the boundary exactly as it always
//!     has. The watcher NEVER touches the persisted control cursor: cursor
//!     advancement is effect-first and drive-owned, and the abort is applied
//!     once, by the drive loop.
//!
//!   * A run whose driver has crashed reads `running` forever, because the
//!     only thing that ever writes a terminal event is the drive loop that no
//!     longer exists. `stop_run` closes that hole: when nothing is driving the
//!     run any more, it appends `RunAborted` itself.
//!
//! `StopWatcher` produces two outputs, and they are deliberately two separate
//! flags rather than one:
//!
//!   * `cancel` - set on Abort only. It is the kill signal (`run_cancellable`,
//!     `run_script` and `wait_backoff` all poll it to tear down in-flight
//!     work) and it is also read by `control_apply.rs` as "an Abort is
//!     pending": seeing it set there appends `RunAborted`. Both readings
//!     require the flag to mean Abort and only Abort.
//!   * `halt` - set on Abort OR Pause. It means "admit no new work" (stop
//!     picking up further chunks of a batch) without claiming the run is
//!     aborting. A Pause must never touch `cancel`: latching it on a pause
//!     would make `control_apply` treat a pause as an abort-in-progress and
//!     terminate the run, which is exactly the bug a single shared flag would
//!     cause.
//!
//! A batch member cannot poll `cancel`/`halt` directly: it only ever sees a
//! batch-local `&AtomicBool` (handed down as a bare pointer into
//! `adapter.run_cancellable`, `run_script` and `wait_backoff`, four agent
//! impls, `proc.rs` and `script.rs`), so the run-level `cancel` has no way to
//! reach it by itself. `CancelFanout` fixes that from the setter's side: every
//! batch-local flag registers itself with the fanout, and `fire()` latches
//! every registered flag through one code path, including a flag registered
//! after the fire already happened.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::control::{Control, post_control, read_control_after, write_control_cursor};
use crate::error::EngineError;
use crate::event::{EventLog, EventPayload, read_all};
use crate::state::{RunState, RunStatus};

/// The reason recorded for an abort that came through `stop_run`, so an
/// operator reading the journal can tell it from a supervisor abort.
const STOP_REASON: &str = "stop requested";

/// Serializes `stop_run`'s read-check-append over the run journal against
/// another `stop_run` racing it: without it two concurrent stops of the same
/// dead run could both observe a non-terminal state and both append
/// `RunAborted`.
const EVENT_LOCK: &str = "events.jsonl.lock";

/// What a `stop_run` call actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// A live driver owns this run. The abort is posted; that driver's watcher
    /// interrupts the in-flight node and its drive loop writes the terminal
    /// event.
    SignaledLiveDriver,
    /// Nothing was driving the run any more, so this call wrote the terminal
    /// `RunAborted` itself.
    FinalizedDeadRun,
    /// The run was already terminal, so this call wrote no terminal event.
    ///
    /// Note that this does NOT always mean nothing happened. On the common
    /// path the run was terminal on entry and the call is a pure no-op. It is
    /// also returned when the run turned terminal under us - a driver
    /// finalizing between our first journal read and the re-check we do
    /// immediately before finalizing - and on that path the abort has already
    /// been posted to control.jsonl and propagated to sub-playbook children.
    /// What the variant promises is only this: no terminal event was written
    /// by us, because the run already had one.
    AlreadyTerminal,
}

impl StopOutcome {
    /// Stable machine-facing name, for the MCP tool and the CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            StopOutcome::SignaledLiveDriver => "signaled_live_driver",
            StopOutcome::FinalizedDeadRun => "finalized_dead_run",
            StopOutcome::AlreadyTerminal => "already_terminal",
        }
    }
}

fn is_terminal(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Succeeded | RunStatus::Failed | RunStatus::Aborted
    )
}

/// Stops a run: posts `Control::Abort`, and - only when no process is driving
/// the run any more - finalizes it on the spot.
///
/// The two halves are deliberately exclusive. If a driver is alive, IT owns the
/// terminal event: writing one here as well would double-apply the abort and
/// race the driver's own journal writes. If no driver is alive, nobody else
/// ever will, so this call has to do it or the run stays `running` forever.
///
/// A run that is terminal on entry short-circuits before anything is posted.
/// A run that BECOMES terminal while we work returns `AlreadyTerminal` too,
/// but by then the abort has been posted and propagated to children - see the
/// variant's own documentation.
pub fn stop_run(root: &Path, run_id: &str) -> Result<StopOutcome, EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }

    // Best effort, like every other lock_dir caller in the tree: a lock we
    // could not take must not stop an operator from stopping a run.
    let _lock = apb_core::fsutil::lock_dir(&run_dir, EVENT_LOCK).ok();

    if is_terminal(RunState::fold(&read_all(&run_dir)?).run_status) {
        return Ok(StopOutcome::AlreadyTerminal);
    }

    let seq = post_control(
        &run_dir,
        Control::Abort {
            reason: STOP_REASON.into(),
        },
    )?;
    // An operator stop of a parent must reach the children that are blocking
    // it, exactly as `run_cancel` does.
    abort_children(root, run_id)?;

    if crate::liveness::driver_is_live(&run_dir, run_id) {
        return Ok(StopOutcome::SignaledLiveDriver);
    }

    // Nothing is driving this run. Re-read the journal before writing to it:
    // the drive loop does not take this lock, and it appends its terminal
    // event just BEFORE dropping `driver.pid`. A stop that read the journal
    // ahead of that append and the pid file after the removal would otherwise
    // stamp a redundant `RunAborted` onto a run that had in fact just
    // finished cleanly.
    if is_terminal(RunState::fold(&read_all(&run_dir)?).run_status) {
        // The abort we just posted has nothing left to do, and the driver that
        // just finalized this run owns the cursor for everything it applied.
        // Mark our own entry consumed so a later resume of a run that finished
        // on its own does not trip over a stale stop command.
        write_control_cursor(&run_dir, seq)?;
        return Ok(StopOutcome::AlreadyTerminal);
    }

    // Apply the abort ourselves: the terminal event, and NOTHING else.
    //
    // This is the replay option of the scalar-cursor trade-off documented on
    // `write_control_cursor`. Advancing the cursor here would mark every entry
    // queued ahead of our Abort applied, and on this path the driver died
    // without applying any of them - so a crashed driver plus `apb note` plus
    // `apb stop` plus `apb resume` silently lost the note. The replay is
    // self-limiting: the next drive re-reads the abort through the drive
    // loop's own Abort arm, which advances the cursor, so it happens once.
    let mut log = EventLog::open(&run_dir)?;
    log.append(EventPayload::RunAborted {
        reason: STOP_REASON.into(),
    })?;
    Ok(StopOutcome::FinalizedDeadRun)
}

/// Posts Abort to every non-terminal sub-playbook child of `run_id`, recursively
/// (spec C). Best-effort per child; a child that no longer exists is skipped.
/// This is how an operator abort of the parent reaches a child that is blocking
/// the parent (e.g. a child paused on human_review): the child's own drive loop
/// scans its control.jsonl at every iteration boundary and returns Aborted, which
/// the parent maps to a failed node.
pub(crate) fn abort_children(root: &Path, run_id: &str) -> Result<(), EngineError> {
    let run_dir = root.join(".apb/runs").join(run_id);
    let events = read_all(&run_dir)?;
    for e in &events {
        if let EventPayload::ChildRunStarted { run_id: child, .. } = &e.payload {
            let child_dir = root.join(".apb/runs").join(child);
            if child_dir.is_dir() && !is_terminal(RunState::fold(&read_all(&child_dir)?).run_status)
            {
                // Best-effort per child (a child that raced to terminal or lost
                // its dir must not block the parent abort), but no longer
                // silent: a failed post is logged with the child run id so an
                // operator can tell an un-propagated abort from a clean one
                // (review I7/R1-I9). apb-engine has no tracing facility, so this
                // is an eprintln, matching the progress/snapshot warnings.
                if let Err(e) = crate::control::post_control(
                    &child_dir,
                    Control::Abort {
                        reason: "parent aborted".into(),
                    },
                ) {
                    eprintln!("apb: warning: failed to post abort to child run `{child}`: {e}");
                }
                abort_children(root, child)?;
            }
        }
    }
    Ok(())
}

/// How long the watcher waits between control.jsonl reads. Fast enough that an
/// operator perceives the stop as immediate, slow enough that a long run costs
/// a handful of file reads per second.
const WATCH_INTERVAL: Duration = Duration::from_millis(200);
/// The interval is slept in slices this size so a finishing drive never waits
/// a full interval for the watcher to notice it should stop.
const WATCH_SLICE: Duration = Duration::from_millis(25);

/// Flags that must all latch the moment the run-level stop fires.
///
/// A batch member polls ONE flag, the batch-local `cancel` its siblings share,
/// because that flag travels all the way into `adapter.run_cancellable`,
/// `run_script` and `wait_backoff` as a bare `&AtomicBool`. Rather than ripple a
/// token type through the adapter trait and every agent impl, the SETTER fans
/// out: the watcher fires once and every registered flag latches through one
/// code path.
///
/// Registration is race-free: a flag registered after the stop already fired
/// latches on registration, which is exactly the window a batch formed during
/// an abort would otherwise fall into.
#[derive(Default)]
pub(crate) struct CancelFanout {
    inner: std::sync::Mutex<FanoutInner>,
}

#[derive(Default)]
struct FanoutInner {
    fired: bool,
    flags: Vec<Arc<AtomicBool>>,
}

impl CancelFanout {
    pub(crate) fn register(self: &Arc<Self>, flag: &Arc<AtomicBool>) -> FanoutGuard {
        let mut inner = self.inner.lock().expect("cancel fanout mutex poisoned");
        if inner.fired {
            flag.store(true, Ordering::SeqCst);
        }
        inner.flags.push(Arc::clone(flag));
        drop(inner);
        FanoutGuard {
            fanout: Arc::clone(self),
            flag: Arc::clone(flag),
        }
    }

    fn fire(&self) {
        let mut inner = self.inner.lock().expect("cancel fanout mutex poisoned");
        inner.fired = true;
        for f in &inner.flags {
            f.store(true, Ordering::SeqCst);
        }
    }

    #[cfg(test)]
    fn registered_len(&self) -> usize {
        self.inner
            .lock()
            .expect("cancel fanout mutex poisoned")
            .flags
            .len()
    }
}

/// Unregisters on drop, so a long run does not accumulate one dead flag per
/// batch.
pub(crate) struct FanoutGuard {
    fanout: Arc<CancelFanout>,
    flag: Arc<AtomicBool>,
}

impl Drop for FanoutGuard {
    fn drop(&mut self) {
        let mut inner = self
            .fanout
            .inner
            .lock()
            .expect("cancel fanout mutex poisoned");
        inner.flags.retain(|f| !Arc::ptr_eq(f, &self.flag));
    }
}

/// Watches `control.jsonl` for a pending `Control::Pause` or `Control::Abort`
/// while a drive is in progress.
///
/// On a pending Pause it sets `halt` and keeps polling - a Pause is not
/// terminal, and an Abort may still arrive later in the same drive. On a
/// pending Abort it sets `halt`, fires the fanout (which latches the run-level
/// `cancel` and every batch-local flag registered with it - which is what
/// kills an in-flight agent's process tree), and returns: an Abort is
/// terminal, so there is nothing left to watch.
///
/// The watcher OBSERVES only. It does not consume the entry and never writes
/// the control cursor: the drive loop still applies the command and owns the
/// cursor, so it takes effect exactly once.
///
/// It cannot outlive the drive: the guard is dropped when `drive` returns, and
/// dropping it stops and joins the thread (bounded by one `WATCH_SLICE`).
pub(crate) struct StopWatcher {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    fanout: Arc<CancelFanout>,
}

impl StopWatcher {
    /// `after` is the drive's starting control cursor: entries at or below it
    /// have already been applied by an earlier drive and must not re-fire.
    /// `cancel` is registered with the returned fanout at construction, so a
    /// batch-local flag registered later through [`Self::fanout`] latches
    /// through the exact same call that latches `cancel` itself.
    pub(crate) fn spawn(
        run_dir: &Path,
        after: Option<u64>,
        cancel: Arc<AtomicBool>,
        halt: Arc<AtomicBool>,
    ) -> Self {
        let fanout = Arc::new(CancelFanout::default());
        let _cancel_guard = fanout.register(&cancel);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_c = Arc::clone(&stop);
        let dir = run_dir.to_path_buf();
        let fanout_thread = Arc::clone(&fanout);
        let handle = std::thread::spawn(move || {
            // Keeps `cancel` registered with the fanout for the life of the
            // watcher thread; the drop that would unregister it never runs
            // until the thread itself exits.
            let _cancel_guard = _cancel_guard;
            loop {
                if stop_c.load(Ordering::Relaxed) {
                    return;
                }
                // A read error (a torn line being appended right now) is not
                // fatal: the next poll re-reads the file.
                if let Ok(entries) = read_control_after(&dir, after) {
                    if entries
                        .iter()
                        .any(|e| matches!(e.cmd, Control::Abort { .. }))
                    {
                        halt.store(true, Ordering::SeqCst);
                        fanout_thread.fire();
                        // Abort is terminal for the drive; the drive loop
                        // finalizes from here, so there is nothing left to
                        // watch.
                        return;
                    }
                    if entries.iter().any(|e| matches!(e.cmd, Control::Pause)) {
                        halt.store(true, Ordering::SeqCst);
                        // A Pause is not terminal: keep polling, because an
                        // Abort may still be posted later in the same drive.
                    }
                }
                let mut slept = Duration::ZERO;
                while slept < WATCH_INTERVAL {
                    if stop_c.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(WATCH_SLICE);
                    slept += WATCH_SLICE;
                }
            }
        });
        Self {
            stop,
            handle: Some(handle),
            fanout,
        }
    }

    /// The fanout the watcher fires on Abort. A batch registers its
    /// batch-local cancel flag with it so a run-level Abort reaches
    /// in-flight batch members too.
    pub(crate) fn fanout(&self) -> Arc<CancelFanout> {
        Arc::clone(&self.fanout)
    }
}

impl Drop for StopWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_outcome_names_are_stable() {
        assert_eq!(
            StopOutcome::SignaledLiveDriver.as_str(),
            "signaled_live_driver"
        );
        assert_eq!(StopOutcome::FinalizedDeadRun.as_str(), "finalized_dead_run");
        assert_eq!(StopOutcome::AlreadyTerminal.as_str(), "already_terminal");
    }

    /// The race the registry closes: a batch created AFTER the abort already
    /// fired must still see the cancel. Registration latches an
    /// already-fired fanout under the same lock that fires it, so there is no
    /// window in which a fresh batch-local flag reads false. Pure, no timing.
    #[test]
    fn a_flag_registered_after_the_stop_fired_latches_on_registration() {
        let fanout = Arc::new(CancelFanout::default());
        let early = Arc::new(AtomicBool::new(false));
        let _g1 = fanout.register(&early);
        fanout.fire();
        assert!(
            early.load(Ordering::SeqCst),
            "a registered flag latches on fire"
        );

        let late = Arc::new(AtomicBool::new(false));
        let _g2 = fanout.register(&late);
        assert!(
            late.load(Ordering::SeqCst),
            "a flag registered after the fire must latch on registration"
        );
    }

    /// A long run must not accumulate one dead flag per batch.
    #[test]
    fn dropping_the_guard_unregisters_the_flag() {
        let fanout = Arc::new(CancelFanout::default());
        let gone = Arc::new(AtomicBool::new(false));
        {
            let _g = fanout.register(&gone);
        }
        assert_eq!(fanout.registered_len(), 0, "the guard unregisters on drop");
        fanout.fire();
        assert!(
            !gone.load(Ordering::SeqCst),
            "an unregistered flag is not touched by a later fire"
        );
    }
}
