//! Fault points after each persistent write of a move, for the unit tests
//! that check what a crash at that point leaves and what recovery makes of
//! it. Outside `cfg(test)` every point is a plain `Ok`.

use std::io;

/// The persistent writes of a move, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Intent,
    Supersede,
    Current,
    PointerClear,
    TrialRecord,
    History,
    IntentRemoval,
}

#[cfg(test)]
thread_local! {
    static FAIL_AFTER: std::cell::Cell<Option<Step>> = const { std::cell::Cell::new(None) };
}

/// Makes the move on this thread fail right after `step` (`None` clears
/// it), as a crash there would.
#[cfg(test)]
pub(crate) fn fail_after(step: Option<Step>) {
    FAIL_AFTER.with(|f| f.set(step));
}

/// The point after `step`.
#[cfg_attr(not(test), inline(always))]
pub(super) fn after(step: Step) -> io::Result<()> {
    #[cfg(test)]
    if FAIL_AFTER.with(|f| f.get()) == Some(step) {
        return Err(io::Error::other(format!("injected fault after {step:?}")));
    }
    let _ = step;
    Ok(())
}
