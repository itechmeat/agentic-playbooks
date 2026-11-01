//! The launch floor: per playbook, the strictest `agent_launch` any saved
//! version declared, kept outside the definition in the machine-local
//! `<config dir>/launch-floors.json`. The effective declaration of a
//! version is the stricter of its own and its floor, so saving a looser
//! version, deleting the key, deleting and recreating the id or restoring an
//! older copy never loosens what was approved. Only a person lowers it.
//!
//! **Identity.** A floor belongs to a [`FloorKey`]: a project playbook by
//! the workspace that OWNS its store, a global one by its id alone (the
//! machine has one global store, whatever project runs it). The key is
//! always passed explicitly by the writer and built from the definition's
//! origin by the reader; it is never inferred from an execution root or a
//! staging path.
//!
//! **A key without a record** has the floor of the strictest declaration
//! among the versions its store holds now ([`scan`]); a read never writes
//! it, so read-only surfaces write nothing. The first write of a record
//! starts from that scan.
//!
//! **Publication.** [`publish`] is the one way a version becomes visible in
//! a store: under the floor lock it checks the channel (an agent channel
//! may not declare looser than the floor), raises the floor and only then
//! runs the rename it is given, so the floor is durable before stricter
//! content is runnable and a person's lowering never interleaves between
//! the check and the publication. Lock order: a playbook's pointer lock
//! before the floor lock; the floor lock never waits on anything else. A
//! crash after the floor write and before the rename leaves the floor
//! stricter than the store (fail closed); only a person lowers it.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::policy::LaunchPolicy;
use crate::fsutil::DirLock;

/// The file, in the machine config dir.
pub const FLOORS_FILE: &str = "launch-floors.json";
/// The lock every floor write holds.
const LOCK: &str = "launch-floors.lock";
/// The scope word of a global key.
pub const GLOBAL_SCOPE: &str = "global";
/// The scope word of a project key.
pub const PROJECT_SCOPE: &str = "project";

/// Whose floor: a project playbook by the workspace that owns its store, a
/// global one machine-wide.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FloorKey {
    Project { workspace_id: String, id: String },
    Global { id: String },
}

impl FloorKey {
    /// The machine-wide key of global playbook `id`.
    pub fn global(id: &str) -> Self {
        FloorKey::Global { id: id.to_string() }
    }

    /// The key of playbook `id` in the store of the workspace at
    /// `owning_root` (the project whose `.apb/playbooks` holds it): a
    /// writer's key. The workspace id is one apb minted and registered at
    /// this root in the projects registry under the config dir
    /// ([`crate::projects::bind_id`]), created and registered when missing,
    /// never only what `.apb/workspace.local` says.
    pub fn project_of(owning_root: &Path, id: &str) -> io::Result<Self> {
        Ok(FloorKey::Project {
            workspace_id: crate::projects::bind_id(owning_root)?,
            id: id.to_string(),
        })
    }

    /// [`FloorKey::project_of`] without creating anything: a reader's key,
    /// `None` when the workspace has no id registered at this root (its
    /// store's scan holds the floor then).
    pub fn read_project(owning_root: &Path, id: &str) -> Option<Self> {
        Self::read_project_in(crate::config::config_dir().as_deref(), owning_root, id)
    }

    /// [`FloorKey::read_project`] against the projects registry of the
    /// config dir `config_dir`.
    pub fn read_project_in(
        config_dir: Option<&Path>,
        owning_root: &Path,
        id: &str,
    ) -> Option<Self> {
        crate::projects::registered_id_in(config_dir, owning_root).map(|workspace_id| {
            FloorKey::Project {
                workspace_id,
                id: id.to_string(),
            }
        })
    }

    /// The playbook's id.
    pub fn id(&self) -> &str {
        match self {
            FloorKey::Project { id, .. } | FloorKey::Global { id } => id,
        }
    }

    /// Whether the key is a global playbook's.
    pub fn is_global(&self) -> bool {
        matches!(self, FloorKey::Global { .. })
    }

    /// `project/<workspace id>/<id>` or `global/<id>`, the record's name.
    pub fn as_string(&self) -> String {
        match self {
            FloorKey::Project { workspace_id, id } => {
                format!("{PROJECT_SCOPE}/{workspace_id}/{id}")
            }
            FloorKey::Global { id } => format!("{GLOBAL_SCOPE}/{id}"),
        }
    }
}

