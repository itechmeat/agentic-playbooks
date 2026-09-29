//! Workspace registry (spec 6). Auto-populated file
//! `<config_dir>/projects.json`, keyed by `workspace_id`. Written
//! concurrently by several processes (CLI, MCP, server), so access is
//! serialized via a file lock, writes are atomic, permissions 0600.
//!
//! Several apb builds can share one config dir (an in-place upgrade while the
//! dashboard keeps running on the old binary), so the file is handled
//! defensively (issue #177): a file that exists but cannot be parsed, or that
//! a newer build wrote (`schema_version` above [`SCHEMA_VERSION`]), is never
//! written: it is served as a read-only view and every writer refuses. Fields
//! this build does not know, on the file and on each entry, are kept on write.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::GlobalConfig;
use crate::fsutil::atomic_write_private;

/// The registry format this build reads and writes. 2 (0.22.1) keeps the
/// `name` / `playbook_count` compatibility fields and every unknown field;
/// 1 is what 0.20.x-0.22.0 wrote, read as is and upgraded on the next write.
const SCHEMA_VERSION: u32 = 2;
/// A file without a `schema_version` predates the field.
const LEGACY_SCHEMA_VERSION: u32 = 1;
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

/// What the registry stores per workspace. Listings derive the project name
/// and its playbook count from the disk ([`ProjectEntry`]); the stored `name`
/// and `playbook_count` are compatibility fields only, never read back here.
/// They stay because 0.20.x/0.21.x readers require `name` and, failing to
/// parse an entry without it, used to replace the whole file with an empty
/// registry (issue #177). `extra` keeps every field another build wrote.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StoredEntry {
    workspace_id: String,
    #[serde(default)]
    fingerprint: Option<String>,
    path: String,
    /// Compatibility: the directory name of `path` (filled in on write).
    #[serde(default)]
    name: String,
    last_seen_ms: u64,
    /// Compatibility: the playbook count when the workspace was last touched.
    #[serde(default)]
    playbook_count: usize,
    state: State,
    #[serde(flatten)]
    extra: Map<String, Value>,
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
    #[serde(default = "legacy_schema")]
    schema_version: u32,
    #[serde(default)]
    entries: BTreeMap<String, StoredEntry>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

fn legacy_schema() -> u32 {
    LEGACY_SCHEMA_VERSION
}

impl Default for ProjectsFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            entries: BTreeMap::new(),
            extra: Map::new(),
        }
    }
}

impl ProjectsFile {
    /// Whether the next access writes the file even without a change: it is
    /// in an older format, or an entry lacks the `name` an older reader
    /// requires (a file 0.22.0 wrote). Upgrading once is what keeps a still
    /// running 0.20.x/0.21.x dashboard from choking on it.
    fn needs_upgrade(&self) -> bool {
        self.schema_version < SCHEMA_VERSION || self.entries.values().any(|e| e.name.is_empty())
    }
}

/// The registry file exists but this build must not write it: it does not
/// parse, cannot be read, or a newer build wrote it. The file is left
/// untouched; the reason is logged once per process.
#[derive(Debug, Clone, thiserror::Error)]
#[error("projects registry `{path}` is read-only for this apb: {reason}")]
pub struct RegistryReadOnly {
    pub path: String,
    pub reason: String,
}

