//! A test guard that reaps the detached driver a test started, shared by
//! the test binaries that start one (`#[path]`-included, since a detached
//! driver is a separate process no test owns as a child).
//!
//! On drop it reads the run's `driver.pid` and kills that process only when
//! its command line still names the run: a driver that already exited may
//! have handed its pid to an unrelated process, which must never be killed.

use std::path::{Path, PathBuf};

/// Kills the detached driver of one run when dropped.
pub struct DriverReaper {
    run_dir: PathBuf,
    run_id: String,
}

impl DriverReaper {
    /// The guard for run `run_id` of the project at `root`.
    pub fn new(root: &Path, run_id: &str) -> Self {
        DriverReaper {
            run_dir: root.join(".apb/runs").join(run_id),
            run_id: run_id.to_string(),
        }
    }
}

/// Whether process `pid` is running with `run_id` as one of its arguments.
fn names_run(pid: u32, run_id: &str) -> bool {
    std::process::Command::new("ps")
        .args(["-ww", "-o", "args=", "-p", &pid.to_string()])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .any(|arg| arg == run_id)
        })
}

impl Drop for DriverReaper {
    fn drop(&mut self) {
        let Some(pid) = std::fs::read_to_string(self.run_dir.join("driver.pid"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
        else {
            return;
        };
        if pid != std::process::id() && names_run(pid, &self.run_id) {
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status();
        }
    }
}