impl std::fmt::Display for FloorKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_string())
    }
}

/// Why a floor could not be read or a save was refused.
#[derive(Debug, thiserror::Error)]
pub enum FloorError {
    /// An agent channel saved a declaration looser than the floor.
    #[error(
        "launch_policy_loosened: playbook `{id}` declares agent_launch `{declared}`, looser than its launch floor `{floor}`; an agent cannot loosen a launch policy. A person can save it in the dashboard editor, or lower the floor with `apb launch floor {id}{global_flag} --lower-to {declared}` at a terminal",
        global_flag = if *global { " --global" } else { "" }
    )]
    Loosened {
        id: String,
        global: bool,
        declared: LaunchPolicy,
        floor: LaunchPolicy,
    },
    /// The dashboard editor with keys lowers a floor the person did not
    /// confirm in the editor.
    #[error(
        "launch_floor_lowering_unconfirmed: saving playbook `{id}` with agent_launch `{declared}` lowers its launch floor from `{floor}`; confirm the lowering in the editor, or lower the floor with `apb launch floor {id}{global_flag} --lower-to {declared}` at a terminal",
        global_flag = if *global { " --global" } else { "" }
    )]
    LoweringUnconfirmed {
        id: String,
        global: bool,
        declared: LaunchPolicy,
        floor: LaunchPolicy,
    },
    /// The dashboard without keys tried to lower a floor: anything on this
    /// machine reaches it, so it never does.
    #[error(
        "launch_floor_lowering_needs_terminal: saving playbook `{id}` with agent_launch `{declared}` lowers its launch floor from `{floor}`, and a dashboard without keys does not lower a floor, since anything on this machine can reach it; lower it with `apb launch floor {id}{global_flag} --lower-to {declared}` at a terminal, or issue a key with `apb server key issue` and sign in",
        global_flag = if *global { " --global" } else { "" }
    )]
    LoweringNeedsTerminal {
        id: String,
        global: bool,
        declared: LaunchPolicy,
        floor: LaunchPolicy,
    },
    /// The file is present and does not read.
    #[error("launch_floor_invalid: {0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl FloorError {
    /// The refusal code.
    pub fn code(&self) -> &'static str {
        match self {
            FloorError::Loosened { .. } => LOOSENED,
            FloorError::LoweringUnconfirmed { .. } => LOWERING_UNCONFIRMED,
            FloorError::LoweringNeedsTerminal { .. } => LOWERING_NEEDS_TERMINAL,
            FloorError::Invalid(_) => INVALID,
            FloorError::Io(_) => "launch_floor_unwritable",
        }
    }
}

/// The code of a loosening an agent channel attempted.
pub const LOOSENED: &str = "launch_policy_loosened";
/// The code of an editor lowering nobody confirmed.
pub const LOWERING_UNCONFIRMED: &str = "launch_floor_lowering_unconfirmed";
/// The code of a lowering a dashboard without keys attempted.
pub const LOWERING_NEEDS_TERMINAL: &str = "launch_floor_lowering_needs_terminal";
/// The code of a floor file that does not read.
pub const INVALID: &str = "launch_floor_invalid";

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FloorsFile {
    #[serde(default)]
    floors: BTreeMap<String, LaunchPolicy>,
}

/// The file's path; `None` without a config dir.
pub fn path() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join(FLOORS_FILE))
}

/// Every record. An absent file has none; a present one must read whole.
pub fn read() -> Result<BTreeMap<String, LaunchPolicy>, FloorError> {
    read_from(path().as_deref())
}

/// [`read`] of the floor file at `path` (a config dir's
/// [`FLOORS_FILE`]); none without one.
pub fn read_from(path: Option<&Path>) -> Result<BTreeMap<String, LaunchPolicy>, FloorError> {
    match path {
        Some(p) => read_at(p),
        None => Ok(BTreeMap::new()),
    }
}

fn read_at(path: &Path) -> Result<BTreeMap<String, LaunchPolicy>, FloorError> {
    let raw = match crate::fsutil::read_settings_file(path, crate::fsutil::SETTINGS_FILE_MAX_BYTES)
    {
        Ok(Some(raw)) => raw,
        Ok(None) => return Ok(BTreeMap::new()),
        Err(e) => {
            return Err(FloorError::Invalid(format!(
                "cannot read `{}`: {e}",
                path.display()
            )));
        }
    };
    if raw.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    serde_json::from_str::<FloorsFile>(&raw)
        .map(|f| f.floors)
        .map_err(|e| FloorError::Invalid(format!("`{}`: {e}", path.display())))
}

