//! Stopping a run for real.
//!
//! Posting `Control::Abort` to `runs/<id>/control.jsonl` used to be the whole
//! story, and it was not enough in two ways:
//!
//!   * The drive loop only reads control at the boundary BETWEEN nodes, so an
//!     abort could not touch an agent that was already running. A supervisor
//!     watching an agent burn through a doomed retry loop had no way to stop
//!     it short of killing the driver process. `AbortWatcher` fixes that: every
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

use std::path::Path;
use std::process::Command;
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

    if driver_is_live(&run_dir, run_id) {
        return Ok(StopOutcome::SignaledLiveDriver);
    }

    // Nothing is driving this run. Re-read the journal before writing to it:
    // the drive loop does not take this lock, and it appends its terminal
    // event just BEFORE dropping `driver.pid`. A stop that read the journal
    // ahead of that append and the pid file after the removal would otherwise
    // stamp a redundant `RunAborted` onto a run that had in fact just
    // finished cleanly.
    if is_terminal(RunState::fold(&read_all(&run_dir)?).run_status) {
        // The abort we just posted has nothing left to do. Mark it consumed so
        // a later resume of this run does not trip over a stale stop command.
        write_control_cursor(&run_dir, seq)?;
        return Ok(StopOutcome::AlreadyTerminal);
    }

    // Apply the abort ourselves: effect first (the terminal event), then the
    // cursor, so a failed append leaves the command unconsumed rather than
    // silently dropped - the same ordering the drive loop uses.
    let mut log = EventLog::open(&run_dir)?;
    log.append(EventPayload::RunAborted {
        reason: STOP_REASON.into(),
    })?;
    write_control_cursor(&run_dir, seq)?;
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

/// Is a process really driving this run right now?
///
/// `driver.pid` alone cannot answer this. Drivers lead their own process group
/// and are reaped promptly, so their pids are released and REUSED: a bare
/// `kill -0` would happily succeed for a completely unrelated process that
/// inherited the number, and we would leave a dead run unfinalized forever.
///
/// The disambiguator is free: a detached driver's argv carries
/// `--run-id <id>`. Around that definitive signal the rule stays biased toward
/// "live", because a wrong "dead" is the worse error - it appends a terminal
/// event while real work is still going on, whereas a wrong "live" only leaves
/// a run that a later stop (or `apb doctor`) can still finalize.
fn driver_is_live(run_dir: &Path, run_id: &str) -> bool {
    let Some(pid) = crate::driver::read_driver_pid(run_dir) else {
        return false;
    };
    // A drive running on a thread of THIS process (the CLI's synchronous run,
    // the in-process background drive) needs no probing and cannot be a
    // reused pid.
    if pid == std::process::id() {
        return true;
    }
    let argv = match process_probe(pid) {
        // The probe worked and the process is gone (or is a zombie, which
        // holds a pid but drives nothing). The only branch that may conclude
        // "dead" without knowing what the process is.
        Probe::NotFound => return false,
        // The probe itself did not work: no `ps` on this host, an unreadable
        // answer. We know nothing, so we assume the driver is alive - see the
        // bias documented above. Concluding "dead" here would let a host
        // without `ps` write a terminal event over every live run.
        Probe::Unknown => return true,
        Probe::Running(argv) => argv,
    };
    if driver_argv_names_run(&argv, run_id) {
        // Definitive: this process is the detached driver of this very run.
        return true;
    }
    if argv.split_whitespace().any(|t| t == "__drive-run") {
        // A driver, but of some other run: our pid was reused.
        return false;
    }
    // Not a detached driver. Any other `apb` process may still be driving this
    // run on a thread (`apb run`, `apb mcp`), so we do not finalize behind its
    // back. Anything that is not `apb` at all is a reused pid.
    argv_program_is_apb(&argv)
}

/// Does this command line belong to the detached driver of `run_id`?
///
/// Compared token by token rather than as a substring: `--run-id stopflow-12`
/// is a prefix of `--run-id stopflow-123456`, so a substring test would let a
/// stop of the short run mistake the long run's driver for its own and report
/// a live driver that is in fact driving something else. Both the
/// `--run-id X` and `--run-id=X` spellings are accepted; the engine only ever
/// spawns the former, but reading argv is a loose contract.
fn driver_argv_names_run(argv: &str, run_id: &str) -> bool {
    let mut tokens = argv.split_whitespace();
    while let Some(tok) = tokens.next() {
        if tok == "--run-id" {
            return tokens.next() == Some(run_id);
        }
        if let Some(value) = tok.strip_prefix("--run-id=") {
            return value == run_id;
        }
    }
    false
}

/// The answer to "what is process `pid`?". Deliberately three-valued: "the
/// probe says there is no such process" and "the probe could not tell us
/// anything" are very different facts, and collapsing them into `None` is what
/// would make a missing `ps` read as a dead driver.
enum Probe {
    Running(String),
    NotFound,
    Unknown,
}

