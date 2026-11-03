//! The setup record: what `apb setup apply` did, kept under the config dir
//! (`<config_dir>/setup/record.json`, private, written atomically) so that
//! `apb setup remove` takes back exactly that. Backups of edited host
//! configs sit next to it in `backups/`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The folder under the config dir.
pub const SETUP_DIR: &str = "setup";
const RECORD_FILE: &str = "record.json";
const BACKUPS_DIR: &str = "backups";
/// Its own name, not the age-checked `.setup.lock` older versions created
/// and removed, so the two schemes never read each other's file.
pub(super) const LOCK: &str = ".setup.flock";

/// One thing setup did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub kind: String,
    pub target: String,
    pub at_ms: u128,
    /// What was written, as the remove of `kind` needs it.
    #[serde(default)]
    pub data: Value,
}

/// One applied plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Applied {
    pub digest: String,
    pub at_ms: u128,
}

/// The whole record.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    #[serde(default)]
    pub applied: Vec<Applied>,
    #[serde(default)]
    pub entries: Vec<Entry>,
}

/// `<config_dir>/setup`.
pub fn setup_dir() -> Option<PathBuf> {
    apb_core::config::config_dir().map(|d| d.join(SETUP_DIR))
}

/// Where edited host configs are backed up.
pub fn backups_dir(dir: &Path) -> PathBuf {
    dir.join(BACKUPS_DIR)
}

/// The path of the record in `dir`.
pub fn record_path(dir: &Path) -> PathBuf {
    dir.join(RECORD_FILE)
}

/// Reads the record; an absent one is empty.
pub fn load(dir: &Path) -> Result<Record, String> {
    match std::fs::read_to_string(record_path(dir)) {
        Ok(raw) => {
            serde_json::from_str(&raw).map_err(|e| format!("the setup record does not parse: {e}"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Record::default()),
        Err(e) => Err(e.to_string()),
    }
}

/// Writes the record, private and atomically.
pub fn save(dir: &Path, record: &Record) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?;
    apb_core::fsutil::atomic_write_private(&record_path(dir), &json).map_err(|e| e.to_string())
}

/// The lock every apply and remove holds: an OS lock on a file in `dir`
/// ([`apb_core::fsutil::hold_lock`]). It is never taken from a setup that is
/// still running, however long it runs, and a crashed setup's lock is free
/// at once, since the kernel drops it with the process.
pub fn lock(dir: &Path) -> Result<apb_core::fsutil::HeldLock, String> {
    apb_core::fsutil::hold_lock(dir, LOCK).map_err(|e| format!("setup is busy: {e}"))
}
