//! The observer of a run's terminal event (routine health D2). The journal
//! calls [`observe`] from its one append path once a `run_finished` or a
//! `run_aborted` line is in the file and the journal lock is released; the
//! observer itself ([`crate::health::install`]) is registered by the
//! process that drives runs (the `apb` binary at start), so the journal
//! stays a leaf that depends on nothing the health evaluation reads.

use std::path::Path;
use std::sync::OnceLock;

static OBSERVER: OnceLock<fn(&Path)> = OnceLock::new();

/// Registers the observer; the first registration wins.
pub fn install(f: fn(&Path)) {
    let _ = OBSERVER.set(f);
}

/// Calls the observer for `run_dir`, when one is registered.
pub(crate) fn observe(run_dir: &Path) {
    if let Some(f) = OBSERVER.get() {
        f(run_dir);
    }
}