/// The strictest declaration among the versions in `playbook_dir` (a
/// playbook's directory: one subdirectory per version, each with its
/// `playbook.yaml`); `None` when none declares one or the directory is
/// absent. A version whose file does not read adds nothing.
pub fn scan(playbook_dir: &Path) -> Option<LaunchPolicy> {
    let entries = std::fs::read_dir(playbook_dir).ok()?;
    entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| declared_in(&e.path().join("playbook.yaml")))
        .max()
}

/// The scan of a key without a record: [`scan`] of its store directory and
/// of every trash entry of the same id beside it (`<store>/../trash/<id>-<ms>`,
/// how a delete names it), so a playbook deleted before its floor was ever
/// recorded is not recreated looser.
pub fn store_scan(playbook_dir: &Path) -> Option<LaunchPolicy> {
    let own = scan(playbook_dir);
    let (Some(id), Some(trash)) = (
        playbook_dir.file_name().and_then(|n| n.to_str()),
        playbook_dir
            .parent()
            .and_then(Path::parent)
            .map(|p| p.join("trash")),
    ) else {
        return own;
    };
    let prefix = format!("{id}-");
    let trashed = std::fs::read_dir(&trash).ok().and_then(|entries| {
        entries
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name().to_str().is_some_and(|n| {
                    n.strip_prefix(&prefix)
                        .is_some_and(|ms| !ms.is_empty() && ms.bytes().all(|b| b.is_ascii_digit()))
                })
            })
            .filter_map(|e| scan(&e.path()))
            .max()
    });
    own.max(trashed)
}

/// What the definition at `path` declares, read without parsing the rest.
fn declared_in(path: &Path) -> Option<LaunchPolicy> {
    let raw = std::fs::read_to_string(path).ok()?;
    declared_of(&raw)
}

/// What the definition text `yaml` declares.
pub fn declared_of(yaml: &str) -> Option<LaunchPolicy> {
    #[derive(Deserialize)]
    struct Decl {
        #[serde(default)]
        agent_launch: Option<LaunchPolicy>,
    }
    yaml_serde::from_str::<Decl>(yaml).ok()?.agent_launch
}

/// The floor of `key` whose store directory is `playbook_dir`: its record,
/// else the scan of the store. Reads only.
pub fn floor_of(
    key: Option<&FloorKey>,
    playbook_dir: Option<&Path>,
) -> Result<Option<LaunchPolicy>, FloorError> {
    let records = read()?;
    Ok(floor_in(&records, key, playbook_dir))
}

/// [`floor_of`] over records already read: the record of `key`, else the
/// [`store_scan`] of `playbook_dir`.
pub fn floor_in(
    records: &BTreeMap<String, LaunchPolicy>,
    key: Option<&FloorKey>,
    playbook_dir: Option<&Path>,
) -> Option<LaunchPolicy> {
    match key.and_then(|k| records.get(&k.as_string())) {
        Some(p) => Some(*p),
        None => playbook_dir.and_then(store_scan),
    }
}

/// The floor file held for a read-modify-write.
pub(crate) struct Guard {
    _lock: DirLock,
    path: PathBuf,
    records: BTreeMap<String, LaunchPolicy>,
}

/// Takes the floor lock and reads the records. `Ok(None)` without a config
/// dir (no floor can be kept). Refused in a read-only process.
pub(crate) fn lock() -> Result<Option<Guard>, FloorError> {
    let Some(path) = path() else {
        return Ok(None);
    };
    let dir = path
        .parent()
        .ok_or_else(|| FloorError::Invalid("the floor file has no directory".into()))?
        .to_path_buf();
    let lock = crate::fsutil::lock_dir(&dir, LOCK)?;
    let records = read_at(&path)?;
    Ok(Some(Guard {
        _lock: lock,
        path,
        records,
    }))
}

impl Guard {
    /// The floor of `key` under the lock (its record, else the scan).
    pub(crate) fn current(
        &self,
        key: &FloorKey,
        playbook_dir: Option<&Path>,
    ) -> Option<LaunchPolicy> {
        floor_in(&self.records, Some(key), playbook_dir)
    }

