//! Proof that apb created a run directory on this machine.
//!
//! A run directory lives in the workspace (`.apb/runs/<id>`), so a cloned
//! repository can ship one: a playbook snapshot, scripts, a manifest and a
//! journal cut off mid-run. Resuming such a directory would execute whatever
//! it holds. When apb prepares a run it stamps the directory with an HMAC of
//! the run id and its write-once manifest, keyed by a per-installation secret
//! in the config directory (`run-origin.key`, 0600), which never enters a
//! repository. A directory apb did not create here has no valid stamp.
//!
//! The key is created by [`ensure_key`] at the entry point of an `apb`
//! process; [`stamp`] never creates it, so library callers (tests, embedders)
//! that never ran that entry point write no key into the config directory and
//! simply produce unstamped runs.

use std::path::{Path, PathBuf};

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

const KEY_FILE: &str = "run-origin.key";
const STAMP_FILE: &str = "origin.stamp";

fn key_path() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join(KEY_FILE))
}

fn load_key() -> Option<Vec<u8>> {
    let raw = std::fs::read_to_string(key_path()?).ok()?;
    let key = raw.trim();
    (key.len() >= 64).then(|| key.as_bytes().to_vec())
}

/// Creates the installation's run-origin key if it does not exist yet. A
/// no-op without a config directory.
pub fn ensure_key() -> std::io::Result<()> {
    let Some(path) = key_path() else {
        return Ok(());
    };
    if load_key().is_some() {
        return Ok(());
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let _lock = crate::fsutil::lock_dir(dir, "run-origin.key.lock").ok();
    if load_key().is_some() {
        return Ok(());
    }
    let mut key = String::new();
    for _ in 0..2 {
        key.push_str(&uuid::Uuid::new_v4().simple().to_string());
    }
    crate::fsutil::atomic_write_private(&path, key.as_bytes())
}

fn mac(key: &[u8], run_dir: &Path, run_id: &str) -> std::io::Result<String> {
    let manifest = match std::fs::read(run_dir.join("manifest.yaml")) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    let mut m = HmacSha256::new_from_slice(key).map_err(std::io::Error::other)?;
    m.update(b"apb-run-origin-v1\0");
    m.update(&(run_id.len() as u64).to_le_bytes());
    m.update(run_id.as_bytes());
    m.update(&Sha256::digest(&manifest));
    Ok(crate::content::hex_lower(&m.finalize().into_bytes()))
}

/// Stamps `run_dir` as created here. Call once the manifest is written. A
/// no-op when the installation has no key yet (see [`ensure_key`]).
pub fn stamp(run_dir: &Path, run_id: &str) -> std::io::Result<()> {
    let Some(key) = load_key() else {
        return Ok(());
    };
    let tag = mac(&key, run_dir, run_id)?;
    crate::fsutil::atomic_write(&run_dir.join(STAMP_FILE), tag.as_bytes())
}

/// Whether `run_dir` carries a valid stamp for `run_id` from this
/// installation: apb created it here and its manifest is the one it wrote.
pub fn verify(run_dir: &Path, run_id: &str) -> bool {
    let (Some(key), Ok(stored)) = (
        load_key(),
        std::fs::read_to_string(run_dir.join(STAMP_FILE)),
    ) else {
        return false;
    };
    let Ok(expected) = mac(&key, run_dir, run_id) else {
        return false;
    };
    // Compared in constant time: the stamp is a MAC.
    let (a, b) = (expected.as_bytes(), stored.trim().as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
