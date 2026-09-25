//! Workspace registry (spec 6). Auto-populated file
//! `<config_dir>/projects.json`, keyed by `workspace_id`. Written
//! concurrently by several processes (CLI, MCP, server), so access is
//! serialized via a file lock, writes are atomic, permissions 0600.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::GlobalConfig;
use crate::fsutil::atomic_write_private;

const SCHEMA_VERSION: u32 = 1;
const DEFAULT_UNREACHABLE_DAYS: u64 = 14;
const DEFAULT_PURGE_DAYS: u64 = 90;
const MS_PER_DAY: u64 = 24 * 60 * 60 * 1000;

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

/// What the registry stores per workspace: only facts that cannot be read
/// back from the disk. The project name and its playbook count are derived at
/// listing time ([`ProjectEntry`]); files written by older builds may still
/// carry `name` / `playbook_count`, which are ignored on read and dropped on
/// the next write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StoredEntry {
    workspace_id: String,
    #[serde(default)]
    fingerprint: Option<String>,
    path: String,
    last_seen_ms: u64,
    state: State,
}

/// A registered workspace as every listing reports it: the stored entry, its
/// state reconciled with the disk by [`is_reachable`], and the derived name
/// and playbook count.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectEntry {
    pub workspace_id: String,
    pub fingerprint: Option<String>,
    pub path: String,
    /// The directory name of `path`.
    pub name: String,
    pub last_seen_ms: u64,
    /// Playbooks on disk right now (0 when the workspace is unreachable).
    pub playbook_count: usize,
    pub state: State,
}