    /// Whether `key` has a record.
    pub(crate) fn recorded(&self, key: &FloorKey) -> Option<LaunchPolicy> {
        self.records.get(&key.as_string()).copied()
    }

    /// Raises `key` to `to` (or to its scan when that is stricter); writes
    /// only when the record changes.
    pub(crate) fn raise(
        &mut self,
        key: &FloorKey,
        to: Option<LaunchPolicy>,
        playbook_dir: Option<&Path>,
    ) -> Result<(), FloorError> {
        let now = self.current(key, playbook_dir);
        let target = now.max(to);
        match target {
            Some(p) if self.recorded(key) != Some(p) && p > LaunchPolicy::Allow => {
                self.write(key, p)
            }
            _ => Ok(()),
        }
    }

    /// Sets `key` to `to` whatever it was: a person's lowering.
    pub(crate) fn set(&mut self, key: &FloorKey, to: LaunchPolicy) -> Result<(), FloorError> {
        if self.recorded(key) == Some(to) {
            return Ok(());
        }
        self.write(key, to)
    }

    fn write(&mut self, key: &FloorKey, to: LaunchPolicy) -> Result<(), FloorError> {
        let mut next = self.records.clone();
        next.insert(key.as_string(), to);
        let body = serde_json::to_vec_pretty(&FloorsFile {
            floors: next.clone(),
        })
        .map_err(|e| FloorError::Invalid(e.to_string()))?;
        crate::fsutil::atomic_write_private(&self.path, &body)?;
        // The rename is durable only once its directory is: a record lost
        // to a power cut while a stricter version (on another filesystem)
        // survived would read as the older, looser floor.
        if let Some(dir) = self.path.parent() {
            crate::fsutil::sync_dir(dir)?;
        }
        self.records = next;
        Ok(())
    }
}

/// The channel a version reaches a store through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveChannel {
    /// An agent (MCP authoring and capture tools, supervisor and forward
    /// patches): may not declare looser than the floor.
    Agent,
    /// A person at a surface that is not the editor (CLI import, migrate,
    /// restore, promotion): raises the floor, never lowers it.
    Person,
    /// The dashboard editor with keys: a person saving the version they
    /// read. It raises the floor; it lowers it only to `lower_to`, the
    /// declaration the person confirmed in the editor, and refuses any
    /// other lowering (`launch_floor_lowering_unconfirmed`).
    Editor { lower_to: Option<LaunchPolicy> },
    /// The dashboard editor without keys: anything on the machine reaches
    /// it, so it raises the floor and refuses every lowering
    /// (`launch_floor_lowering_needs_terminal`).
    KeylessEditor,
}

/// Where a version is published and through which channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveAuthority {
    pub key: FloorKey,
    pub channel: SaveChannel,
}

/// Publishes a version declaring `declared` into the store `playbook_dir`
/// of `authority.key`: under the floor lock, refuses an agent channel's
/// loosening, raises (or, from the editor with a confirmed lowering, sets)
/// the floor, then runs
/// `rename` while the lock is still held. `rename`'s own error comes back
/// as the inner `Err`.
pub fn publish<T, E>(
    authority: &SaveAuthority,
    playbook_dir: &Path,
    declared: Option<LaunchPolicy>,
    rename: impl FnOnce() -> Result<T, E>,
) -> Result<Result<T, E>, FloorError> {
    let Some(mut guard) = lock()? else {
        return Ok(rename());
    };
    let key = &authority.key;
    let floor = guard.current(key, Some(playbook_dir));
    let declared_or_allow = declared.unwrap_or_default();
    match authority.channel {
        SaveChannel::Agent => {
            if let Some(f) = floor
                && declared_or_allow < f
            {
                return Err(FloorError::Loosened {
                    id: key.id().to_string(),
                    global: key.is_global(),
                    declared: declared_or_allow,
                    floor: f,
                });
            }
            guard.raise(key, declared, Some(playbook_dir))?;
        }
        SaveChannel::Person => guard.raise(key, declared, Some(playbook_dir))?,
        SaveChannel::Editor { lower_to } => match floor {
            Some(f) if declared_or_allow < f => {
                if lower_to != Some(declared_or_allow) {
                    return Err(FloorError::LoweringUnconfirmed {
                        id: key.id().to_string(),
                        global: key.is_global(),
                        declared: declared_or_allow,
                        floor: f,
                    });
                }
                guard.set(key, declared_or_allow)?;
            }
            _ => guard.raise(key, declared, Some(playbook_dir))?,
        },
        SaveChannel::KeylessEditor => {
            if let Some(f) = floor
                && declared_or_allow < f
            {
                return Err(FloorError::LoweringNeedsTerminal {
                    id: key.id().to_string(),
                    global: key.is_global(),
                    declared: declared_or_allow,
                    floor: f,
                });
            }
            guard.raise(key, declared, Some(playbook_dir))?;
        }
    }
    let out = rename();
    drop(guard);
    Ok(out)
}

