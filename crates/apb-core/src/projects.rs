//! Workspace registry (spec 6). Auto-populated file
//! `<config_dir>/projects.json`, keyed by `workspace_id`. Written
//! concurrently by several processes (CLI, MCP, server), so access is
//! serialized via a file lock, writes are atomic, permissions 0600.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::GlobalConfig;
use crate::fsutil::atomic_write_private;

const SCHEMA_VERSION: u32 = 1;
const DEFAULT_UNREACHABLE_DAYS: u64 = 14;
const DEFAULT_PURGE_DAYS: u64 = 90;
const MS_PER_DAY: u64 = 24 * 60 * 60 * 1000;
const LOCK_ATTEMPTS: u32 = 80;
const LOCK_STEP_MS: u64 = 25;

/// State of a workspace entry (spec 6.4). Timestamps are `u64` ms: that is
/// enough for centuries, and `u128` does not deserialize via serde_json in
/// an internally-tagged enum (buffering through Content).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum State {
    Active,
    Unreachable { since_ms: u64 },
    Tombstoned { since_ms: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectEntry {
    pub workspace_id: String,
    #[serde(default)]
    pub fingerprint: Option<String>,
    pub path: String,
    pub name: String,
    pub last_seen_ms: u64,
    #[serde(default)]
    pub playbook_count: usize,
    pub state: State,
}

#[derive(Debug, Serialize, Deserialize)]
struct ProjectsFile {
    #[serde(default = "default_schema")]
    schema_version: u32,
    #[serde(default)]
    entries: BTreeMap<String, ProjectEntry>,
}

fn default_schema() -> u32 {
    SCHEMA_VERSION
}

impl Default for ProjectsFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            entries: BTreeMap::new(),
        }
    }
}

/// Error accessing a workspace through the registry (spec 6.4): the path was removed/moved.
#[derive(Debug, thiserror::Error)]
pub enum ProjectAccessError {
    #[error("workspace `{0}` is not registered")]
    Unknown(String),
    #[error("workspace `{workspace_id}` is unreachable (path `{path}`)")]
    Unreachable { workspace_id: String, path: String },
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn projects_path() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join("projects.json"))
}

/// Whether auto-registration is disabled: env `APB_NO_REGISTRY=1`, a CI
/// environment, or `registry: false` in the config (spec 6.2).
fn registration_disabled(cfg: &GlobalConfig) -> bool {
    if std::env::var("APB_NO_REGISTRY")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return true;
    }
    if std::env::var("CI").is_ok() {
        return true;
    }
    cfg.registry == Some(false)
}

fn unreachable_ms(cfg: &GlobalConfig) -> u64 {
    cfg.registry_unreachable_days
        .unwrap_or(DEFAULT_UNREACHABLE_DAYS)
        .saturating_mul(MS_PER_DAY)
}

fn purge_ms(cfg: &GlobalConfig) -> u64 {
    cfg.registry_purge_days
        .unwrap_or(DEFAULT_PURGE_DAYS)
        .saturating_mul(MS_PER_DAY)
}

/// RAII lock over `projects.json.lock`. Best-effort: on timeout (a stuck
/// lock left by a crashed process) it force-steals the lock. The lock is
/// tagged with a unique owner token: the guard removes the file ONLY if the
/// token is still ours - otherwise, after a force-steal, one process could
/// tear down another live process's lock (cascade).
struct LockGuard {
    path: PathBuf,
    token: String,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // Remove only our own lock: if the content is no longer our token,
        // the lock has been taken over (ours expired and was grabbed) -
        // do not touch someone else's lock.
        if std::fs::read_to_string(&self.path)
            .map(|c| c.trim() == self.token)
            .unwrap_or(false)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn new_lock_token() -> String {
    format!("{}-{}", std::process::id(), uuid::Uuid::new_v4().simple())
}

fn acquire_lock(base: &Path) -> std::io::Result<LockGuard> {
    use std::io::Write;
    std::fs::create_dir_all(base)?;
    let path = base.join("projects.json.lock");
    let token = new_lock_token();
    for _ in 0..LOCK_ATTEMPTS {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                f.write_all(token.as_bytes())?;
                return Ok(LockGuard { path, token });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                std::thread::sleep(std::time::Duration::from_millis(LOCK_STEP_MS));
            }
            Err(e) => return Err(e),
        }
    }
    // Timeout: treat the lock as stale, force-steal it under our token.
    let _ = std::fs::remove_file(&path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    f.write_all(token.as_bytes())?;
    Ok(LockGuard { path, token })
}

