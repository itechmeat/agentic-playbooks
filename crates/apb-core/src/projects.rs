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
/// An entry unreachable this long is dropped by the next registry write,
/// unless the operator configured `registry_unreachable_days` or
/// `registry_purge_days`, in which case the limit is at least
/// [`drop_days`]: max(30, unreachable + purge). Every drop re-checks
/// reachability first, so an entry never disappears while its workspace is
/// there again.
const DROP_UNREACHABLE_DAYS: u64 = 30;
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
    /// The workspace id of the registered project whose git repository this
    /// workspace is a linked worktree of, recorded at registration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_workspace_id: Option<String>,
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
    /// Set when this workspace is a git linked worktree of the registered
    /// project with this workspace id: listings nest it under that project.
    pub parent_workspace_id: Option<String>,
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
            parent_workspace_id: e.parent_workspace_id.clone(),
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
        // An entry whose path yields no name at all cannot be filled in, and
        // must not make every listing rewrite the file.
        self.schema_version < SCHEMA_VERSION
            || self
                .entries
                .values()
                .any(|e| e.name.is_empty() && !workspace_name(Path::new(&e.path)).is_empty())
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

/// Why `apb projects prune` failed. A failed read or write is reported to
/// the caller instead of an empty removed list, which a CLI would show as
/// "nothing to prune" with a successful exit.
#[derive(Debug, thiserror::Error)]
pub enum PruneError {
    /// The registry is read-only for this build and something would be
    /// removed.
    #[error(transparent)]
    ReadOnly(#[from] RegistryReadOnly),
    /// Reading or writing the registry failed.
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
    crate::process_mode::guard()?;
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
    // A read-only process answers a listing from the view and refuses a
    // registration or a removal that would change the file.
    if crate::process_mode::is_read_only() {
        if access == Access::Write && probe != snapshot {
            return Err(RegistryReadOnly {
                path: path.display().to_string(),
                reason: crate::process_mode::READ_ONLY_REFUSAL.to_string(),
            }
            .into());
        }
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
        match GlobalConfig::load() {
            Ok(cfg) => drop_long_unreachable(&mut file, &cfg, crate::clock::now_ms_u64()),
            Err(e) => {
                // A config that does not parse must not silently fall back
                // to the plain 30-day drop limit: a user who configured a
                // longer purge would lose entries early with no diagnostic.
                // Skip the auto-drop for this write; the configured limits
                // are applied again once the config loads.
                eprintln!(
                    "apb: projects registry auto-drop skipped, config.yaml failed to load: {e}"
                );
            }
        }
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

/// The day count the long-unreachable auto-drop uses. Plain 30, but when the
/// operator configured `registry_unreachable_days` or
/// `registry_purge_days`, at least the sum of the two: a user who asked for a
/// 365-day purge must not lose entries at 30 days.
fn drop_days(cfg: &GlobalConfig) -> u64 {
    if cfg.registry_unreachable_days.is_none() && cfg.registry_purge_days.is_none() {
        return DROP_UNREACHABLE_DAYS;
    }
    let unreach = cfg
        .registry_unreachable_days
        .unwrap_or(DEFAULT_UNREACHABLE_DAYS);
    let purge = cfg.registry_purge_days.unwrap_or(DEFAULT_PURGE_DAYS);
    DROP_UNREACHABLE_DAYS.max(unreach.saturating_add(purge))
}

/// Removes the entries unreachable for at least [`drop_days`]. A tombstoned
/// entry was unreachable for the tombstone threshold before its `since_ms`,
/// so that lower bound is what counts for it. An entry past its limit is
/// re-checked against the disk first: a workspace that came back stays, as
/// active again, and only one that is still gone is dropped.
fn drop_long_unreachable(file: &mut ProjectsFile, cfg: &GlobalConfig, now: u64) {
    let limit = drop_days(cfg).saturating_mul(MS_PER_DAY);
    let unreach = unreachable_ms(cfg);
    let mut drop_ids = Vec::new();
    for (id, e) in file.entries.iter_mut() {
        let over = match &e.state {
            State::Active => continue,
            State::Unreachable { since_ms } => now.saturating_sub(*since_ms) >= limit,
            State::Tombstoned { since_ms } => {
                now.saturating_sub(*since_ms).saturating_add(unreach) >= limit
            }
        };
        if !over {
            continue;
        }
        if is_reachable(id, Path::new(&e.path)) {
            // It came back since it was marked unreachable: keep it, active
            // again.
            e.state = State::Active;
        } else if verifiably_gone(Path::new(&e.path)) {
            drop_ids.push(id.clone());
        }
    }
    for id in drop_ids {
        file.entries.remove(&id);
    }
}

/// The only fact the long-unreachable auto-drop may drop an entry on: the
/// workspace path itself stats as absent while its parent directory still
/// stats. A stat error of any other kind - an unmounted disk, a down sshfs,
/// a permission problem - is never proof the directory is gone, so the
/// entry stays ([`prune`]'s `NotFound` rule, applied here too).
fn verifiably_gone(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            path.parent().is_some_and(|p| p.is_dir())
        }
        _ => false,
    }
}

/// Transitions `unreachable` entries older than the threshold to
/// `tombstoned`, physically removes `tombstoned` entries older than the
/// purge threshold, and drops the long unreachable ones
/// ([`drop_long_unreachable`]). Called inside the lock on every access.
fn apply_time_transitions(file: &mut ProjectsFile, cfg: &GlobalConfig) {
    let now = crate::clock::now_ms_u64();
    drop_long_unreachable(file, cfg, now);
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
    if crate::process_mode::is_read_only() {
        return;
    }
    let cfg = GlobalConfig::load().unwrap_or_default();
    if registration_disabled(&cfg) {
        return;
    }
    let Ok(workspace_id) = crate::workspace::ensure_id(root) else {
        return;
    };
    let now = crate::clock::now_ms_u64();

    let _ = with_registry(Access::Write, |file| {
        apply_time_transitions(file, &cfg);
        register_in(file, &workspace_id, root, now);
    });
}

/// Inserts (or refreshes) the entry of `workspace_id` at `root`, unless the
/// id is registered to ANOTHER directory that still holds it (a copied
/// checkout, a second clone that kept the file): the last checkout to run a
/// command would otherwise capture every request for that id. A real move
/// leaves the old directory without the id, so the entry follows the
/// workspace. Fields another build stored on the entry survive. Returns
/// whether the id is registered at `root` afterwards.
fn register_in(file: &mut ProjectsFile, workspace_id: &str, root: &Path, now: u64) -> bool {
    if let Some(existing) = file.entries.get(workspace_id) {
        let old = Path::new(&existing.path);
        let same_dir = std::fs::canonicalize(old).ok() == std::fs::canonicalize(root).ok();
        if !same_dir && is_reachable(workspace_id, old) {
            return false;
        }
    }
    let extra = file
        .entries
        .get(workspace_id)
        .map(|e| e.extra.clone())
        .unwrap_or_default();
    let previous_parent = file
        .entries
        .get(workspace_id)
        .and_then(|e| e.parent_workspace_id.clone());
    let parent_workspace_id = match worktree_parent(file, workspace_id, root) {
        // git could not be asked: keep what was recorded, never erase it on
        // a transient failure.
        ParentLookup::Unknown => previous_parent.filter(|p| *p != workspace_id),
        ParentLookup::At(id) => Some(id),
        ParentLookup::None => None,
    };
    file.entries.insert(
        workspace_id.to_string(),
        StoredEntry {
            workspace_id: workspace_id.to_string(),
            fingerprint: crate::workspace::fingerprint(root),
            path: root.to_string_lossy().into_owned(),
            // The compatibility fields, computed as 0.20.x/0.21.x did.
            name: workspace_name(root),
            last_seen_ms: now,
            playbook_count: count_playbooks(root),
            state: State::Active,
            parent_workspace_id,
            extra,
        },
    );
    true
}

/// What [`worktree_parent`] learned about the parent of a worktree.
enum ParentLookup {
    /// git worked and named the registered parent project.
    At(String),
    /// git worked and there is no registered parent: a main checkout, a
    /// directory outside git, or an unregistered parent.
    None,
    /// git could not be asked (it would not start or failed): the caller
    /// keeps the previously recorded parent instead of clearing it.
    Unknown,
}

/// The workspace id of the registered project `root` is a git linked
/// worktree of: [`parent_root`] finds the parent project's directory, and an
/// entry other than `workspace_id` registered there is the parent.
fn worktree_parent(file: &ProjectsFile, workspace_id: &str, root: &Path) -> ParentLookup {
    match parent_root(root) {
        ParentRoot::At(parent_root) => parent_entry(file, workspace_id, &parent_root),
        ParentRoot::None => ParentLookup::None,
        ParentRoot::Unknown => ParentLookup::Unknown,
    }
}

/// The registered entry, other than `workspace_id`, at `parent_root`.
fn parent_entry(file: &ProjectsFile, workspace_id: &str, parent_root: &Path) -> ParentLookup {
    file.entries
        .values()
        .find(|e| e.workspace_id != workspace_id && entry_at(e, parent_root))
        .map(|e| ParentLookup::At(e.workspace_id.clone()))
        .unwrap_or(ParentLookup::None)
}

/// What [`parent_root`] learned from git about a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ParentRoot {
    /// A linked worktree whose parent project would sit at this canonical
    /// directory of the main checkout.
    At(PathBuf),
    /// A main checkout or a directory outside git.
    None,
    /// git could not be asked.
    Unknown,
}