/// Why a registry access failed.
#[derive(Debug, thiserror::Error)]
enum RegistryError {
    #[error(transparent)]
    ReadOnly(#[from] RegistryReadOnly),
    #[error(transparent)]
    Io(#[from] std::io::Error),
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

/// What reading `projects.json` produced.
enum Loaded {
    /// A file this build may write (or no file at all).
    Writable(ProjectsFile),
    /// A file this build must never write, with the view to serve instead:
    /// what could be parsed of a newer file, empty for an unparsable one.
    ReadOnly { view: ProjectsFile, reason: String },
}

fn read_file(path: &Path) -> Loaded {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Loaded::Writable(ProjectsFile::default());
        }
        Err(e) => {
            return Loaded::ReadOnly {
                view: ProjectsFile::default(),
                reason: format!("cannot read it ({e})"),
            };
        }
    };
    match serde_json::from_slice::<ProjectsFile>(&raw) {
        Ok(file) if file.schema_version > SCHEMA_VERSION => Loaded::ReadOnly {
            reason: format!(
                "it was written by a newer apb (schema_version {}, this build knows up to {SCHEMA_VERSION})",
                file.schema_version
            ),
            view: file,
        },
        Ok(file) => Loaded::Writable(file),
        Err(e) => {
            // Name the newer format when that is why it does not parse.
            let newer = serde_json::from_slice::<Value>(&raw)
                .ok()
                .and_then(|v| v.get("schema_version").and_then(Value::as_u64))
                .filter(|v| *v > u64::from(SCHEMA_VERSION));
            let reason = match newer {
                Some(v) => format!(
                    "it was written by a newer apb (schema_version {v}, this build knows up to {SCHEMA_VERSION}) and does not parse here ({e})"
                ),
                None => format!("it does not parse ({e})"),
            };
            Loaded::ReadOnly {
                view: ProjectsFile::default(),
                reason,
            }
        }
    }
}

/// Logs a read-only registry once per process and reason: the dashboard
/// lists the registry every few seconds.
fn warn_read_only(path: &Path, reason: &str) {
    static SEEN: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    let key = format!("{}\n{reason}", path.display());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if seen.get_or_insert_with(HashSet::new).insert(key) {
        eprintln!(
            "apb: projects registry `{}` is left untouched: {reason}. Registered workspaces are read-only until the file is fixed, removed, or read by a matching apb.",
            path.display()
        );
    }
}

/// Writes the file in this build's format, filling in the compatibility
/// `name` of any entry that lacks it. Only ever called with a file whose
/// `schema_version` this build knows ([`read_file`] refuses the others).
fn write_file(path: &Path, file: &ProjectsFile) -> std::io::Result<()> {
    let mut out = file.clone();
    out.schema_version = SCHEMA_VERSION;
    for e in out.entries.values_mut() {
        if e.name.is_empty() {
            e.name = workspace_name(Path::new(&e.path));
        }
    }
    let bytes = serde_json::to_vec_pretty(&out).map_err(std::io::Error::other)?;
    atomic_write_private(path, &bytes)
}

/// How a registry access treats a file it must not write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    /// A listing or a lookup: its changes are bookkeeping (reconciled states,
    /// time transitions), so on a read-only file it answers from the view.
    View,
    /// A registration or a removal: on a read-only file it refuses whenever it
    /// would change something.
    Write,
}

/// All registry operations go through this single point. `f` is first
/// applied to an unlocked snapshot: when it changes nothing (every listing of
/// a settled registry, which the dashboard does every few seconds) the file is
/// neither locked nor written. Otherwise `f` runs again on a fresh read under
/// the lock and the result is written once. A file [`read_file`] reports
/// read-only is never written, whatever `f` does.
fn with_registry<T>(
    access: Access,
    mut f: impl FnMut(&mut ProjectsFile) -> T,
) -> Result<T, RegistryError> {
    let Some(path) = projects_path() else {
        // Configless environment: there is no registry, hand back an empty snapshot.
        let mut empty = ProjectsFile::default();
        return Ok(f(&mut empty));
    };
    let snapshot = match read_file(&path) {
        Loaded::Writable(file) => file,
        Loaded::ReadOnly { view, reason } => return read_only(access, &path, view, reason, f),
    };
    let mut probe = snapshot.clone();
    let out = f(&mut probe);
    if probe == snapshot && !snapshot.needs_upgrade() {
        return Ok(out);
    }
    let base = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let _lock = crate::fsutil::lock_dir(&base, "projects.json.lock")?;
    let mut file = match read_file(&path) {
        Loaded::Writable(file) => file,
        Loaded::ReadOnly { view, reason } => return read_only(access, &path, view, reason, f),
    };
    let before = file.clone();
    let out = f(&mut file);
    if file != before || before.needs_upgrade() {
        write_file(&path, &file)?;
    }
    Ok(out)
}

