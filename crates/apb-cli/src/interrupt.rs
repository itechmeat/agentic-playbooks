//! The operator's interruption of a long command (`apb eval`, the paid
//! agent probe): the first SIGINT or SIGTERM sets [`INTERRUPTED`], which the
//! command passes on as its cancel flag so it can stop what it started and
//! clean up; a second signal ends the process the ordinary way.

use std::sync::atomic::{AtomicBool, Ordering};

/// Set by the first SIGINT or SIGTERM after [`install`].
pub(crate) static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Whether a signal arrived.
pub(crate) fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

#[cfg(unix)]
extern "C" fn on_signal(sig: libc::c_int) {
    // A second signal ends the process the ordinary way.
    if INTERRUPTED.swap(true, Ordering::SeqCst) {
        // SAFETY: signal() and raise() are async-signal-safe.
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }
}

/// Routes SIGINT and SIGTERM to [`INTERRUPTED`]. A child started in its own
/// process group does not get a terminal's Ctrl-C, so the command stops it
/// through the flag.
pub(crate) fn install() {
    #[cfg(unix)]
    // SAFETY: the handler only touches an atomic and async-signal-safe calls.
    unsafe {
        let h = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        libc::signal(libc::SIGINT, h);
        libc::signal(libc::SIGTERM, h);
    }
}
