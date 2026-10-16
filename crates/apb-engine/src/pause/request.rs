//! Asking for a pause, and what a driver leaves behind when a pause ends its
//! drive (issue #200, design 2, 4 and 8.1).
//!
//! [`request_pause`] is the one entry point every surface calls (`apb pause`,
//! the MCP `run_pause` tool, the dashboard): a run with a live driver gets a
//! `pause` control command that driver applies at its next boundary (drain)
//! or right away (now); a run nothing drives (already paused, or its driver
//! is gone) gets its `run_paused` journaled here, since no driver would ever
//! apply a command.

use std::path::Path;

use crate::control::{Control, post_control};
use crate::error::EngineError;
use crate::event::{EventLog, read_all};
use crate::state::{RunState, RunStatus};
use crate::workdir::WorkdirGuard;

use super::{PauseBy, PauseRequest};

/// What [`request_pause`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PauseOutcome {
    /// A live driver owns the run: the pause is posted to its control queue
    /// (`seq`) and the run reads `pausing` until the driver applies it.
    Posted { seq: u64 },
    /// Nothing drives the run (it was paused already, or its driver is
    /// gone): `run_paused` was journaled here, at journal sequence `seq`.
    Journaled { seq: u64 },
}

impl PauseOutcome {
    /// Stable machine-facing name, for the MCP tool and the CLI.
    pub fn as_str(&self) -> &'static str {
        match self {
            PauseOutcome::Posted { .. } => "posted",
            PauseOutcome::Journaled { .. } => "paused",
        }
    }
}

/// Pauses run `run_id` as `req` asks.
///
/// - A finished, failed or stopped run cannot be paused: an error.
/// - A paused run gets a new `run_paused` with the new reason, mode and
///   `until` (the paused interval keeps its start), and its lock follows the
///   new `keep_lock`.
/// - A run with a live driver (running, or queued for the workdir) gets the
///   control command; a queued run pauses at its first boundary.
/// - A run whose driver is gone is paused on the spot.
///
/// A pause with `until` that this call journals leaves a waker behind
/// ([`super::waker::spawn_waker`]); a posted one gets its waker from the
/// driver that applies it.
pub fn request_pause(
    root: &Path,
    run_id: &str,
    req: &PauseRequest,
) -> Result<PauseOutcome, EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let mut req = req.clone();
    req.by.get_or_insert(PauseBy::Operator);
    // Serialized against `stop_run`, another pause of a driverless run and
    // the engine's resume of a pause. Unlike `stop_run`, which must never be
    // kept from stopping a run, a pause that cannot take the lock refuses:
    // journaled without it, it could race a stop or a resume.
    let _lock = apb_core::fsutil::lock_dir(&run_dir, crate::stop::EVENT_LOCK)?;
    let state = RunState::fold(&read_all(&run_dir)?);
    if state.run_status.is_terminal() {
        return Err(EngineError::Invalid(format!(
            "run `{run_id}` is already {}; only a live or paused run can be paused",
            state.run_status.as_str()
        )));
    }
    if state.run_status != RunStatus::Paused
        && crate::liveness::driver_alive(&run_dir, run_id) == Some(true)
    {
        let seq = post_control(&run_dir, Control::Pause(req))?;
        return Ok(PauseOutcome::Posted { seq });
    }
    // Nothing drives the run: the lock first (a `--keep-lock` that cannot
    // take the lock refuses before anything is journaled), then the event.
    sync_lock(root, &run_dir, run_id, req.keep_lock)?;
    let mut log = EventLog::open(&run_dir)?;
    let seq = log.append(req.to_event())?.seq;
    if req.until.is_some() {
        super::waker::spawn_waker(root, run_id, seq);
    }
    Ok(PauseOutcome::Journaled { seq })
}

/// Makes the busy lock of a run nothing drives follow its pause: parked
/// under the run for `keep_lock`, released otherwise. A run that takes no
/// workdir lock (read-only playbook, sub-playbook child) has nothing to do.
fn sync_lock(
    root: &Path,
    run_dir: &Path,
    run_id: &str,
    keep_lock: bool,
) -> Result<(), EngineError> {
    if !keep_lock {
        crate::workdir::release_paused(root, run_id);
        return Ok(());
    }
    if !takes_workdir_lock(run_dir) {
        return Ok(());
    }
    let tree = RunState::fold(&read_all(run_dir)?)
        .worktree
        .map(std::path::PathBuf::from);
    crate::workdir::park_for(root, tree.as_deref(), run_id)
}

/// Whether the run's snapshot takes the workdir lock (a write-run).
fn takes_workdir_lock(run_dir: &Path) -> bool {
    let is_child = crate::run_config::read_run_config(run_dir)
        .ok()
        .is_some_and(|c| c.parent_run.is_some());
    !is_child
        && std::fs::read_to_string(run_dir.join("playbook.yaml"))
            .ok()
            .and_then(|y| crate::legacy_snapshot::parse_snapshot_playbook(&y).ok())
            .is_some_and(|p| p.nodes.iter().any(|n| n.kind.takes_workdir_lock()))
}

/// Called by the drive when it ends `paused` (issue #200, design 4): the
/// driver exits, so a `--keep-lock` pause parks the busy lock under the run
/// instead of releasing it, and a pause with `until` spawns the waker that
/// resumes the run then. Best effort: a pause is journaled already, and a
/// lock or waker that could not be set up leaves an ordinary paused run.
pub(crate) fn after_pause(
    root: &Path,
    run_dir: &Path,
    run_id: &str,
    lock: &mut Option<WorkdirGuard>,
) {
    let Ok(events) = read_all(run_dir) else {
        return;
    };
    let Some((seq, keep_lock, until)) = super::waker::latest_pause(&events) else {
        return;
    };
    if keep_lock
        && let Some(guard) = lock.take()
        && let Err(e) = guard.park(run_id)
    {
        eprintln!(
            "apb: warning: run `{run_id}` paused with --keep-lock, but its lock could not be kept: {e}"
        );
    }
    if until.is_some() {
        super::waker::spawn_waker(root, run_id, seq);
    }
}
