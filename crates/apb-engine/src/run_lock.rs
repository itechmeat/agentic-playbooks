//! The run's event lock (`events.jsonl.lock`): the lock a stop, a pause,
//! a resume's commit and a prune's removal take on one run directory, and
//! [`in_run`], a write into a run that never lands where the run was.
//! A leaf: it names no other module of the engine but the error type.

use std::path::Path;

use crate::error::EngineError;

/// The name of the run's event lock file. Serializes `stop_run`'s
/// read-check-append over the run journal against another `stop_run`
/// racing it, a resume's commit, and a prune's removal of the run.
pub const EVENT_LOCK: &str = "events.jsonl.lock";

/// Runs `write` on the run at `run_dir` under its event lock, the lock a
/// prune takes before it moves a run away: the run is still in place while
/// `write` runs, and a prune that comes after sees what it wrote. A run
/// that is gone (pruned, or never there) is refused, and nothing is made
/// where it was.
pub fn in_run<R>(
    run_dir: &Path,
    write: impl FnOnce() -> Result<R, EngineError>,
) -> Result<R, EngineError> {
    let gone = || EngineError::NotFound(format!("run `{}`", run_dir.display()));
    let _lock = match apb_core::fsutil::lock_existing_dir(run_dir, EVENT_LOCK) {
        Ok(lock) => lock,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(gone()),
        Err(e) => return Err(e.into()),
    };
    if !run_dir.join("events.jsonl").is_file() {
        return Err(gone());
    }
    write()
}