/// Where the parent project of `root` sits when `root` is in a git linked
/// worktree. A linked worktree's top holds a `.git` file, a main checkout's a
/// directory: only the former is worth starting git for ([`ask_git`]).
/// [`ParentRoot::None`] for a main checkout or a directory outside git.
fn parent_root(root: &Path) -> ParentRoot {
    match linked_dot_git(root) {
        Some(_) => ask_git(root),
        None => ParentRoot::None,
    }
}

/// The `.git` file of the linked worktree `root` is in: the nearest `.git`
/// among `root` and its ancestors, when it is a file.
fn linked_dot_git(root: &Path) -> Option<PathBuf> {
    root.ancestors()
        .map(|d| d.join(".git"))
        .find(|g| g.exists())
        .filter(|g| g.is_file())
}

/// git's answer for a directory in a linked worktree: git reports a git dir
/// apart from the common dir, the main worktree is the common dir's parent,
/// and the parent project sits at the same place inside it as `root` does
/// inside its worktree. Asked through [`crate::git::Repo`], never a plain
/// `git`. [`ParentRoot::Unknown`] when git cannot open what `.git` points
/// to.
fn ask_git(root: &Path) -> ParentRoot {
    let Ok(repo) = crate::git::Repo::locate(root) else {
        return ParentRoot::Unknown;
    };
    if repo.git_dir == repo.common_dir {
        return ParentRoot::None;
    }
    let Some(main_tree) = repo.common_dir.parent() else {
        return ParentRoot::Unknown;
    };
    let Ok(canon) = std::fs::canonicalize(root) else {
        return ParentRoot::Unknown;
    };
    let Ok(inside) = canon.strip_prefix(&repo.work_tree) else {
        return ParentRoot::Unknown;
    };
    match std::fs::canonicalize(main_tree.join(inside)) {
        Ok(parent_root) => ParentRoot::At(parent_root),
        Err(_) => ParentRoot::Unknown,
    }
}