fn read_file(path: &Path) -> ProjectsFile {
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            eprintln!(
                "apb: ignoring malformed projects registry `{}`: {e}",
                path.display()
            );
            ProjectsFile::default()
        }),
        Err(_) => ProjectsFile::default(),
    }
}

fn write_file(path: &Path, file: &ProjectsFile) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(file).map_err(std::io::Error::other)?;
    atomic_write_private(path, &bytes)
}

/// All registry operations go through this single point.
fn with_registry<T>(f: impl FnOnce(&mut ProjectsFile) -> T) -> std::io::Result<T> {
    let Some(path) = projects_path() else {
        // Configless environment: there is no registry, hand back an empty snapshot.
        let mut empty = ProjectsFile::default();
        return Ok(f(&mut empty));
    };
    let base = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let _lock = acquire_lock(&base)?;
    let mut file = read_file(&path);
    let out = f(&mut file);
    write_file(&path, &file)?;
    Ok(out)
}

fn count_playbooks(root: &Path) -> usize {
    crate::registry::Registry::open(root)
        .map(|r| r.playbook_ids().len())
        .unwrap_or(0)
}

fn workspace_name(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string_lossy().into_owned())
}

/// Transitions `unreachable` entries older than the threshold to
/// `tombstoned`, physically removes `tombstoned` entries older than the
/// purge threshold. Called inside the lock on every access.
fn apply_time_transitions(file: &mut ProjectsFile, cfg: &GlobalConfig) {
    let now = now_ms();
    let unreach = unreachable_ms(cfg);
    let purge = purge_ms(cfg);
    let mut to_remove = Vec::new();
    for (id, e) in file.entries.iter_mut() {
        match &e.state {
            State::Unreachable { since_ms } if now.saturating_sub(*since_ms) >= unreach => {
                e.state = State::Tombstoned { since_ms: now };
            }
            State::Tombstoned { since_ms } if now.saturating_sub(*since_ms) >= purge => {
                to_remove.push(id.clone());
            }
            _ => {}
        }
    }
    for id in to_remove {
        file.entries.remove(&id);
    }
}

/// Best-effort auto-registration of the current workspace (spec 6.2). Never
/// returns an error and never noticeably slows the command down: any
/// failure (no config, lock held past the timeout, corrupt file) is
/// silently swallowed.
pub fn touch(root: &Path) {
    let cfg = GlobalConfig::load().unwrap_or_default();
    if registration_disabled(&cfg) {
        return;
    }
    let Ok(workspace_id) = crate::workspace::ensure_id(root) else {
        return;
    };
    let fingerprint = crate::workspace::fingerprint(root);
    let name = workspace_name(root);
    let path = root.to_string_lossy().into_owned();
    let count = count_playbooks(root);
    let now = now_ms();

    let _ = with_registry(|file| {
        apply_time_transitions(file, &cfg);
        file.entries.insert(
            workspace_id.clone(),
            ProjectEntry {
                workspace_id: workspace_id.clone(),
                fingerprint,
                path,
                name,
                last_seen_ms: now,
                playbook_count: count,
                state: State::Active,
            },
        );
    });
}

/// Active and unreachable entries (excluding tombstoned), with time-based
/// transitions applied (spec 6.4).
pub fn list_active() -> Vec<ProjectEntry> {
    let cfg = GlobalConfig::load().unwrap_or_default();
    with_registry(|file| {
        apply_time_transitions(file, &cfg);
        file.entries
            .values()
            .filter(|e| !matches!(e.state, State::Tombstoned { .. }))
            .cloned()
            .collect()
    })
    .unwrap_or_default()
}