fn process_probe(pid: u32) -> Probe {
    let out = match Command::new("ps")
        .args(["-o", "stat=", "-o", "args=", "-p", &pid.to_string()])
        .output()
    {
        Ok(out) => out,
        // `ps` is missing or could not be spawned at all.
        Err(_) => return Probe::Unknown,
    };
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() {
        // The ordinary "no such process" answer: a non-zero exit with nothing
        // on stdout (the diagnostic goes to stderr). A non-zero exit that DID
        // print something is some other failure, and we do not guess.
        return if line.is_empty() {
            Probe::NotFound
        } else {
            Probe::Unknown
        };
    }
    // A successful probe that printed nothing also means no such process.
    if line.is_empty() {
        return Probe::NotFound;
    }
    let Some((stat, argv)) = line.split_once(char::is_whitespace) else {
        // Output we cannot parse: unknown, not dead.
        return Probe::Unknown;
    };
    if stat.starts_with('Z') {
        return Probe::NotFound;
    }
    Probe::Running(argv.trim().to_string())
}

fn argv_program_is_apb(argv: &str) -> bool {
    argv.split_whitespace()
        .next()
        .and_then(|p| p.rsplit('/').next())
        .is_some_and(|name| name == "apb" || name.starts_with("apb-") || name.starts_with("apb."))
}

/// How long the watcher waits between control.jsonl reads. Fast enough that an
/// operator perceives the stop as immediate, slow enough that a long run costs
/// a handful of file reads per second.
const WATCH_INTERVAL: Duration = Duration::from_millis(200);
/// The interval is slept in slices this size so a finishing drive never waits
/// a full interval for the watcher to notice it should stop.
const WATCH_SLICE: Duration = Duration::from_millis(25);

/// Watches `control.jsonl` for a pending `Control::Abort` while a drive is in
/// progress and, on seeing one, sets the drive's cancel flag - which is what
/// kills the agent process tree mid-node.
///
/// The watcher OBSERVES only. It does not consume the entry and never writes
/// the control cursor: the drive loop still applies the abort and owns the
/// cursor, so the abort takes effect exactly once.
///
/// It cannot outlive the drive: the guard is dropped when `drive` returns, and
/// dropping it stops and joins the thread (bounded by one `WATCH_SLICE`).
pub(crate) struct AbortWatcher {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl AbortWatcher {
    /// `after` is the drive's starting control cursor: entries at or below it
    /// have already been applied by an earlier drive and must not re-fire.
    pub(crate) fn spawn(run_dir: &Path, after: Option<u64>, cancel: Arc<AtomicBool>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_c = Arc::clone(&stop);
        let dir = run_dir.to_path_buf();
        let handle = std::thread::spawn(move || {
            loop {
                if stop_c.load(Ordering::Relaxed) {
                    return;
                }
                // A read error (a torn line being appended right now) is not
                // fatal: the next poll re-reads the file.
                if let Ok(entries) = read_control_after(&dir, after)
                    && entries
                        .iter()
                        .any(|e| matches!(e.cmd, Control::Abort { .. }))
                {
                    cancel.store(true, Ordering::SeqCst);
                    // The flag is sticky for the rest of the drive and the
                    // drive loop finalizes from here, so there is nothing left
                    // to watch.
                    return;
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
        }
    }
}

impl Drop for AbortWatcher {
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

    #[test]
    fn only_an_apb_program_counts_as_a_possible_driver() {
        assert!(argv_program_is_apb("/usr/local/bin/apb run demo"));
        assert!(argv_program_is_apb("apb mcp"));
        assert!(!argv_program_is_apb("/bin/zsh -l"));
        assert!(!argv_program_is_apb("apbx run demo"));
        assert!(!argv_program_is_apb(""));
    }

    #[test]
    fn the_current_process_is_always_a_live_driver() {
        let dir = tempfile::tempdir().unwrap();
        apb_core::fsutil::atomic_write(
            &crate::driver::driver_pid_path(dir.path()),
            std::process::id().to_string().as_bytes(),
        )
        .unwrap();
        assert!(driver_is_live(dir.path(), "any-run"));
    }

    #[test]
    fn run_id_is_matched_as_a_token_not_a_prefix() {
        assert!(driver_argv_names_run(
            "/usr/bin/apb __drive-run --root /w --run-id stopflow-12",
            "stopflow-12"
        ));
        assert!(driver_argv_names_run(
            "/usr/bin/apb __drive-run --run-id=stopflow-12 --resume",
            "stopflow-12"
        ));
        // The bug this guards: a prefix must not pass for the longer id.
        assert!(!driver_argv_names_run(
            "/usr/bin/apb __drive-run --root /w --run-id stopflow-123456",
            "stopflow-12"
        ));
        assert!(!driver_argv_names_run("/usr/bin/apb mcp", "stopflow-12"));
    }

    #[test]
    fn a_probe_that_cannot_answer_never_concludes_dead() {
        // A live pid this test certainly owns.
        assert!(matches!(
            process_probe(std::process::id()),
            Probe::Running(_)
        ));
        // `ps` answering "no such process" is the one case that means dead.
        // Pid 0 is never a normal user process on unix.
        assert!(matches!(process_probe(u32::MAX), Probe::NotFound));
    }

    #[test]
    fn a_missing_pid_file_means_no_driver() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!driver_is_live(dir.path(), "any-run"));
    }
}
