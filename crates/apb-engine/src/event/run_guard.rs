//! Whether a run's definition is guarded, as the event log keeps it.
//!
//! A run whose definition uses a feature an older apb would silently drop
//! (`apb_core::validate::GuardedFeature`) carries a `run_guard` after every
//! checkpoint while the latest `run_guard` names any feature. Such an apb
//! then finds an unknown event after the last checkpoint and refuses to
//! continue the run; stopping it stays possible for every binary. The
//! journal is self-describing: every writer knows whether the run is
//! guarded without loading the definition.
//!
//! A `run_guard` naming a feature this binary does not know was written by
//! a newer apb: the event log reads it as an unknown event
//! ([`newer_feature`]), so this binary refuses to continue the run the way
//! an older one refuses a `run_guard` itself, and stopping still works.
//!
//! This module only keeps the state; the event log maps each event to the
//! `requires` of a `run_guard` it is, and appends the guard in the same
//! write as the checkpoint, after any deferral guard.

/// The features the latest `run_guard` of the journal names.
#[derive(Debug, Default)]
pub(super) struct RunGuardTracker {
    requires: Vec<String>,
}

impl RunGuardTracker {
    /// The state after a journal whose events have these `run_guard`
    /// lists (`None` for every other event), in journal order.
    pub(super) fn fold<'a>(guards: impl Iterator<Item = Option<&'a [String]>>) -> Self {
        let mut tracker = Self::default();
        for g in guards {
            tracker.apply(g);
        }
        tracker
    }

    /// Folds one event appended to the journal: the `requires` of a
    /// `run_guard`, `None` for any other event.
    pub(super) fn apply(&mut self, guard: Option<&[String]>) {
        if let Some(requires) = guard {
            self.requires = requires.to_vec();
        }
    }

    /// The `requires` of the guard a checkpoint appended now carries, or
    /// `None` while the run is not guarded.
    pub(super) fn requires_after_checkpoint(&self) -> Option<Vec<String>> {
        (!self.requires.is_empty()).then(|| self.requires.clone())
    }
}

/// The first feature of a `run_guard`'s `requires` this binary does not
/// know, written by a newer apb; `None` when it knows every one.
pub(super) fn newer_feature(requires: &[String]) -> Option<&str> {
    requires
        .iter()
        .map(String::as_str)
        .find(|name| apb_core::validate::GuardedFeature::parse(name).is_none())
}
