//! Store of dismissed "make this a playbook" suggestions (spec 8.2).
//!
//! A user who answers "no, and don't suggest this again" gets the suggestion
//! pattern (an English kebab-slug) recorded here. The catalog returns the live
//! patterns to the agent so it doesn't suggest the same thing again. Records
//! expire by TTL.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::fsutil::atomic_write_private;

const SCHEMA_VERSION: u32 = 1;
const DEFAULT_TTL_DAYS: u64 = 90;
const MS_PER_DAY: u64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DismissRecord {
    pub created_ms: u64,
    pub ttl_days: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct DismissFile {
    #[serde(default = "default_schema")]
    schema_version: u32,
    #[serde(default)]
    patterns: BTreeMap<String, DismissRecord>,
}

fn default_schema() -> u32 {
    SCHEMA_VERSION
}

impl Default for DismissFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            patterns: BTreeMap::new(),
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn dismiss_path() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join("dismissed.json"))
}

fn load() -> DismissFile {
    let Some(path) = dismiss_path() else {
        return DismissFile::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            eprintln!(
                "apb: ignoring malformed dismiss store `{}`: {e}",
                path.display()
            );
            DismissFile::default()
        }),
        Err(_) => DismissFile::default(),
    }
}

fn is_live(rec: &DismissRecord, now: u64) -> bool {
    let ttl_ms = rec.ttl_days.saturating_mul(MS_PER_DAY);
    now.saturating_sub(rec.created_ms) < ttl_ms
}

const LOCK_NAME: &str = "dismissed.json.lock";

/// Live (not-yet-expired) dismissal patterns. Also cleans up expired entries
/// if the file is writable. Never-fail: any failure -> an empty list. The
/// read-modify-write is serialized by a file lock (shared with record) -
/// protection against process races.
pub fn active_patterns() -> Vec<String> {
    let now = now_ms();
    let Some(dir) = crate::config::config_dir() else {
        return Vec::new();
    };
    let _lock = crate::fsutil::lock_dir(&dir, LOCK_NAME).ok();
    let mut file = load();
    let before = file.patterns.len();
    file.patterns.retain(|_, rec| is_live(rec, now));
    let live: Vec<String> = file.patterns.keys().cloned().collect();
    if file.patterns.len() != before
        && let Ok(bytes) = serde_json::to_vec_pretty(&file)
    {
        let _ = atomic_write_private(&dir.join("dismissed.json"), &bytes);
    }
    live
}

/// Records a dismissal of a pattern (spec 8.2). TTL in days; `None` -> default 90.
/// Best-effort, under the same lock as active_patterns.
pub fn record(pattern: &str, ttl_days: Option<u64>) -> std::io::Result<()> {
    let Some(dir) = crate::config::config_dir() else {
        return Ok(());
    };
    let _lock = crate::fsutil::lock_dir(&dir, LOCK_NAME).ok();
    let mut file = load();
    file.patterns.insert(
        pattern.to_string(),
        DismissRecord {
            created_ms: now_ms(),
            ttl_days: ttl_days.unwrap_or(DEFAULT_TTL_DAYS),
        },
    );
    let bytes = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
    atomic_write_private(&dir.join("dismissed.json"), &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard;
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                std::env::remove_var("APB_CONFIG_DIR");
            }
        }
    }

    #[test]
    fn record_then_active_roundtrip() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        assert!(active_patterns().is_empty());
        record("save-review-playbook", None).unwrap();
        assert_eq!(active_patterns(), vec!["save-review-playbook".to_string()]);
    }

    #[test]
    fn expired_pattern_drops_out() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        record("old-thing", Some(0)).unwrap();
        assert!(active_patterns().is_empty());
    }
}
