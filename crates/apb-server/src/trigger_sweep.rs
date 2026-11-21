//! The trigger sweep host (start rules): the listener task that evaluates
//! every enabled rule of every registered project. Modelled on
//! `health_sweep.rs`, with a nudge so a delivery the append path just stored
//! is evaluated at once rather than at the next tick, and a heartbeat the
//! doctor reads to tell a live sweep from a stale one.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

/// The test-only override of the period, in milliseconds (debug builds
/// only, through `apb_engine::env_duration::test_override`).
pub(crate) const TRIGGER_SWEEP_ENV: &str = "APB_TRIGGER_SWEEP_MS";

/// The wake the append path hands the task, created by [`spawn`]. `Notify`
/// stores one permit, so nudges that arrive while a pass runs collapse into
/// the next one.
static NUDGE: OnceLock<tokio::sync::Notify> = OnceLock::new();

/// The task is started once per process.
static STARTED: OnceLock<()> = OnceLock::new();

/// The period between two sweeps: 5 s, shortened by the variable in a debug
/// build only (`apb_engine::env_duration::test_override`).
pub(crate) fn period() -> Duration {
    apb_engine::env_duration::test_override(std::env::var(TRIGGER_SWEEP_ENV).ok().as_deref())
        .unwrap_or(Duration::from_secs(5))
}

/// Wakes the task now (an append just stored an event). A no-op before
/// `spawn` ran in this process.
pub(crate) fn nudge() {
    if let Some(notify) = NUDGE.get() {
        notify.notify_one();
    }
}

/// One pass over `roots`; the number of rules considered. Errors are
/// printed (`apb trigger: <root> <rule>: <error>`) and the pass goes on.
pub(crate) fn sweep_once(roots: &[PathBuf]) -> usize {
    let mut n = 0;
    for root in roots {
        for (rule, res) in apb_mcp::tools::trigger::sweep_root(root) {
            n += 1;
            if let Err(e) = res {
                eprintln!("apb trigger: {} {rule}: {e}", root.display());
            }
        }
    }
    n
}

/// Starts the task once per process (a `OnceLock`): each pass sweeps the
/// rules of every registered project and beats the heartbeat the doctor
/// reads. A process inside a run step never starts it, the same check
/// `sweep_root` makes per pass, so a step never hosts a sweep. `true` only
/// for the one call that started the task.
pub(crate) fn spawn() -> bool {
    if STARTED.set(()).is_err() {
        return false;
    }
    if apb_core::step_env::StepCaller::from_env()
        .refusal("the rule sweep")
        .is_some()
    {
        return false;
    }
    let notify = NUDGE.get_or_init(tokio::sync::Notify::new);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(period()) => {}
                _ = notify.notified() => {}
            }
            let _ = tokio::task::spawn_blocking(|| {
                let roots: Vec<PathBuf> = apb_core::projects::list_active()
                    .into_iter()
                    .map(|e| PathBuf::from(e.path))
                    .collect();
                sweep_once(&roots)
            })
            .await;
            apb_engine::trigger::view::beat();
        }
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_period_defaults_to_five_seconds() {
        // SAFETY: the variable is only read by this module's own period.
        let saved = std::env::var_os(TRIGGER_SWEEP_ENV);
        unsafe { std::env::remove_var(TRIGGER_SWEEP_ENV) };
        assert_eq!(period(), Duration::from_secs(5));
        unsafe { std::env::set_var(TRIGGER_SWEEP_ENV, "250") };
        let short = period();
        match saved {
            Some(v) => unsafe { std::env::set_var(TRIGGER_SWEEP_ENV, v) },
            None => unsafe { std::env::remove_var(TRIGGER_SWEEP_ENV) },
        }
        assert_eq!(
            short,
            if cfg!(debug_assertions) {
                Duration::from_millis(250)
            } else {
                Duration::from_secs(5)
            }
        );
    }

    /// At most one call in a process starts the task: without the guard
    /// every call would start one (and answer `true`).
    #[tokio::test]
    async fn spawn_starts_one_task_per_process() {
        let started = [spawn(), spawn(), spawn()];
        assert!(started.iter().filter(|s| **s).count() <= 1, "{started:?}");
        assert!(!spawn(), "a later call never starts another task");
    }
}
