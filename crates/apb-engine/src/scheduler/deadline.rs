//! An eval run's wall clock: the drive aborts the run itself at its
//! deadline, so the limit holds even when the `apb eval` that started it is
//! gone.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::control::Control;

/// Posts an abort to the run it watches once the wall clock passes
/// `deadline_ms`; dropping it (the drive ended) ends the watch.
pub(super) struct DeadlineWatch {
    done: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// The abort reason a run past its eval deadline is stopped with.
pub const DEADLINE_REASON: &str = "deadline: the run passed its wall-clock limit";

impl DeadlineWatch {
    /// An eval run's watch: posts an immediate abort with
    /// [`DEADLINE_REASON`] at `deadline_ms`.
    pub(super) fn start(run_dir: &Path, deadline_ms: u64) -> Self {
        let dir = run_dir.to_path_buf();
        Self::at(u128::from(deadline_ms), move || {
            let _ = crate::control::post_control(
                &dir,
                Control::Abort {
                    reason: DEADLINE_REASON.into(),
                    by: Some(crate::pause::kind::PauseBy::Engine),
                    graceful: false,
                },
            );
            true
        })
    }

    /// A run's deadline watch (see `crate::run_deadline`): at the latch's
    /// expiry it ends the run through the one stop path, decided under the
    /// event lock; a stop that could not take the lock tries again.
    pub(super) fn start_run(
        root: &Path,
        run_dir: &Path,
        run_id: &str,
        latch: crate::run_deadline::DeadlineLatch,
    ) -> Self {
        let (root, dir, run_id) = (
            root.to_path_buf(),
            run_dir.to_path_buf(),
            run_id.to_string(),
        );
        Self::at(latch.expires_at_ms + held_back_ms(), move || {
            let _ = &dir;
            #[cfg(any(test, feature = "testing"))]
            crate::deadline_hooks::at(crate::deadline_hooks::Hook::WatchBeforeStop, &dir);
            crate::run_deadline::stop_if_expired(
                &root,
                &run_id,
                crate::run_deadline::Expect::Driving,
            )
            .is_ok()
        })
    }

    /// Runs `fire` once the wall clock passes `at_ms`, again every poll
    /// while it returns false; dropping the watch ends it.
    fn at(at_ms: u128, mut fire: impl FnMut() -> bool + Send + 'static) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let flag = done.clone();
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                let now = apb_core::clock::now_ms();
                if now >= at_ms && fire() {
                    return;
                }
                let left = at_ms.saturating_sub(now).clamp(1, 500) as u64;
                std::thread::park_timeout(Duration::from_millis(left));
            }
        });
        DeadlineWatch {
            done,
            thread: Some(thread),
        }
    }
}

/// How long a test holds the run watch back past the deadline, so only the
/// admission checks can stop the work (`APB_TEST_DEADLINE_WATCH_DELAY_MS`).
/// Zero outside test builds.
fn held_back_ms() -> u128 {
    #[cfg(any(test, feature = "testing"))]
    {
        crate::env_duration::millis("APB_TEST_DEADLINE_WATCH_DELAY_MS").map_or(0, |d| d.as_millis())
    }
    #[cfg(not(any(test, feature = "testing")))]
    {
        0
    }
}

impl Drop for DeadlineWatch {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}
