//! The observers of a run's terminal event (routine health D2,
//! before-running-alone D5). The journal calls [`observe`] from its one
//! append path once a `run_finished` or a `run_aborted` line is in the
//! file and the journal lock is released; the observers themselves
//! ([`crate::health::install`] and [`crate::probation::install`]) are
//! registered by the process that drives runs (the `apb` binary at
//! start), so the journal stays a leaf that depends on nothing the
//! observers read. The health observer runs first and the probation
//! ledger's writer second; they share no file and no lock (the health
//! state and the probation ledger are separate signed files under their
//! own locks), and neither reads what the other writes.

use std::path::Path;
use std::sync::OnceLock;

static OBSERVER: OnceLock<fn(&Path)> = OnceLock::new();
static LEDGER: OnceLock<fn(&Path)> = OnceLock::new();

/// Registers the health observer; the first registration wins.
pub fn install(f: fn(&Path)) {
    let _ = OBSERVER.set(f);
}

/// Registers the probation ledger's writer; the first registration wins.
pub fn install_ledger(f: fn(&Path)) {
    let _ = LEDGER.set(f);
}

/// Calls the health observer first and the ledger writer second, each
/// when registered.
pub(crate) fn observe(run_dir: &Path) {
    if let Some(f) = OBSERVER.get() {
        f(run_dir);
    }
    if let Some(f) = LEDGER.get() {
        f(run_dir);
    }
}
