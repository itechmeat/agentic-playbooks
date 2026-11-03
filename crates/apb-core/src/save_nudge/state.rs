//! What the save check remembers per host session, so it reminds at most
//! once per session and asks the decision model at most once per request:
//! `<config_dir>/hooks/save-nudge.json`, private, written atomically under
//! a lock. A session is kept by the sha256 of its id, a request by the
//! sha256 of its text, never the text itself. Sessions untouched for
//! [`KEEP_DAYS`] are dropped on the next write.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The folder under the config dir.
pub const HOOKS_DIR: &str = "hooks";
const FILE: &str = "save-nudge.json";
const LOCK: &str = ".save-nudge.flock";
/// How long a session is remembered.
pub const KEEP_DAYS: u64 = 30;
/// The most model answers kept per session.
const MAX_ANSWERS: usize = 32;
const DAY_MS: u64 = 86_400_000;

/// One session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEntry {
    /// The last time the check wrote this entry.
    pub at_ms: u64,
    /// The session was reminded.
    #[serde(default)]
    pub nudged: bool,
    /// The decision model's answer per request digest.
    #[serde(default)]
    pub answers: BTreeMap<String, bool>,
    /// The digests of [`SessionEntry::answers`], oldest first: the cap
    /// drops the oldest answer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    sessions: BTreeMap<String, SessionEntry>,
}

/// The state folder: `<config_dir>/hooks`.
pub fn dir() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join(HOOKS_DIR))
}

/// The key a session is kept under.
pub fn session_key(session_id: &str) -> String {
    crate::content::sha256_hex(session_id.as_bytes())
}

/// The key a request is kept under.
pub fn request_key(request: &str) -> String {
    crate::content::sha256_hex(request.as_bytes())
}

/// The state file; one that does not parse (a truncated or hand-edited
/// file) reads as empty, so the next write replaces it instead of the check
/// staying silent for good.
fn load(dir: &Path) -> std::io::Result<StateFile> {
    match std::fs::read(dir.join(FILE)) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_default()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StateFile::default()),
        Err(e) => Err(e),
    }
}

/// The entry of the session `key` in `dir`; a missing one is empty.
pub fn entry(dir: &Path, key: &str) -> std::io::Result<SessionEntry> {
    Ok(load(dir)?.sessions.remove(key).unwrap_or_default())
}

/// Changes the entry of the session `key` with `f` under the lock, drops
/// sessions older than [`KEEP_DAYS`], and writes the file; what `f`
/// returned.
pub fn update<R>(
    dir: &Path,
    key: &str,
    now_ms: u64,
    f: impl FnOnce(&mut SessionEntry) -> R,
) -> std::io::Result<R> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match crate::fsutil::create_private_dir(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    let _lock = crate::fsutil::hold_lock(dir, LOCK)?;
    let mut file = load(dir)?;
    let horizon = now_ms.saturating_sub(KEEP_DAYS * DAY_MS);
    file.sessions.retain(|_, e| e.at_ms >= horizon);
    let e = file.sessions.entry(key.to_string()).or_default();
    let out = f(e);
    e.at_ms = now_ms;
    let answers = &e.answers;
    e.order.retain(|k| answers.contains_key(k));
    let known: std::collections::BTreeSet<&String> = e.order.iter().collect();
    let added: Vec<String> = answers
        .keys()
        .filter(|k| !known.contains(k))
        .cloned()
        .collect();
    e.order.extend(added);
    while e.answers.len() > MAX_ANSWERS && !e.order.is_empty() {
        let oldest = e.order.remove(0);
        e.answers.remove(&oldest);
    }
    let bytes = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
    crate::fsutil::atomic_write_private(&dir.join(FILE), &bytes)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_is_remembered_and_old_ones_go() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("hooks");
        let now = 100 * DAY_MS;
        update(&dir, "old", now - 31 * DAY_MS, |e| e.nudged = true).unwrap();
        update(&dir, "s", now, |e| {
            e.answers.insert("r".into(), true);
        })
        .unwrap();
        update(&dir, "s", now, |e| e.nudged = true).unwrap();
        let s = entry(&dir, "s").unwrap();
        assert!(s.nudged);
        assert_eq!(s.answers.get("r"), Some(&true));
        assert_eq!(entry(&dir, "old").unwrap(), SessionEntry::default());
        let raw = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(!raw.contains("\"old\""), "{raw}");
    }

    #[test]
    fn a_corrupt_file_is_read_as_empty_and_rewritten() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("hooks");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE), "{\"sessions\": tru").unwrap();
        assert_eq!(entry(&dir, "s").unwrap(), SessionEntry::default());
        update(&dir, "s", DAY_MS, |e| e.nudged = true).unwrap();
        assert!(entry(&dir, "s").unwrap().nudged);
    }

    #[test]
    fn the_answer_cap_drops_the_oldest_answer_not_the_smallest_key() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("hooks");
        for i in 0..MAX_ANSWERS {
            update(&dir, "s", DAY_MS, |e| {
                e.answers.insert(format!("b{i:02}"), true);
            })
            .unwrap();
        }
        // The newest answer sorts first; the oldest one, `b00`, goes.
        update(&dir, "s", DAY_MS, |e| {
            e.answers.insert("a".into(), false);
        })
        .unwrap();
        let answers = entry(&dir, "s").unwrap().answers;
        assert_eq!(answers.len(), MAX_ANSWERS);
        assert_eq!(answers.get("a"), Some(&false));
        assert!(!answers.contains_key("b00"), "{answers:?}");
    }
}
