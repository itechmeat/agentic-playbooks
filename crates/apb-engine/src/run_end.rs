//! What an ending run leaves for later runs and people: its known problems
//! and its corrections. One entry point every end path calls; best effort,
//! a fault is reported on stderr and changes nothing about the run.

use std::path::Path;

/// Records what the run at `run_dir` left as it ended.
pub(crate) fn record(run_dir: &Path) {
    // A run that may skip ran from a read-only scripts copy; an ended run
    // gives the write bits back, so removing it needs nothing special. A
    // resume takes the read-only copy again.
    if let Err(e) = crate::run_scripts::unprotect(run_dir) {
        eprintln!("apb: could not give the run's scripts copy its write bits back: {e}");
    }
    crate::known_problems::observe_ended(run_dir);
    let root = crate::known_problems::root_of(run_dir);
    if let Err(e) = crate::corrections::observe(&root, run_dir) {
        eprintln!("apb: could not record the run's corrections: {e}");
    }
}
