use std::path::{Path, PathBuf};
use std::process::Command;

use apb_core::fsutil::atomic_write;

use crate::error::EngineError;

#[derive(Debug)]
pub struct WorkdirGuard {
    lock_path: PathBuf,
}

impl Drop for WorkdirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

fn pid_alive(pid: u32) -> bool {
    // unix: `kill -0 <pid>` exits successfully if the process exists.
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn acquire(root: &Path, allow_shared: bool) -> Result<Option<WorkdirGuard>, EngineError> {
    if allow_shared {
        return Ok(None);
    }
    let lock_path = root.join(".apb/workdir.lock");
    if lock_path.is_file() {
        let raw = std::fs::read_to_string(&lock_path).unwrap_or_default();
        let pid: u32 = raw.trim().parse().unwrap_or(0);
        if pid != 0 && pid_alive(pid) {
            return Err(EngineError::WorkdirBusy(format!(
                "another write-run holds the workdir (pid {pid}); use worktree or --allow-shared-workdir"
            )));
        }
        // stale lock - overwrite it
    }
    atomic_write(&lock_path, std::process::id().to_string().as_bytes())?;
    Ok(Some(WorkdirGuard { lock_path }))
}
