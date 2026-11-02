//! The retry of a run's last notifications. The drive's last sweep is the
//! last one a driver runs, so a send it saw fail (or refused for a passing
//! reason) would wait for a sweep that never comes. When such a send may be
//! due again, the drive leaves `apb __pause-waker --notify` behind: it
//! sleeps the retry gap and sweeps the run once, for as long as a retry is
//! ahead (each transition has at most [`MAX_TRIES`] tries). No journal state of its own: whether
//! a send is due is a pure function of the send ledger, so a sweep it runs
//! sends nothing twice, and a send the trust gate retired is never retried.
//! A reboot loses the waker; `apb resume --all` sweeps a finished run once.
//! It is spawned, and kept to one per run, by [`crate::waker`].

use std::path::Path;
use std::time::Duration;

use super::send::Notifier;
use super::transitions::{MAX_TRIES, retry_gap_ms};
use crate::error::EngineError;
use crate::event::Event;

/// The notify waker's body: rounds of a sleep of the retry gap and one
/// sweep of run `run_id`, while a retry is ahead. A round that finds a live
/// driver sweeps nothing (the driver sweeps itself) but the waker stays:
/// that driver may end with a send that failed after this check, and it
/// spawns no waker of its own while this one holds the lease. For the same
/// reason a waker that runs out of work looks once more after it let the
/// lease go. [`MAX_TRIES`] sweeps in a row that try nothing end it. Returns
/// how many sweeps it ran.
pub fn run_retry_waker(root: &Path, run_id: &str) -> Result<usize, EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    let notifier = Notifier::new(root, &run_dir, run_id);
    let mut sweeps = 0;
    loop {
        if !run_dir.is_dir() {
            return Ok(sweeps);
        }
        // One notify waker per run (see `crate::waker`): a second one ends
        // here.
        let Some(lease) = crate::waker::Lease::try_take(&run_dir, crate::waker::WakerFor::Notify)?
        else {
            return Ok(sweeps);
        };
        let (ran, seen) = retry_while_ahead(&run_dir, run_id, &notifier);
        sweeps += ran;
        drop(lease);
        // A drive that ended while the lease was held left its retry here:
        // its try is in the journal before its own waker check failed.
        if journal_len(&run_dir) == seen || !notifier.retry_ahead() {
            return Ok(sweeps);
        }
    }
}

/// The rounds of [`run_retry_waker`] under its lease: how many sweeps ran,
/// and the journal size read just before the check that ended them. The
/// size is taken before that check, so a try a drive journals after it (and
/// whose own waker the held lease suppressed) shows up as growth.
fn retry_while_ahead(run_dir: &Path, run_id: &str, notifier: &Notifier) -> (usize, u64) {
    let mut sweeps = 0;
    let mut idle = 0;
    loop {
        let seen = journal_len(run_dir);
        if !(idle < MAX_TRIES && run_dir.is_dir() && notifier.retry_ahead()) {
            return (sweeps, seen);
        }
        std::thread::sleep(Duration::from_millis(retry_gap_ms()));
        if crate::liveness::driver_alive(run_dir, run_id) == Some(true) {
            continue;
        }
        let before = journal_len(run_dir);
        notifier.sweep_now();
        sweeps += 1;
        idle = if journal_len(run_dir) == before {
            idle + 1
        } else {
            0
        };
    }
}

/// The size of the run's journal: it grows with every try.
fn journal_len(run_dir: &Path) -> u64 {
    std::fs::metadata(run_dir.join("events.jsonl")).map_or(0, |m| m.len())
}

/// Whether the drive should leave the notify waker behind for run
/// `run_dir`: a send of its last sweep may be due again.
pub(crate) fn retry_ahead(root: &Path, run_dir: &Path, run_id: &str) -> bool {
    Notifier::new(root, run_dir, run_id).retry_ahead()
}

/// One sweep of finished run `run_dir` as its journal `events` read: what
/// `apb resume --all` does for a run whose notify waker a reboot lost. A
/// live notify waker owns the run's retries (two sweeps deciding from their
/// own reads could spend two tries with no gap between them), so the sweep
/// runs only under the waker's lease. A sweep with nothing due reads
/// nothing else.
pub(crate) fn sweep_finished(root: &Path, run_dir: &Path, run_id: &str, events: &[Event]) {
    let Ok(Some(_lease)) = crate::waker::Lease::try_take(run_dir, crate::waker::WakerFor::Notify)
    else {
        return;
    };
    Notifier::new(root, run_dir, run_id).sweep(events);
}
