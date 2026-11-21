//! Faults an install test injects on its own thread: the template
//! install's fit report and the draft's provenance write (compiled only
//! with the `testing` feature, which a build without debug assertions
//! refuses). A leaf: the install and the draft write both read it.

use std::cell::RefCell;

/// A step of an install made to fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The fit report of the new draft.
    Fit,
    /// The provenance file of the new draft.
    Provenance,
}

thread_local! {
    static FAULTS: RefCell<Vec<Fault>> = const { RefCell::new(Vec::new()) };
}

/// The faults in force on this thread until the guard drops.
pub struct Injected(());

impl Drop for Injected {
    fn drop(&mut self) {
        FAULTS.with(|f| f.borrow_mut().clear());
    }
}

/// Makes each of `faults` fail on this thread while the guard lives.
pub fn inject(faults: &[Fault]) -> Injected {
    FAULTS.with(|f| *f.borrow_mut() = faults.to_vec());
    Injected(())
}

/// Whether `fault` is injected on this thread.
pub(crate) fn fails(fault: Fault) -> bool {
    FAULTS.with(|f| f.borrow().contains(&fault))
}