/// The floor lock held without writing: a start holds it from its last
/// policy check until its authorization is written, so a publication that
/// raises a floor lands either before the check or after the record.
pub struct Hold {
    _lock: DirLock,
}

/// Takes the floor lock for a [`Hold`]; `Ok(None)` without a config dir.
/// The records are not read: a person launching past a damaged floor file
/// (the tolerant read) holds the lock all the same.
pub fn hold() -> Result<Option<Hold>, FloorError> {
    let Some(path) = path() else {
        return Ok(None);
    };
    let dir = path
        .parent()
        .ok_or_else(|| FloorError::Invalid("the floor file has no directory".into()))?;
    Ok(Some(Hold {
        _lock: crate::fsutil::lock_dir(dir, LOCK)?,
    }))
}

/// Records `key`'s scanned floor before its store directory leaves (a
/// delete to the trash): a later recreate of the id meets it.
pub fn keep_before_removal(key: &FloorKey, playbook_dir: &Path) -> Result<(), FloorError> {
    let Some(mut guard) = lock()? else {
        return Ok(());
    };
    guard.raise(key, None, Some(playbook_dir))
}

/// A person lowers (or sets) `key`'s floor to `to`.
pub fn set_by_person(key: &FloorKey, to: LaunchPolicy) -> Result<(), FloorError> {
    let Some(mut guard) = lock()? else {
        return Err(FloorError::Invalid(
            "no config dir to keep a floor in".into(),
        ));
    };
    guard.set(key, to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_name_the_owner_not_the_runner() {
        let p = FloorKey::Project {
            workspace_id: "ws1".into(),
            id: "deploy".into(),
        };
        assert_eq!(p.as_string(), "project/ws1/deploy");
        assert_eq!(FloorKey::global("deploy").as_string(), "global/deploy");
    }

    #[test]
    fn declared_of_reads_only_the_key() {
        assert_eq!(
            declared_of("id: p\nagent_launch: deny\nnodes: []\n"),
            Some(LaunchPolicy::Deny)
        );
        assert_eq!(declared_of("id: p\n"), None);
    }

    #[test]
    fn scan_takes_the_strictest_version() {
        let dir = tempfile::tempdir().unwrap();
        for (v, decl) in [("1.0.0", "allow"), ("1.1.0", "deny"), ("1.2.0", "ask")] {
            let vd = dir.path().join(v);
            std::fs::create_dir_all(&vd).unwrap();
            std::fs::write(
                vd.join("playbook.yaml"),
                format!("id: p\nagent_launch: {decl}\n"),
            )
            .unwrap();
        }
        assert_eq!(scan(dir.path()), Some(LaunchPolicy::Deny));
        assert_eq!(scan(&dir.path().join("missing")), None);
    }

    #[test]
    fn a_damaged_file_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(FLOORS_FILE);
        std::fs::write(&p, "{ not json").unwrap();
        assert!(matches!(read_at(&p), Err(FloorError::Invalid(_))));
        std::fs::write(&p, r#"{"floors":{"global/x":"never"}}"#).unwrap();
        assert!(matches!(read_at(&p), Err(FloorError::Invalid(_))));
        std::fs::write(&p, r#"{"floors":{},"extra":1}"#).unwrap();
        assert!(matches!(read_at(&p), Err(FloorError::Invalid(_))));
        std::fs::write(&p, r#"{"floors":{"global/x":"deny"}}"#).unwrap();
        assert_eq!(
            read_at(&p).unwrap().get("global/x"),
            Some(&LaunchPolicy::Deny)
        );
    }
}