/// [`with_registry`] on a file it must not write: `f` runs on the in-memory
/// view, nothing is written, and a writer that changed something is refused.
fn read_only<T>(
    access: Access,
    path: &Path,
    mut view: ProjectsFile,
    reason: String,
    mut f: impl FnMut(&mut ProjectsFile) -> T,
) -> Result<T, RegistryError> {
    warn_read_only(path, &reason);
    let before = view.clone();
    let out = f(&mut view);
    if access == Access::Write && view != before {
        return Err(RegistryReadOnly {
            path: path.display().to_string(),
            reason,
        }
        .into());
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
    // The compatibility fields, computed as 0.20.x/0.21.x did at registration.
    let name = workspace_name(root);
    let playbook_count = count_playbooks(root);
    let now = crate::clock::now_ms_u64();

    let _ = with_registry(Access::Write, |file| {
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
        // Fields another build stored on this entry survive re-registration.
        let extra = file
            .entries
            .get(&workspace_id)
            .map(|e| e.extra.clone())
            .unwrap_or_default();
        file.entries.insert(
            workspace_id.clone(),
            StoredEntry {
                workspace_id: workspace_id.clone(),
                fingerprint: fingerprint.clone(),
                path: path.clone(),
                name: name.clone(),
                last_seen_ms: now,
                playbook_count,
                state: State::Active,
                extra,
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
    with_registry(Access::View, |file| {
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
    with_registry(Access::View, |file| {
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

/// Manual removal of an entry (for `apb projects remove`). Returns `true` if
/// the entry existed, and an error when the registry is read-only for this
/// build and the entry is there (so it cannot be removed).
pub fn remove(workspace_id: &str) -> Result<bool, RegistryReadOnly> {
    match with_registry(Access::Write, |file| {
        file.entries.remove(workspace_id).is_some()
    }) {
        Ok(removed) => Ok(removed),
        Err(RegistryError::ReadOnly(e)) => Err(e),
        Err(RegistryError::Io(_)) => Ok(false),
    }
}

#[cfg(test)]
pub(crate) fn test_set_unreachable_since(workspace_id: &str, since_ms: u64) {
    let _ = with_registry(Access::Write, |file| {
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

    // --- issue #177: a registry this build cannot or must not write ---------

    fn registry_file(cfg: &Path) -> PathBuf {
        cfg.join("projects.json")
    }

    /// Every operation a CLI, MCP or dashboard process runs against the
    /// registry, including the writers.
    fn touch_everything(workspace: &Path, ws_id: &str) {
        touch(workspace);
        let _ = remove(ws_id);
        let _ = remove("ws-registered-elsewhere");
        let _ = list_active();
        let _ = list_reachable();
        let _ = resolve_root(ws_id);
        let _ = resolve_root("ws-registered-elsewhere");
    }

    /// The entry shape 0.20.x/0.21.x require: `name` is mandatory there.
    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct OldReaderEntry {
        workspace_id: String,
        #[serde(default)]
        fingerprint: Option<String>,
        path: String,
        name: String,
        last_seen_ms: u64,
        #[serde(default)]
        playbook_count: usize,
        state: State,
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct OldReaderFile {
        schema_version: u32,
        entries: BTreeMap<String, OldReaderEntry>,
    }

    #[test]
    fn a_v1_file_written_by_an_old_build_is_read_and_upgraded_in_place() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let proj = tempfile::tempdir().unwrap();
        init_project(proj.path()).unwrap();
        let ws_id = crate::workspace::ensure_id(proj.path()).unwrap();
        // Exactly what 0.21.0 wrote after `apb runs` in this workspace.
        let old = serde_json::json!({
            "schema_version": 1,
            "entries": { ws_id.clone(): {
                "workspace_id": ws_id,
                "fingerprint": null,
                "path": proj.path().to_string_lossy(),
                "name": "old-name",
                "last_seen_ms": 1,
                "playbook_count": 7,
                "state": { "kind": "active" }
            }}
        });
        std::fs::write(registry_file(cfg.path()), old.to_string()).unwrap();

        let listed = list_active();
        assert_eq!(listed.len(), 1, "the old file is read");
        assert_eq!(listed[0].workspace_id, ws_id);
        assert_eq!(listed[0].state, State::Active);

        let written: Value =
            serde_json::from_slice(&std::fs::read(registry_file(cfg.path())).unwrap()).unwrap();
        assert_eq!(
            written["schema_version"], SCHEMA_VERSION,
            "upgraded on first access"
        );
        let entry = &written["entries"][ws_id.as_str()];
        assert_eq!(entry["name"], "old-name", "compatibility name kept");
        assert_eq!(entry["playbook_count"], 7, "compatibility count kept");
    }

    /// What this build writes is still a registry 0.20.x/0.21.x parse: each
    /// entry carries `name` and `playbook_count`, computed as they did, and the
    /// entries a 0.22.0 build left without them are filled in.
    #[test]
    fn a_written_registry_parses_with_the_old_readers_shape() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let bare = tempfile::tempdir().unwrap();
        init_project(bare.path()).unwrap();
        let bare_id = crate::workspace::ensure_id(bare.path()).unwrap();
        // What 0.22.0 wrote: no `name`, no `playbook_count`, still version 1.
        let v0220 = serde_json::json!({
            "schema_version": 1,
            "entries": { bare_id.clone(): {
                "workspace_id": bare_id,
                "fingerprint": null,
                "path": bare.path().to_string_lossy(),
                "last_seen_ms": 1,
                "state": { "kind": "active" }
            }}
        });
        std::fs::write(registry_file(cfg.path()), v0220.to_string()).unwrap();

        let proj = tempfile::tempdir().unwrap();
        init_project(proj.path()).unwrap();
        std::fs::create_dir_all(proj.path().join(".apb/playbooks/second")).unwrap();
        touch(proj.path());
        let ws_id = crate::workspace::ensure_id(proj.path()).unwrap();

        let raw = std::fs::read(registry_file(cfg.path())).unwrap();
        let old: OldReaderFile = serde_json::from_slice(&raw)
            .unwrap_or_else(|e| panic!("a 0.21.x reader rejects the file: {e}"));
        assert_eq!(old.schema_version, SCHEMA_VERSION);
        let e = &old.entries[&ws_id];
        assert_eq!(e.name, workspace_name(proj.path()));
        assert_eq!(e.playbook_count, count_playbooks(proj.path()));
        assert_eq!(old.entries[&bare_id].name, workspace_name(bare.path()));
    }

    #[test]
    fn a_malformed_registry_is_left_byte_identical_by_every_operation() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let proj = tempfile::tempdir().unwrap();
        init_project(proj.path()).unwrap();
        let ws_id = crate::workspace::ensure_id(proj.path()).unwrap();
        // Truncated mid-write, and a shape an old or foreign build left.
        for raw in [
            "{\"schema_version\": 1, \"entries\": {".to_string(),
            serde_json::json!({"schema_version": 1, "entries": {"ws-registered-elsewhere": {"path": 3}}}).to_string(),
        ] {
            std::fs::write(registry_file(cfg.path()), &raw).unwrap();
            touch_everything(proj.path(), &ws_id);
            assert_eq!(
                std::fs::read_to_string(registry_file(cfg.path())).unwrap(),
                raw,
                "a registry that does not parse was rewritten"
            );
            assert!(list_active().is_empty(), "served as an empty read-only view");
        }
    }

    #[test]
    fn unknown_fields_survive_a_round_trip() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let proj = tempfile::tempdir().unwrap();
        init_project(proj.path()).unwrap();
        let ws_id = crate::workspace::ensure_id(proj.path()).unwrap();
        let file = serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "future_file_field": { "nested": [1, 2] },
            "entries": {
                ws_id.clone(): {
                    "workspace_id": ws_id,
                    "path": proj.path().to_string_lossy(),
                    "name": "p",
                    "last_seen_ms": 1,
                    "state": { "kind": "active" },
                    "future_entry_field": "kept"
                },
                "ws-registered-elsewhere": {
                    "workspace_id": "ws-registered-elsewhere",
                    "path": "/nowhere",
                    "name": "nowhere",
                    "last_seen_ms": 1,
                    "state": { "kind": "unreachable", "since_ms": 1 },
                    "future_entry_field": 42
                }
            }
        });
        std::fs::write(registry_file(cfg.path()), file.to_string()).unwrap();

        // A write (re-registration) and a listing that reconciles states.
        touch(proj.path());
        let _ = list_active();

        let written: Value =
            serde_json::from_slice(&std::fs::read(registry_file(cfg.path())).unwrap()).unwrap();
        assert_eq!(
            written["future_file_field"],
            serde_json::json!({ "nested": [1, 2] })
        );
        assert_eq!(
            written["entries"][ws_id.as_str()]["future_entry_field"],
            "kept"
        );
        assert_ne!(
            written["entries"][ws_id.as_str()]["last_seen_ms"],
            1,
            "the touch really wrote the file"
        );
        assert_eq!(
            written["entries"]["ws-registered-elsewhere"]["future_entry_field"],
            42
        );
    }

    #[test]
    fn a_registry_from_a_newer_schema_is_read_but_never_written() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let proj = tempfile::tempdir().unwrap();
        init_project(proj.path()).unwrap();
        let ws_id = crate::workspace::ensure_id(proj.path()).unwrap();
        let newer = serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": SCHEMA_VERSION + 1,
            "entries": {
                ws_id.clone(): {
                    "workspace_id": ws_id,
                    "path": proj.path().to_string_lossy(),
                    "last_seen_ms": 1,
                    "state": { "kind": "active" }
                },
                "ws-registered-elsewhere": {
                    "workspace_id": "ws-registered-elsewhere",
                    "path": "/nowhere",
                    "last_seen_ms": 1,
                    "state": { "kind": "active" }
                }
            }
        }))
        .unwrap();
        std::fs::write(registry_file(cfg.path()), &newer).unwrap();

        // Read: the listing and the lookup answer from the file as it is,
        // reconciled in memory only.
        let listed = list_active();
        assert_eq!(listed.len(), 2, "a newer registry is still read");
        assert!(resolve_root(&ws_id).is_ok());
        // Never written: a writer that would change it refuses.
        assert!(
            remove(&ws_id).is_err(),
            "removal from a newer file must refuse"
        );
        assert_eq!(remove("ws-not-there").ok(), Some(false));
        touch_everything(proj.path(), &ws_id);
        assert_eq!(
            std::fs::read_to_string(registry_file(cfg.path())).unwrap(),
            newer,
            "a registry from a newer schema was rewritten"
        );
    }

    /// Issue #177 step 4: once upgraded, a settled registry is not rewritten
    /// by listings (the upgrade itself is one write, not one per listing).
    #[test]
    fn the_upgrade_is_written_once() {
        use std::os::unix::fs::MetadataExt;
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let proj = tempfile::tempdir().unwrap();
        init_project(proj.path()).unwrap();
        let ws_id = crate::workspace::ensure_id(proj.path()).unwrap();
        let v1 = serde_json::json!({
            "schema_version": 1,
            "entries": { ws_id.clone(): {
                "workspace_id": ws_id,
                "path": proj.path().to_string_lossy(),
                "last_seen_ms": 1,
                "state": { "kind": "active" }
            }}
        });
        let file = registry_file(cfg.path());
        std::fs::write(&file, v1.to_string()).unwrap();
        let _ = list_active();
        let stamp = |p: &Path| {
            let m = std::fs::metadata(p).unwrap();
            (m.ino(), m.mtime(), m.mtime_nsec())
        };
        let upgraded = stamp(&file);
        let _ = list_active();
        let _ = list_reachable();
        let _ = resolve_root(&ws_id);
        assert_eq!(
            stamp(&file),
            upgraded,
            "a listing rewrote the upgraded file"
        );
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