impl ProjectEntry {
    fn from_stored(e: &StoredEntry) -> Self {
        let root = PathBuf::from(&e.path);
        let playbook_count = if e.state == State::Active {
            count_playbooks(&root)
        } else {
            0
        };
        Self {
            workspace_id: e.workspace_id.clone(),
            fingerprint: e.fingerprint.clone(),
            path: e.path.clone(),
            name: workspace_name(&root),
            last_seen_ms: e.last_seen_ms,
            playbook_count,
            state: e.state.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ProjectsFile {
    #[serde(default = "default_schema")]
    schema_version: u32,
    #[serde(default)]
    entries: BTreeMap<String, StoredEntry>,
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

/// All registry operations go through this single point. `f` is first
/// applied to an unlocked snapshot: when it changes nothing (every listing of
/// a settled registry, which the dashboard does every few seconds) the file is
/// neither locked nor written. Otherwise `f` runs again on a fresh read under
/// the lock and the result is written once.
fn with_registry<T>(mut f: impl FnMut(&mut ProjectsFile) -> T) -> std::io::Result<T> {
    let Some(path) = projects_path() else {
        // Configless environment: there is no registry, hand back an empty snapshot.
        let mut empty = ProjectsFile::default();
        return Ok(f(&mut empty));
    };
    let snapshot = read_file(&path);
    let mut probe = snapshot.clone();
    let out = f(&mut probe);
    if probe == snapshot {
        return Ok(out);
    }
    let base = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let _lock = crate::fsutil::lock_dir(&base, "projects.json.lock")?;
    let mut file = read_file(&path);
    let before = file.clone();
    let out = f(&mut file);
    if file != before {
        write_file(&path, &file)?;
    }
    Ok(out)
}

/// The one definition of "this workspace is there", shared by every listing,
/// the dashboard watcher and [`resolve_root`]: the path holds an `.apb`
/// directory whose `workspace.local` names this very workspace. The identity
/// check keeps an edited `projects.json` from pointing a workspace id at some
/// other directory.
fn is_reachable(workspace_id: &str, path: &Path) -> bool {
    path.join(".apb").is_dir()
        && crate::workspace::read_id(path).is_some_and(|id| id == workspace_id)
}

/// Reconciles an entry's stored state with [`is_reachable`]: a reachable
/// workspace is `active`, an unreachable active one becomes `unreachable`
/// from `now`. An entry already unreachable or tombstoned keeps its original
/// `since_ms`, which is what the time-based transitions count from.
fn reconcile(entry: &mut StoredEntry, now: u64) {
    if is_reachable(&entry.workspace_id, Path::new(&entry.path)) {
        entry.state = State::Active;
    } else if entry.state == State::Active {
        entry.state = State::Unreachable { since_ms: now };
    }
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
    let now = crate::clock::now_ms_u64();
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
    let path = root.to_string_lossy().into_owned();
    let now = crate::clock::now_ms_u64();

    let _ = with_registry(|file| {
        apply_time_transitions(file, &cfg);
        // An id already registered to ANOTHER directory that still holds it
        // (a copied checkout, a second clone that kept the file) is not
        // re-pointed here: the last checkout to run a command would otherwise
        // capture every request for that id. A real move leaves the old
        // directory without the id, so the entry follows the workspace.
        if let Some(existing) = file.entries.get(&workspace_id) {
            let old = Path::new(&existing.path);
            let same_dir = std::fs::canonicalize(old).ok() == std::fs::canonicalize(root).ok();
            if !same_dir && is_reachable(&workspace_id, old) {
                return;
            }
        }
        file.entries.insert(
            workspace_id.clone(),
            StoredEntry {
                workspace_id: workspace_id.clone(),
                fingerprint: fingerprint.clone(),
                path: path.clone(),
                last_seen_ms: now,
                state: State::Active,
            },
        );
    });
}

/// Active and unreachable entries (excluding tombstoned), each reconciled
/// with the disk ([`is_reachable`]) and with time-based transitions applied
/// (spec 6.4). The registry is written only when a state actually changed.
pub fn list_active() -> Vec<ProjectEntry> {
    let cfg = GlobalConfig::load().unwrap_or_default();
    let now = crate::clock::now_ms_u64();
    with_registry(|file| {
        for e in file.entries.values_mut() {
            reconcile(e, now);
        }
        apply_time_transitions(file, &cfg);
        file.entries
            .values()
            .filter(|e| !matches!(e.state, State::Tombstoned { .. }))
            .map(ProjectEntry::from_stored)
            .collect()
    })
    .unwrap_or_default()
}

/// The `active` entries of [`list_active`]: the workspaces that are there
/// right now. The global dashboard lists and watches these.
pub fn list_reachable() -> Vec<ProjectEntry> {
    list_active()
        .into_iter()
        .filter(|e| e.state == State::Active)
        .collect()
}

/// Resolves a `workspace_id` to a root path on disk (spec 6.4), reconciling
/// its state the same way the listings do: a reachable workspace is `active`,
/// otherwise it turns `unreachable` and a structured error is returned.
/// Time-based transitions are applied along the way.
pub fn resolve_root(workspace_id: &str) -> Result<PathBuf, ProjectAccessError> {
    let cfg = GlobalConfig::load().unwrap_or_default();
    let now = crate::clock::now_ms_u64();
    with_registry(|file| {
        apply_time_transitions(file, &cfg);
        let Some(entry) = file.entries.get_mut(workspace_id) else {
            return Err(ProjectAccessError::Unknown(workspace_id.to_string()));
        };
        reconcile(entry, now);
        let path = PathBuf::from(&entry.path);
        if entry.state == State::Active {
            Ok(std::fs::canonicalize(&path).unwrap_or(path))
        } else {
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
    fn list_reachable_skips_projects_whose_path_is_gone() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let a = tempfile::tempdir().unwrap();
        init_project(a.path()).unwrap();
        touch(a.path());
        let a_id = crate::workspace::ensure_id(a.path()).unwrap();

        let c = tempfile::tempdir().unwrap();
        init_project(c.path()).unwrap();
        touch(c.path());

        assert_eq!(list_reachable().len(), 2, "both live projects listed");

        // c disappears (a moved/removed workspace, like a throwaway temp dir).
        std::fs::remove_dir_all(c.path().join(".apb")).unwrap();
        let reachable = list_reachable();
        assert_eq!(reachable.len(), 1, "the dead project is filtered out");
        assert_eq!(reachable[0].workspace_id, a_id);
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

        // "Move": the same workspace.local in a new directory while the old
        // one is gone -> a single entry, path updated.
        std::fs::remove_dir_all(a.path().join(".apb")).unwrap();
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

    /// Another checkout cannot take over a live workspace's id: not by a
    /// `workspace.local` that is a symlink to that workspace's file, and not
    /// by a copy of it while the original is still there. Either would route
    /// every request for that id to the other checkout.
    #[cfg(unix)]
    #[test]
    fn another_checkout_cannot_take_over_a_live_workspace_id() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let victim = tempfile::tempdir().unwrap();
        init_project(victim.path()).unwrap();
        touch(victim.path());
        let id = crate::workspace::ensure_id(victim.path()).unwrap();
        let victim_root = std::fs::canonicalize(victim.path()).unwrap();

        let linked = tempfile::tempdir().unwrap();
        init_project(linked.path()).unwrap();
        std::os::unix::fs::symlink(
            victim.path().join(".apb/workspace.local"),
            linked.path().join(".apb/workspace.local"),
        )
        .unwrap();
        touch(linked.path());
        assert_eq!(resolve_root(&id).unwrap(), victim_root, "symlinked id");

        let copied = tempfile::tempdir().unwrap();
        init_project(copied.path()).unwrap();
        std::fs::write(copied.path().join(".apb/workspace.local"), &id).unwrap();
        touch(copied.path());
        assert_eq!(resolve_root(&id).unwrap(), victim_root, "copied id");
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
        let long_ago = crate::clock::now_ms_u64().saturating_sub(15 * MS_PER_DAY);
        test_set_unreachable_since(&ws_id, long_ago);
        let listed = list_active();
        assert!(listed.is_empty(), "tombstoned workspace must not be listed");
    }

    /// Issue #139 F11: every listing answers "is this project there?" with the
    /// same probe. A deleted workspace must not read `active` in the CLI/MCP
    /// listing while the dashboard already hides it.
    #[test]
    fn a_deleted_workspace_is_never_listed_active() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let proj = tempfile::tempdir().unwrap();
        init_project(proj.path()).unwrap();
        touch(proj.path());
        drop(proj);

        let listed = list_active();
        assert_eq!(listed.len(), 1, "a gone workspace is still listed");
        assert!(
            matches!(listed[0].state, State::Unreachable { .. }),
            "a deleted workspace read {:?}",
            listed[0].state
        );
        assert!(list_reachable().is_empty());
    }

    /// Issue #139 F11: one definition of reachable. A workspace `resolve_root`
    /// opens (it has `.apb` and a matching `workspace.local`) is one the
    /// dashboard lists, even before it holds a single playbook.
    #[test]
    fn a_workspace_that_resolves_is_listed_reachable() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let proj = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(proj.path().join(".apb")).unwrap();
        touch(proj.path());
        let ws_id = crate::workspace::ensure_id(proj.path()).unwrap();

        assert!(resolve_root(&ws_id).is_ok());
        let reachable: Vec<String> = list_reachable()
            .into_iter()
            .map(|e| e.workspace_id)
            .collect();
        assert_eq!(reachable, vec![ws_id]);
    }

    /// Issue #139 F11: the playbook count is read from disk, not a copy made
    /// when the CLI last ran in the project (a playbook created through the
    /// dashboard or MCP never updated it).
    #[test]
    fn playbook_count_follows_the_disk() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let proj = tempfile::tempdir().unwrap();
        init_project(proj.path()).unwrap();
        touch(proj.path());
        std::fs::create_dir_all(proj.path().join(".apb/playbooks/later")).unwrap();

        assert_eq!(list_active()[0].playbook_count, 1);
        assert_eq!(list_reachable()[0].playbook_count, 1);
    }

    /// Issue #139 F11: reading the registry does not write it. The dashboard
    /// lists it every 5 s; each read used to rewrite `projects.json`.
    #[test]
    fn listing_an_unchanged_registry_does_not_rewrite_it() {
        use std::os::unix::fs::MetadataExt;
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let live = tempfile::tempdir().unwrap();
        init_project(live.path()).unwrap();
        touch(live.path());
        let gone = tempfile::tempdir().unwrap();
        init_project(gone.path()).unwrap();
        touch(gone.path());
        drop(gone);
        // The first listing records that `gone` became unreachable: a real
        // change, written once.
        let _ = list_active();

        let file = cfg.path().join("projects.json");
        let stamp = |p: &Path| {
            let m = std::fs::metadata(p).unwrap();
            (m.ino(), m.mtime(), m.mtime_nsec())
        };
        let before = stamp(&file);
        let _ = list_active();
        let _ = list_reachable();
        let ws_id = crate::workspace::ensure_id(live.path()).unwrap();
        let _ = resolve_root(&ws_id);
        assert_eq!(stamp(&file), before, "a read rewrote projects.json");
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
