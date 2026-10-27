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
    pub(super) fn start(run_dir: &Path, deadline_ms: u64) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let (flag, dir) = (done.clone(), run_dir.to_path_buf());
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                let now = apb_core::clock::now_ms();
                let at = u128::from(deadline_ms);
                if now >= at {
                    let _ = crate::control::post_control(
                        &dir,
                        Control::Abort {
                            reason: DEADLINE_REASON.into(),
                        },
                    );
                    return;
                }
                let left = (at - now).min(500) as u64;
                std::thread::park_timeout(Duration::from_millis(left));
            }
        });
        DeadlineWatch {
            done,
            thread: Some(thread),
        }
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
