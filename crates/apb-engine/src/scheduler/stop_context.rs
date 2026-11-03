//! What stopped an attempt, as the drive knows it: the run abort, a `--now`
//! pause, or a supervisor interrupt. The attempt status reads `interrupted`
//! (or `cancelled` for a host task) in all three cases; only the stop
//! context tells them apart, and only it decides whether the chain walk
//! ends (`run_paused`, `run_aborted`) or goes on to retry and fallback
//! (`supervisor_interrupted`).

use std::sync::atomic::{AtomicBool, Ordering};

use crate::execution_identity::CloseReason;

/// Which stop closed an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopReason {
    RunPaused,
    RunAborted,
    SupervisorInterrupted,
}

impl StopReason {
    /// The close reason a stop journals.
    pub(crate) fn close_reason(self) -> CloseReason {
        match self {
            StopReason::RunPaused => CloseReason::RunPaused,
            StopReason::RunAborted => CloseReason::RunAborted,
            StopReason::SupervisorInterrupted => CloseReason::SupervisorInterrupted,
        }
    }

    /// Whether the stop ends the chain walk: no transition, no next step.
    pub(crate) fn ends_chain(self) -> bool {
        matches!(self, StopReason::RunPaused | StopReason::RunAborted)
    }
}

/// The flags an attempt's stop is read from.
pub(crate) struct StopContext<'a> {
    /// The run's abort flag.
    pub(crate) cancel: &'a AtomicBool,
    /// The node's kill flag; a `--now` pause marks it graceful
    /// (`crate::pause::interrupt`).
    pub(crate) kill: Option<&'a AtomicBool>,
    /// The supervisor interrupt flag of the attempt.
    pub(crate) interrupt: Option<&'a AtomicBool>,
}

impl StopContext<'_> {
    /// The stop that closed the attempt, if any: the abort first, then a
    /// graceful kill, then a supervisor interrupt. A kill flag set without
    /// the pause's mark is an abort (or a lost `join: any`, which ends the
    /// walk the same way). When `cancel` and `kill` are the one node flag a
    /// drive hands an execution, the mark alone tells a pause from an abort.
    pub(crate) fn reason(&self) -> Option<StopReason> {
        let graceful = self
            .kill
            .is_some_and(|k| k.load(Ordering::SeqCst) && crate::pause::interrupt::is_graceful(k));
        let same_flag = self.kill.is_some_and(|k| std::ptr::eq(k, self.cancel));
        if self.cancel.load(Ordering::SeqCst) && !(same_flag && graceful) {
            return Some(StopReason::RunAborted);
        }
        if graceful {
            return Some(StopReason::RunPaused);
        }
        if self.kill.is_some_and(|k| k.load(Ordering::SeqCst)) {
            return Some(StopReason::RunAborted);
        }
        if self.interrupt.is_some_and(|i| i.load(Ordering::SeqCst)) {
            return Some(StopReason::SupervisorInterrupted);
        }
        None
    }

    /// The close reason of [`Self::reason`].
    pub(crate) fn close_reason(&self) -> Option<CloseReason> {
        self.reason().map(StopReason::close_reason)
    }

    /// Whether a stop that ends the chain walk closed the attempt.
    pub(crate) fn ends_chain(&self) -> bool {
        self.reason().is_some_and(StopReason::ends_chain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(
        cancel: &'a AtomicBool,
        kill: &'a AtomicBool,
        interrupt: &'a AtomicBool,
    ) -> StopContext<'a> {
        StopContext {
            cancel,
            kill: Some(kill),
            interrupt: Some(interrupt),
        }
    }

    #[test]
    fn each_flag_names_its_own_stop() {
        let (cancel, kill, interrupt) = (
            AtomicBool::new(false),
            AtomicBool::new(false),
            AtomicBool::new(false),
        );
        assert_eq!(ctx(&cancel, &kill, &interrupt).reason(), None);
        interrupt.store(true, Ordering::SeqCst);
        let c = ctx(&cancel, &kill, &interrupt);
        assert_eq!(c.reason(), Some(StopReason::SupervisorInterrupted));
        assert!(!c.ends_chain(), "a supervisor interrupt keeps the walk");
        crate::pause::interrupt::mark_graceful(&kill);
        kill.store(true, Ordering::SeqCst);
        let c = ctx(&cancel, &kill, &interrupt);
        assert_eq!(c.reason(), Some(StopReason::RunPaused));
        assert!(c.ends_chain());
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(
            ctx(&cancel, &kill, &interrupt).reason(),
            Some(StopReason::RunAborted),
            "the abort wins over the pause"
        );
        crate::pause::interrupt::unmark_graceful(&kill);
    }

    #[test]
    fn one_node_flag_tells_a_pause_from_an_abort_by_its_mark() {
        let node = AtomicBool::new(false);
        let one = |f: &AtomicBool| {
            StopContext {
                cancel: f,
                kill: Some(f),
                interrupt: None,
            }
            .reason()
        };
        assert_eq!(one(&node), None);
        node.store(true, Ordering::SeqCst);
        assert_eq!(one(&node), Some(StopReason::RunAborted));
        crate::pause::interrupt::mark_graceful(&node);
        assert_eq!(one(&node), Some(StopReason::RunPaused));
        crate::pause::interrupt::unmark_graceful(&node);
    }
}
