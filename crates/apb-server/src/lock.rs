//! `<config_dir>/serve.lock`: which dashboard serves this config dir.
//!
//! The global dashboard is one per config dir (it reconciles connectors,
//! watches the global store, owns the projects registry view). The port bind
//! only excludes a second server on the SAME port, so the lock is what keeps
//! a second dashboard on another port from starting over the same store, and
//! it names the one that is running when that is refused. Only the instance
//! that wrote the lock removes it.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};

use apb_core::fsutil::atomic_write;
use serde::{Deserialize, Serialize};

const LOCK_FILE: &str = "serve.lock";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockInfo {
    pub port: u16,
    pub pid: u32,
    pub root_fingerprint: String,
    pub instance_id: String,
}

/// Why [`GlobalLock::acquire`] refused.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error(
        "an apb dashboard (pid {pid}) is already serving this config dir on port {port}; stop it or open http://127.0.0.1:{port}"
    )]
    Held { pid: u32, port: u16 },
    #[error(transparent)]
    Io(#[from] io::Error),
}

fn fingerprint(root: &Path) -> String {
    let mut h = DefaultHasher::new();
    root.to_string_lossy().hash(&mut h);
    format!("{:016x}", h.finish())
}

/// The lock currently recorded for `config_dir`, if any parses.
pub fn read_global_lock(config_dir: &Path) -> Option<LockInfo> {
    let raw = std::fs::read(config_dir.join(LOCK_FILE)).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// The held global dashboard lock. Dropping it removes the file, but only
/// while it still names this instance.
#[derive(Debug)]
pub struct GlobalLock {
    path: PathBuf,
    info: LockInfo,
}

impl GlobalLock {
    /// Takes the lock for a dashboard on `port`. Refused while another live
    /// apb process holds it; a lock left by a dead process, or by this
    /// process before a re-exec (same pid), is replaced.
    pub fn acquire(config_dir: &Path, port: u16) -> Result<Self, LockError> {
        // Two dashboards starting at once must not both see "free".
        let _guard = apb_core::fsutil::lock_dir(config_dir, ".serve.lock.lock")?;
        if let Some(existing) = read_global_lock(config_dir)
            && existing.pid != std::process::id()
            && apb_engine::liveness::apb_pid_is_live(existing.pid)
        {
            return Err(LockError::Held {
                pid: existing.pid,
                port: existing.port,
            });
        }
        let info = LockInfo {
            port,
            pid: std::process::id(),
            root_fingerprint: fingerprint(config_dir),
            instance_id: uuid::Uuid::new_v4().to_string(),
        };
        let bytes = serde_json::to_vec_pretty(&info).map_err(io::Error::other)?;
        let path = config_dir.join(LOCK_FILE);
        atomic_write(&path, &bytes)?;
        Ok(Self { path, info })
    }

    pub fn info(&self) -> &LockInfo {
        &self.info
    }
}

impl Drop for GlobalLock {
    fn drop(&mut self) {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        if read_global_lock(dir).is_some_and(|l| l.instance_id == self.info.instance_id) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