/// Resolves a `workspace_id` to a root path on disk (spec 6.4). Success
/// transitions the entry to `active`; failure (path missing or no `.apb/`)
/// transitions it to `unreachable` and returns a structured error.
/// Time-based transitions are applied along the way.
pub fn resolve_root(workspace_id: &str) -> Result<PathBuf, ProjectAccessError> {
    let cfg = GlobalConfig::load().unwrap_or_default();
    let now = now_ms();
    with_registry(|file| {
        apply_time_transitions(file, &cfg);
        let Some(entry) = file.entries.get_mut(workspace_id) else {
            return Err(ProjectAccessError::Unknown(workspace_id.to_string()));
        };
        let path = PathBuf::from(&entry.path);
        // Identity binding: the path must actually contain a workspace with
        // the REQUESTED id (spec 6). Otherwise, editing projects.json could
        // redirect resolve to someone else's directory.
        let local_id = std::fs::read_to_string(path.join(".apb/workspace.local"))
            .ok()
            .map(|s| s.trim().to_string());
        let reachable = path.is_dir()
            && path.join(".apb").is_dir()
            && local_id.as_deref() == Some(workspace_id);
        if reachable {
            entry.state = State::Active;
            entry.last_seen_ms = now;
            Ok(std::fs::canonicalize(&path).unwrap_or(path))
        } else {
            if !matches!(
                entry.state,
                State::Unreachable { .. } | State::Tombstoned { .. }
            ) {
                entry.state = State::Unreachable { since_ms: now };
            }
            Err(ProjectAccessError::Unreachable {
                workspace_id: workspace_id.to_string(),
                path: entry.path.clone(),
            })
        }
    })
    .unwrap_or_else(|_| Err(ProjectAccessError::Unknown(workspace_id.to_string())))
}

/// Manual removal of an entry (for `playbook projects remove`). Returns
/// `true` if the entry existed.
pub fn remove(workspace_id: &str) -> bool {
    with_registry(|file| file.entries.remove(workspace_id).is_some()).unwrap_or(false)
}

#[cfg(test)]
pub(crate) fn test_set_unreachable_since(workspace_id: &str, since_ms: u64) {
    let _ = with_registry(|file| {
        if let Some(e) = file.entries.get_mut(workspace_id) {
            e.state = State::Unreachable { since_ms };
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::init_project;

    struct EnvGuard;
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                std::env::remove_var("APB_CONFIG_DIR");
                std::env::remove_var("APB_NO_REGISTRY");
                std::env::remove_var("CI");
            }
        }
    }

    fn setup(cfg: &Path) {
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg);
            std::env::remove_var("APB_NO_REGISTRY");
            std::env::remove_var("CI");
        }
    }

    #[test]
    fn touch_registers_and_updates_path_by_workspace_id() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let a = tempfile::tempdir().unwrap();
        init_project(a.path()).unwrap();
        touch(a.path());
        let ws_id = crate::workspace::ensure_id(a.path()).unwrap();
        let listed = list_active();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].workspace_id, ws_id);
        let first_path = listed[0].path.clone();

        // "Move": the same workspace.local in a new directory -> a single
        // entry, path updated.
        let b = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(b.path().join(".apb/playbooks")).unwrap();
        std::fs::write(b.path().join(".apb/workspace.local"), &ws_id).unwrap();
        touch(b.path());
        let listed = list_active();
        assert_eq!(
            listed.len(),
            1,
            "same workspace_id must not create a second entry"
        );
        assert_ne!(
            listed[0].path, first_path,
            "path should follow the workspace"
        );
    }

    #[test]
    fn unreachable_then_tombstoned_by_time_only() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let proj = tempfile::tempdir().unwrap();
        init_project(proj.path()).unwrap();
        touch(proj.path());
        let ws_id = crate::workspace::ensure_id(proj.path()).unwrap();

        // Path disappears -> resolve_root marks it unreachable and returns an error.
        drop(proj);
        let err = resolve_root(&ws_id).unwrap_err();
        assert!(matches!(err, ProjectAccessError::Unreachable { .. }));
        assert_eq!(list_active().len(), 1, "unreachable still listed");

        // Fake since_ms to 15 days ago -> the next access tombstones it.
        let long_ago = now_ms().saturating_sub(15 * MS_PER_DAY);
        test_set_unreachable_since(&ws_id, long_ago);
        let listed = list_active();
        assert!(listed.is_empty(), "tombstoned workspace must not be listed");
    }

    #[test]
    fn ci_env_skips_registration() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        unsafe {
            std::env::set_var("CI", "1");
        }
        let _g = EnvGuard;

        let proj = tempfile::tempdir().unwrap();
        init_project(proj.path()).unwrap();
        touch(proj.path());
        assert!(list_active().is_empty(), "CI must skip registration");
    }
}
