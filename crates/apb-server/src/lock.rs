use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::Path;

use apb_core::fsutil::atomic_write;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct LockInfo {
    pub port: u16,
    pub pid: u32,
    pub root_fingerprint: String,
    pub instance_id: String,
}

fn fingerprint(root: &Path) -> String {
    let mut h = DefaultHasher::new();
    root.to_string_lossy().hash(&mut h);
    format!("{:016x}", h.finish())
}

pub fn write_lock(root: &Path, port: u16) -> io::Result<LockInfo> {
    let info = LockInfo {
        port,
        pid: std::process::id(),
        root_fingerprint: fingerprint(root),
        instance_id: uuid::Uuid::new_v4().to_string(),
    };
    let bytes = serde_json::to_vec_pretty(&info).map_err(io::Error::other)?;
    atomic_write(&root.join(".apb/serve.lock"), &bytes)?;
    Ok(info)
}

pub fn remove_lock(root: &Path) -> io::Result<()> {
    let p = root.join(".apb/serve.lock");
    if p.exists() {
        std::fs::remove_file(p)?;
    }
    Ok(())
}
