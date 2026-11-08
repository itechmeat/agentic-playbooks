//! What an ending run leaves for later runs and people: its known problems
//! and its corrections. One entry point every end path calls; best effort,
//! a fault is reported on stderr and changes nothing about the run.

use std::path::Path;

/// Records what the run at `run_dir` left as it ended.
pub(crate) fn record(run_dir: &Path) {
    crate::known_problems::observe_ended(run_dir);
    let root = crate::known_problems::root_of(run_dir);
    if let Err(e) = crate::corrections::observe(&root, run_dir) {
        eprintln!("apb: could not record the run's corrections: {e}");
    }
}