/// The identity of a linked worktree's `.git` file: its bytes (the `gitdir:`
/// line naming the main checkout) and its modification time. `git worktree
/// add`, `move` and `repair` rewrite the file, so a git answer kept under an
/// equal stamp still holds.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DotGitStamp {
    bytes: Vec<u8>,
    modified: Option<std::time::SystemTime>,
}

/// A `.git` file longer than this is not one git wrote; it is never stamped,
/// so git is asked every time.
const DOT_GIT_MAX: u64 = 4096;

fn dot_git_stamp(dot_git: &Path) -> Option<DotGitStamp> {
    use std::io::Read;
    let file = std::fs::File::open(dot_git).ok()?;
    let modified = file.metadata().ok()?.modified().ok();
    let mut bytes = Vec::new();
    file.take(DOT_GIT_MAX + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= DOT_GIT_MAX).then_some(DotGitStamp { bytes, modified })
}

/// What git said about each orphaned linked worktree in this process, under
/// the stamp of its `.git` file at the time: a dashboard lists the registry
/// every few seconds and must not start git for the same worktree each time.
/// An answer stands only while the stamp is equal; a failure to ask git is
/// not kept; [`backfill_parents`] keeps only the entries still orphaned, so
/// the map is never larger than the registry. A workspace's own touch always
/// asks git afresh.
static PARENT_ROOTS: Mutex<BTreeMap<PathBuf, (DotGitStamp, ParentRoot)>> =
    Mutex::new(BTreeMap::new());

fn cached_parent_root(root: &Path) -> ParentRoot {
    let Some(dot_git) = linked_dot_git(root) else {
        return ParentRoot::None;
    };
    // Stamped before git is asked: a `.git` rewritten meanwhile leaves an
    // older stamp, so the next listing asks again.
    let Some(stamp) = dot_git_stamp(&dot_git) else {
        return ask_git(root);
    };
    let known = PARENT_ROOTS.lock().ok().and_then(|m| {
        m.get(root)
            .filter(|(kept, _)| *kept == stamp)
            .map(|(_, answer)| answer.clone())
    });
    if let Some(known) = known {
        return known;
    }
    let found = ask_git(root);
    if found != ParentRoot::Unknown
        && let Ok(mut m) = PARENT_ROOTS.lock()
    {
        m.insert(root.to_path_buf(), (stamp, found.clone()));
    }
    found
}

/// Fills in the parent of every active entry that has none and is a git
/// linked worktree of a registered project: entries registered before
/// worktree nesting existed, or before their parent project was registered,
/// and not touched since. A recorded parent is never changed here; only a
/// touch of the workspace itself does that.
///
/// `asked` carries git's answers across the passes of one
/// [`with_registry`] call: the first pass, over the snapshot read before the
/// lock, asks; a pass under the lock only reads them, so git never runs while
/// other registry writers wait. An entry first seen under the lock is left
/// for the next listing. The change is written once, under the lock,
/// atomically.
fn backfill_parents(file: &mut ProjectsFile, asked: &mut Option<BTreeMap<PathBuf, ParentRoot>>) {
    let orphans: Vec<(String, PathBuf)> = file
        .entries
        .values()
        .filter(|e| e.state == State::Active && e.parent_workspace_id.is_none())
        .map(|e| (e.workspace_id.clone(), PathBuf::from(&e.path)))
        .collect();
    let answers = asked.get_or_insert_with(|| {
        if let Ok(mut m) = PARENT_ROOTS.lock() {
            m.retain(|root, _| orphans.iter().any(|(_, path)| path == root));
        }
        orphans
            .iter()
            .map(|(_, path)| (path.clone(), cached_parent_root(path)))
            .collect()
    });
    for (id, path) in orphans {
        let Some(ParentRoot::At(parent_root)) = answers.get(&path) else {
            continue;
        };
        if let ParentLookup::At(parent) = parent_entry(file, &id, parent_root)
            && let Some(e) = file.entries.get_mut(&id)
        {
            e.parent_workspace_id = Some(parent);
        }
    }
}

/// Whether `entry` names the directory `canon` (already canonical).
fn entry_at(entry: &StoredEntry, canon: &Path) -> bool {
    std::fs::canonicalize(&entry.path).ok().as_deref() == Some(canon)
}

/// The workspace id of `root` as the launch floors and the machine's exact
/// `launch:` entries key it: the id in `.apb/workspace.local` only when it
/// has the shape apb mints and the projects registry under the config dir
/// has it registered at this very root. A file in the project tree alone
/// never names another project's id. `None` otherwise.
pub fn registered_id(root: &Path) -> Option<String> {
    registered_id_in(crate::config::config_dir().as_deref(), root)
}

/// [`registered_id`] against the registry of the config dir `config_dir`.
pub fn registered_id_in(config_dir: Option<&Path>, root: &Path) -> Option<String> {
    let id = crate::workspace::read_id(root)?;
    if !crate::workspace::is_well_formed_id(&id) {
        return None;
    }
    let canon = std::fs::canonicalize(root).ok()?;
    let file = match read_file(&config_dir?.join("projects.json")) {
        Loaded::Writable(file) => file,
        Loaded::ReadOnly { view, .. } => view,
    };
    file.entries
        .get(&id)
        .is_some_and(|e| entry_at(e, &canon))
        .then_some(id)
}

/// [`registered_id`], registering the workspace at `root` first when it is
/// not yet (whatever the listing opt-out says: a floor needs its owner
/// bound): what a writer of a launch floor keys it by. Refused when the id
/// is not one apb minted, when another live directory holds the same id,
/// or when the registry cannot be written.
pub fn bind_id(root: &Path) -> std::io::Result<String> {
    let id = crate::workspace::ensure_id(root)?;
    if !crate::workspace::is_well_formed_id(&id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "`{}` holds `{id}`, which is not a workspace id apb minted; delete the file so apb mints one",
                root.join(".apb/workspace.local").display()
            ),
        ));
    }
    if registered_id(root).as_deref() == Some(id.as_str()) {
        return Ok(id);
    }
    if projects_path().is_none() {
        return Err(std::io::Error::other(
            "no config dir to register the workspace in",
        ));
    }
    let now = crate::clock::now_ms_u64();
    let bound = with_registry(Access::Write, |file| register_in(file, &id, root, now))
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    if !bound {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "workspace id `{id}` in `{}` is registered to another project that still holds it; delete the file here so apb mints this checkout its own",
                root.join(".apb/workspace.local").display()
            ),
        ));
    }
    Ok(id)
}

/// Active and unreachable entries (excluding tombstoned), each reconciled
/// with the disk ([`is_reachable`]) and with time-based transitions applied
/// (spec 6.4), and the worktree parent filled in where an entry lacks it
/// ([`backfill_parents`]). The registry is written only when a state or a
/// parent actually changed.
pub fn list_active() -> Vec<ProjectEntry> {
    let cfg = GlobalConfig::load().unwrap_or_default();
    let now = crate::clock::now_ms_u64();
    let mut asked = None;
    with_registry(Access::View, |file| {
        for e in file.entries.values_mut() {
            reconcile(e, now);
        }
        apply_time_transitions(file, &cfg);
        backfill_parents(file, &mut asked);
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

/// `apb projects prune`: removes every entry whose directory is genuinely
/// gone (`NotFound`), reconciled with [`is_reachable`]. An entry whose path
/// still exists but is momentarily unreachable (an unmounted disk, a down
/// sshfs, no permission) is kept and only marked unreachable, so a transient
/// outage never drops registered projects. Each removal goes the way
/// [`remove`] removes one, under the registry lock. Returns the removed
/// entries. `dry_run` reports the same list and writes nothing. Refused when
/// the registry is read-only for this build and something would be removed;
/// I/O errors surface instead of an empty list.
pub fn prune(dry_run: bool) -> Result<Vec<ProjectEntry>, PruneError> {
    let now = crate::clock::now_ms_u64();
    let gone = |file: &mut ProjectsFile| -> Vec<ProjectEntry> {
        let mut removed = Vec::new();
        let ids: Vec<String> = file.entries.keys().cloned().collect();
        for id in ids {
            let Some(entry) = file.entries.get_mut(&id) else {
                continue;
            };
            reconcile(entry, now);
            let genuinely_gone = std::fs::metadata(&entry.path)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound);
            if genuinely_gone
                && entry.state != State::Active
                && let Some(e) = file.entries.remove(&id)
            {
                removed.push(ProjectEntry::from_stored(&e));
            }
        }
        removed
    };
    if dry_run {
        let Some(path) = projects_path() else {
            return Ok(Vec::new());
        };
        let mut file = match read_file(&path) {
            Loaded::Writable(file) => file,
            Loaded::ReadOnly { view, .. } => view,
        };
        return Ok(gone(&mut file));
    }
    match with_registry(Access::Write, gone) {
        Ok(removed) => Ok(removed),
        Err(RegistryError::ReadOnly(e)) => Err(e.into()),
        Err(RegistryError::Io(e)) => Err(e.into()),
    }
}

/// `entries` ordered for display: each main project followed by the
/// worktrees nested under it (depth 1). Main projects and each group of
/// worktrees are sorted by byte order of the label (`str` ordering over the
/// `name` field, then the `path`), never by locale-aware comparison; the web
/// project picker pairs this rule with JavaScript `localeCompare` for its own
/// display. A worktree whose parent is not among `entries` is listed as a
/// main project.
pub fn nest_worktrees(entries: Vec<ProjectEntry>) -> Vec<(usize, ProjectEntry)> {
    let ids: HashSet<String> = entries.iter().map(|e| e.workspace_id.clone()).collect();
    let nested_under = |e: &ProjectEntry| {
        e.parent_workspace_id
            .clone()
            .filter(|p| *p != e.workspace_id && ids.contains(p))
    };
    let (mut mains, children): (Vec<_>, Vec<_>) =
        entries.into_iter().partition(|e| nested_under(e).is_none());
    let mut by_parent: BTreeMap<String, Vec<ProjectEntry>> = BTreeMap::new();
    for c in children {
        by_parent
            .entry(nested_under(&c).unwrap_or_default())
            .or_default()
            .push(c);
    }
    let key = |e: &ProjectEntry| (e.name.clone(), e.path.clone());
    mains.sort_by_key(key);
    let mut out = Vec::new();
    for m in mains {
        let mut kids = by_parent.remove(&m.workspace_id).unwrap_or_default();
        kids.sort_by_key(key);
        out.push((0, m));
        out.extend(kids.into_iter().map(|k| (1, k)));
    }
    // A parent that is itself nested (not a shape git makes) keeps its own
    // worktrees listed, at the top level.
    out.extend(by_parent.into_values().flatten().map(|k| (0, k)));
    out
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
    use std::os::unix::fs::PermissionsExt;

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
        // Unreachable for a day: recent enough that a write keeps it.
        let since = crate::clock::now_ms_u64() - MS_PER_DAY;
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
                    "state": { "kind": "unreachable", "since_ms": since },
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

    /// An entry whose path yields no name cannot be filled in; it must not
    /// count as a pending upgrade, or every listing would rewrite the file.
    #[test]
    fn an_entry_without_a_derivable_name_does_not_force_rewrites() {
        use std::os::unix::fs::MetadataExt;
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;
        let file = registry_file(cfg.path());
        let v2 = serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "entries": { "ws-nameless": {
                "workspace_id": "ws-nameless",
                "path": "",
                "last_seen_ms": 1,
                "state": { "kind": "tombstoned", "since_ms": crate::clock::now_ms_u64() }
            }}
        });
        std::fs::write(&file, v2.to_string()).unwrap();
        let stamp = |p: &Path| {
            let m = std::fs::metadata(p).unwrap();
            (m.ino(), m.mtime(), m.mtime_nsec())
        };
        let before = stamp(&file);
        let _ = list_active();
        let _ = list_active();
        assert_eq!(stamp(&file), before, "a listing rewrote the file");
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    /// A git linked worktree of a registered repository records the parent
    /// project's workspace id; the main checkout records none.
    #[test]
    fn a_linked_worktree_records_its_parent_project() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let base = tempfile::tempdir().unwrap();
        let main = base.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q"]);
        std::fs::write(main.join(".gitignore"), ".apb/workspace.local\n").unwrap();
        git(&main, &["add", ".gitignore"]);
        git(&main, &["commit", "-q", "-m", "init"]);
        init_project(&main).unwrap();
        touch(&main);
        let main_id = crate::workspace::ensure_id(&main).unwrap();

        let wt = base.path().join("wt");
        git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
        init_project(&wt).unwrap();
        touch(&wt);
        let wt_id = crate::workspace::ensure_id(&wt).unwrap();
        assert_ne!(wt_id, main_id);

        let listed = list_active();
        let of = |id: &str| listed.iter().find(|e| e.workspace_id == id).unwrap();
        assert_eq!(
            of(&wt_id).parent_workspace_id.as_deref(),
            Some(main_id.as_str())
        );
        assert_eq!(of(&main_id).parent_workspace_id, None);

        let nested: Vec<(usize, String)> = nest_worktrees(listed)
            .into_iter()
            .map(|(d, e)| (d, e.workspace_id))
            .collect();
        assert_eq!(nested, vec![(0, main_id), (1, wt_id)]);
    }

    /// A registry written before worktree nesting existed holds a linked
    /// worktree's entry without `parent_workspace_id`. A listing fills the
    /// parent in from git and keeps it in the file, so the pickers nest the
    /// worktree without it being touched again.
    #[test]
    fn a_listing_backfills_the_parent_of_an_entry_from_the_old_shape() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let base = tempfile::tempdir().unwrap();
        let main = base.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q"]);
        std::fs::write(main.join(".gitignore"), ".apb/workspace.local\n").unwrap();
        git(&main, &["add", ".gitignore"]);
        git(&main, &["commit", "-q", "-m", "init"]);
        init_project(&main).unwrap();
        let main_id = crate::workspace::ensure_id(&main).unwrap();
        let wt = base.path().join("wt");
        git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
        init_project(&wt).unwrap();
        let wt_id = crate::workspace::ensure_id(&wt).unwrap();

        // Exactly what beta.2 and earlier wrote: no `parent_workspace_id`.
        write_old_registry(
            cfg.path(),
            &[(&main_id, &main, "main"), (&wt_id, &wt, "wt")],
        );

        let listed = list_active();
        let of = |id: &str| listed.iter().find(|e| e.workspace_id == id).unwrap();
        assert_eq!(
            of(&wt_id).parent_workspace_id.as_deref(),
            Some(main_id.as_str()),
            "the listing nests the old worktree entry"
        );
        assert_eq!(of(&main_id).parent_workspace_id, None);

        let written: Value =
            serde_json::from_slice(&std::fs::read(registry_file(cfg.path())).unwrap()).unwrap();
        assert_eq!(
            written["entries"][wt_id.as_str()]["parent_workspace_id"],
            main_id.as_str(),
            "the backfilled parent is kept in the file"
        );
        assert!(
            written["entries"][main_id.as_str()]
                .get("parent_workspace_id")
                .is_none()
        );
        assert_eq!(written["entries"][wt_id.as_str()]["last_seen_ms"], 1);

        // Once nested, the worktree is no longer asked about, and what git
        // said about it is not kept: the cache holds orphans only.
        let _ = list_active();
        assert!(
            !PARENT_ROOTS.lock().unwrap().contains_key(&wt),
            "a nested entry's git answer is dropped"
        );
    }

    /// Writes the registry exactly as beta.2 and earlier did: active
    /// entries without `parent_workspace_id`.
    fn write_old_registry(cfg: &Path, entries: &[(&str, &Path, &str)]) {
        let entries: Map<String, Value> = entries
            .iter()
            .map(|(id, path, name)| {
                let entry = serde_json::json!({
                    "workspace_id": id,
                    "fingerprint": null,
                    "path": path.to_string_lossy(),
                    "name": name,
                    "last_seen_ms": 1,
                    "playbook_count": 0,
                    "state": { "kind": "active" }
                });
                (id.to_string(), entry)
            })
            .collect();
        let old = serde_json::json!({ "schema_version": SCHEMA_VERSION, "entries": entries });
        std::fs::write(registry_file(cfg), old.to_string()).unwrap();
    }

    /// A long-lived process (the dashboard) lists a directory while it is
    /// not in git, then the directory becomes a linked worktree of a
    /// registered project. The next listing nests it: what git said about
    /// the plain directory is not kept past the change of its `.git`.
    #[test]
    fn a_directory_that_becomes_a_worktree_later_is_backfilled() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let base = tempfile::tempdir().unwrap();
        let main = base.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q"]);
        std::fs::write(main.join(".gitignore"), ".apb/workspace.local\n").unwrap();
        git(&main, &["add", ".gitignore"]);
        git(&main, &["commit", "-q", "-m", "init"]);
        init_project(&main).unwrap();
        let main_id = crate::workspace::ensure_id(&main).unwrap();
        let wt = base.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        init_project(&wt).unwrap();
        let wt_id = crate::workspace::ensure_id(&wt).unwrap();
        write_old_registry(
            cfg.path(),
            &[(&main_id, &main, "main"), (&wt_id, &wt, "wt")],
        );

        let parent_of_wt = || {
            list_active()
                .into_iter()
                .find(|e| e.workspace_id == wt_id)
                .unwrap()
                .parent_workspace_id
        };
        assert_eq!(parent_of_wt(), None, "a plain directory has no parent");

        // The same directory, same workspace id, now a linked worktree.
        let id_file = wt.join(".apb").join("workspace.local");
        let id_bytes = std::fs::read(&id_file).unwrap();
        std::fs::remove_dir_all(&wt).unwrap();
        git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
        std::fs::create_dir_all(wt.join(".apb")).unwrap();
        std::fs::write(&id_file, id_bytes).unwrap();

        assert_eq!(
            parent_of_wt().as_deref(),
            Some(main_id.as_str()),
            "the listing after the change nests the worktree"
        );
    }

    fn entry(id: &str, name: &str, parent: Option<&str>) -> ProjectEntry {
        ProjectEntry {
            workspace_id: id.into(),
            fingerprint: None,
            path: format!("/p/{name}"),
            name: name.into(),
            last_seen_ms: 0,
            playbook_count: 0,
            state: State::Active,
            parent_workspace_id: parent.map(str::to_string),
        }
    }

    #[test]
    fn worktrees_nest_under_their_parent_and_orphans_stay_top_level() {
        let nested: Vec<(usize, String)> = nest_worktrees(vec![
            entry("w2", "b-feature", Some("b")),
            entry("a", "alpha", None),
            entry("o", "orphan", Some("missing")),
            entry("b", "beta", None),
            entry("w1", "a-fix", Some("b")),
        ])
        .into_iter()
        .map(|(d, e)| (d, e.workspace_id))
        .collect();
        let expect: Vec<(usize, String)> = [(0, "a"), (0, "b"), (1, "w1"), (1, "w2"), (0, "o")]
            .into_iter()
            .map(|(d, id)| (d, id.to_string()))
            .collect();
        assert_eq!(nested, expect);
    }

    /// `prune` removes exactly the entries whose workspace is gone, the
    /// dry run lists the same ones and leaves the file byte-identical.
    #[test]
    fn prune_removes_gone_workspaces_and_the_dry_run_writes_nothing() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let live = tempfile::tempdir().unwrap();
        init_project(live.path()).unwrap();
        touch(live.path());
        let live_id = crate::workspace::ensure_id(live.path()).unwrap();
        let gone = tempfile::tempdir().unwrap();
        init_project(gone.path()).unwrap();
        touch(gone.path());
        let gone_id = crate::workspace::ensure_id(gone.path()).unwrap();
        drop(gone);

        let file = registry_file(cfg.path());
        let before = std::fs::read(&file).unwrap();
        let planned: Vec<String> = prune(true)
            .unwrap()
            .into_iter()
            .map(|e| e.workspace_id)
            .collect();
        assert_eq!(planned, vec![gone_id.clone()]);
        assert_eq!(std::fs::read(&file).unwrap(), before, "a dry run wrote");

        let removed: Vec<String> = prune(false)
            .unwrap()
            .into_iter()
            .map(|e| e.workspace_id)
            .collect();
        assert_eq!(removed, vec![gone_id]);
        let left: Vec<String> = list_active().into_iter().map(|e| e.workspace_id).collect();
        assert_eq!(left, vec![live_id]);
        assert!(prune(false).unwrap().is_empty(), "nothing left to prune");
    }

    /// Any registry write drops an entry unreachable for 30 days, before
    /// its tombstone would have been purged.
    #[test]
    fn a_registry_write_drops_entries_unreachable_for_30_days() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let mut ids = Vec::new();
        for _ in 0..2 {
            let gone = tempfile::tempdir().unwrap();
            init_project(gone.path()).unwrap();
            touch(gone.path());
            ids.push(crate::workspace::ensure_id(gone.path()).unwrap());
        }
        let now = crate::clock::now_ms_u64();
        test_set_unreachable_since(&ids[0], now - 31 * MS_PER_DAY);
        test_set_unreachable_since(&ids[1], now - 2 * MS_PER_DAY);

        let live = tempfile::tempdir().unwrap();
        init_project(live.path()).unwrap();
        touch(live.path());

        let raw = std::fs::read_to_string(registry_file(cfg.path())).unwrap();
        assert!(
            !raw.contains(&ids[0]),
            "a 31-day unreachable entry survived"
        );
        assert!(
            raw.contains(&ids[1]),
            "a 2-day unreachable entry was dropped"
        );
    }

    /// The tombstoned arm of the drop: a tombstone ages by the unreachable
    /// window too, so an entry tombstoned 17 days ago (17 + 14 = 31 days of
    /// unreachable-equivalent age) goes on any registry write, while one
    /// tombstoned 15 days ago (29) stays.
    #[test]
    fn a_registry_write_drops_tombstoned_entries_past_the_limit() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let now = crate::clock::now_ms_u64();
        let mut ids = Vec::new();
        for days in [17, 15] {
            let gone = tempfile::tempdir().unwrap();
            init_project(gone.path()).unwrap();
            touch(gone.path());
            let id = crate::workspace::ensure_id(gone.path()).unwrap();
            drop(gone);
            let since = now.saturating_sub(days * MS_PER_DAY);
            let _ = with_registry(Access::Write, |file| {
                if let Some(e) = file.entries.get_mut(&id) {
                    e.state = State::Tombstoned { since_ms: since };
                }
            });
            ids.push(id);
        }

        let live = tempfile::tempdir().unwrap();
        init_project(live.path()).unwrap();
        touch(live.path());

        let raw = std::fs::read_to_string(registry_file(cfg.path())).unwrap();
        assert!(!raw.contains(&ids[0]), "a 31-day tombstone survived");
        assert!(raw.contains(&ids[1]), "a 29-day tombstone was dropped");
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

    /// A registry I/O failure must surface as an error from `prune`, never as
    /// an empty removed list the caller would report as "nothing to prune".
    /// The fault: a directory sits where the registry lock file goes, so the
    /// locked write fails no matter who runs.
    #[test]
    fn prune_surfaces_a_registry_io_error() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let proj = tempfile::tempdir().unwrap();
        init_project(proj.path()).unwrap();
        touch(proj.path());
        // A second project that disappears: removing it is what makes prune
        // write, so the induced lock failure is on the write path.
        let gone = tempfile::tempdir().unwrap();
        init_project(gone.path()).unwrap();
        touch(gone.path());
        drop(gone);
        std::fs::create_dir(cfg.path().join("projects.json.lock")).unwrap();

        let err = prune(false).expect_err("a failed prune must not report success");
        assert!(matches!(err, PruneError::Io(_)), "{err:?}");
    }

    /// Prune removes only entries whose directory is genuinely gone
    /// (NotFound). A path that still exists but fails the identity check is
    /// kept: an unmounted disk or a down sshfs must not cost the registry its
    /// entry.
    #[test]
    fn prune_keeps_an_entry_whose_path_still_exists() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let live = tempfile::tempdir().unwrap();
        init_project(live.path()).unwrap();
        touch(live.path());
        let live_id = crate::workspace::ensure_id(live.path()).unwrap();

        // Exists, but the identity check fails: the workspace file names
        // another id, like a copied checkout that took the directory over.
        let alien = tempfile::tempdir().unwrap();
        init_project(alien.path()).unwrap();
        touch(alien.path());
        let alien_id = crate::workspace::ensure_id(alien.path()).unwrap();
        std::fs::write(
            alien.path().join(".apb/workspace.local"),
            "some-other-workspace",
        )
        .unwrap();

        let removed: Vec<String> = prune(false)
            .unwrap()
            .into_iter()
            .map(|e| e.workspace_id)
            .collect();
        assert_eq!(removed, Vec::<String>::new(), "nothing is genuinely gone");
        // The alien path is unreachable, so it is not active any more, but
        // the entry itself must still exist in the registry file.
        let reachable: Vec<String> = list_reachable()
            .into_iter()
            .map(|e| e.workspace_id)
            .collect();
        let raw = std::fs::read_to_string(registry_file(cfg.path())).unwrap();
        assert!(raw.contains(&alien_id), "the unreachable entry was pruned");
        assert!(raw.contains(&live_id));
        assert!(
            !reachable.contains(&alien_id),
            "the alien path must not count as reachable"
        );
    }

    /// The long-unreachable auto-drop re-checks reachability right before it
    /// drops: an entry whose workspace came back stays, as active again.
    #[test]
    fn the_auto_drop_keeps_an_entry_that_became_reachable_again() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let proj = tempfile::tempdir().unwrap();
        init_project(proj.path()).unwrap();
        touch(proj.path());
        let id = crate::workspace::ensure_id(proj.path()).unwrap();
        let now = crate::clock::now_ms_u64();
        test_set_unreachable_since(&id, now.saturating_sub(40 * MS_PER_DAY));

        let raw = std::fs::read_to_string(registry_file(cfg.path())).unwrap();
        assert!(
            raw.contains(&id),
            "a reachable workspace was dropped at the 30-day mark"
        );
        let state = with_registry(Access::View, |file| {
            file.entries.get(&id).map(|e| e.state.clone())
        })
        .unwrap()
        .unwrap();
        assert_eq!(state, State::Active, "a reachable workspace stays active");
    }

    /// The long-unreachable auto-drop removes an entry only when its
    /// directory is verifiably gone (stat: NotFound while the parent still
    /// stats). A path that exists but fails the identity check - like an
    /// unmounted disk later re-mounted under a moved checkout, or a down
    /// sshfs - is kept even past the limit.
    #[test]
    fn the_auto_drop_keeps_an_entry_whose_path_still_exists() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let proj = tempfile::tempdir().unwrap();
        init_project(proj.path()).unwrap();
        touch(proj.path());
        let id = crate::workspace::ensure_id(proj.path()).unwrap();
        // Exists, but the identity check fails: the workspace file names
        // another id, like a copied checkout that took the directory over.
        std::fs::write(
            proj.path().join(".apb/workspace.local"),
            "some-other-workspace",
        )
        .unwrap();

        // A path whose stat errors without being NotFound (here: the parent
        // denies search), as an unmounted or unreadable mount does.
        let hidden = tempfile::tempdir_in(cfg.path()).unwrap();
        std::fs::create_dir(hidden.path().join("ws")).unwrap();
        let hidden_id = format!("ws-hidden-{id}");
        let _ = with_registry(Access::Write, |file| {
            file.entries.insert(
                hidden_id.clone(),
                StoredEntry {
                    workspace_id: hidden_id.clone(),
                    fingerprint: None,
                    path: hidden.path().join("ws").display().to_string(),
                    name: "hidden".into(),
                    last_seen_ms: 0,
                    playbook_count: 0,
                    state: State::Active,
                    parent_workspace_id: None,
                    extra: Default::default(),
                },
            );
        });
        std::fs::set_permissions(hidden.path(), std::fs::Permissions::from_mode(0o000)).unwrap();
        let now = crate::clock::now_ms_u64();
        test_set_unreachable_since(&id, now.saturating_sub(40 * MS_PER_DAY));
        test_set_unreachable_since(&hidden_id, now.saturating_sub(40 * MS_PER_DAY));
        std::fs::set_permissions(hidden.path(), std::fs::Permissions::from_mode(0o700)).unwrap();

        let raw = std::fs::read_to_string(registry_file(cfg.path())).unwrap();
        assert!(
            raw.contains(&id),
            "an existing path must never be dropped by the auto-drop"
        );
        assert!(
            raw.contains(&hidden_id),
            "a stat error other than NotFound must never drop an entry"
        );
    }

    /// A config.yaml that does not parse must not silently lower the
    /// auto-drop limit back to 30 days: the drop is skipped entirely, so a
    /// user who configured a 365-day purge never loses an entry at 30.
    #[test]
    fn a_broken_config_skips_the_auto_drop() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;
        std::fs::write(
            cfg.path().join("config.yaml"),
            "registry_purge_days: [not, a, number\n",
        )
        .unwrap();

        let proj = tempfile::tempdir().unwrap();
        init_project(proj.path()).unwrap();
        touch(proj.path());
        let id = crate::workspace::ensure_id(proj.path()).unwrap();
        // Genuinely gone: only the limit could keep the entry, and the
        // configured limit is 30 + 365, so without the broken config the
        // entry survives; with a silently defaulted config it would not be
        // distinguishable. Force the plain-30 case instead by removing the
        // configured keys from the picture: assert with a default config the
        // entry goes, then assert the broken config keeps it.
        std::fs::remove_dir_all(proj.path()).unwrap();
        let now = crate::clock::now_ms_u64();
        test_set_unreachable_since(&id, now.saturating_sub(40 * MS_PER_DAY));

        let raw = std::fs::read_to_string(registry_file(cfg.path())).unwrap();
        assert!(
            raw.contains(&id),
            "a broken config must not fall back to the 30-day default drop"
        );
    }

    /// With the registry day settings configured, the auto-drop moves to
    /// max(30, unreachable + purge) days instead of the plain 30.
    #[test]
    fn configured_registry_days_raise_the_auto_drop_limit() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;
        std::fs::write(
            cfg.path().join("config.yaml"),
            "registry_unreachable_days: 14\nregistry_purge_days: 365\n",
        )
        .unwrap();

        let proj = tempfile::tempdir().unwrap();
        init_project(proj.path()).unwrap();
        touch(proj.path());
        let id = crate::workspace::ensure_id(proj.path()).unwrap();
        // The workspace directory itself is genuinely gone (the auto-drop
        // removes an entry only on a stat NotFound), so only the day limit
        // can keep it.
        std::fs::remove_dir_all(proj.path()).unwrap();
        let now = crate::clock::now_ms_u64();
        test_set_unreachable_since(&id, now.saturating_sub(40 * MS_PER_DAY));

        let raw = std::fs::read_to_string(registry_file(cfg.path())).unwrap();
        assert!(
            raw.contains(&id),
            "40 days is within 14 + 365, so the entry must be kept"
        );

        test_set_unreachable_since(
            &id,
            crate::clock::now_ms_u64().saturating_sub(380 * MS_PER_DAY),
        );
        let raw = std::fs::read_to_string(registry_file(cfg.path())).unwrap();
        assert!(
            !raw.contains(&id),
            "380 days is past 14 + 365, so the entry must be dropped"
        );
    }

    /// A git that cannot start at a touch must not erase the recorded
    /// worktree parent: the parent is kept unless git works and reports none.
    #[test]
    fn touch_keeps_the_recorded_parent_when_git_fails() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        setup(cfg.path());
        let _g = EnvGuard;

        let parent = tempfile::tempdir().unwrap();
        init_project(parent.path()).unwrap();
        touch(parent.path());
        let parent_id = crate::workspace::ensure_id(parent.path()).unwrap();

        let child = tempfile::tempdir().unwrap();
        init_project(child.path()).unwrap();
        touch(child.path());
        let child_id = crate::workspace::ensure_id(child.path()).unwrap();
        let _ = with_registry(Access::Write, |file| {
            if let Some(e) = file.entries.get_mut(&child_id) {
                e.parent_workspace_id = Some(parent_id.clone());
            }
        });

        // The child moves to a directory whose `.git` file is garbage, so
        // git cannot report anything about it.
        let moved = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(moved.path().join(".apb/playbooks")).unwrap();
        std::fs::write(
            moved.path().join(".git"),
            "gitdir: /definitely/not/a/git/dir\n",
        )
        .unwrap();
        std::fs::write(moved.path().join(".apb/workspace.local"), &child_id).unwrap();
        std::fs::remove_dir_all(child.path().join(".apb")).unwrap();
        touch(moved.path());

        let entry = with_registry(Access::View, |file| {
            file.entries
                .get(&child_id)
                .map(|e| (e.parent_workspace_id.clone(), e.path.clone()))
        })
        .unwrap()
        .unwrap();
        assert_eq!(
            entry.0,
            Some(parent_id),
            "the recorded parent was erased on a git failure"
        );
        assert_eq!(Path::new(&entry.1), moved.path());
    }
}
