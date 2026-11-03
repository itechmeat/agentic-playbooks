//! Content apb ships inside its binary: default playbooks and process
//! skills, from the repository folder `bundled/` (`playbooks/<id>/` with a
//! `playbook.yaml`, an optional `scripts/` and `evals/`; `skills/<name>/`
//! with a `SKILL.md` and its files), baked in with rust-embed exactly like
//! the official connectors ([`crate::connector::official`]).
//!
//! The delivery follows the connector path: a playbook is written into the
//! global store as an ordinary version and only that exact digest is trusted
//! with origin `Bundled` ([`playbooks`]); a skill is copied into the global
//! skills directory with a sidecar of its shipped digest ([`skills`]). An
//! upgrade moves an unmodified copy to the new shipped content and leaves a
//! modified one alone, reporting an update instead. [`roles`] plans the role
//! profiles the shipped playbooks name.
//!
//! The id prefix `apb-` is reserved for this content: no other playbook may
//! be created with it ([`refuse_reserved_id`]).
//!
//! Every write of shipped content (install, replace, remove, reconcile)
//! holds one lock under the config dir ([`hold`]); the reconcile a server
//! start runs only tries it and skips when another process holds it. A
//! copy is never moved to an older shipped version than the newest one
//! installed: an older binary reports newer defaults and leaves them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::content::TreeFile;
use crate::tree_install::FileMap;
use crate::validate::{Issue, Severity};
use crate::versioning::VersioningError;

mod playbooks;
mod report;
mod role_trust;
mod roles;
mod skills;
#[cfg(test)]
mod tests;

pub use playbooks::{
    BundledError, CREATED_BY, PlaybookInstalled, install_playbook, playbook_state, playbook_store,
    remove_playbook, replace_modified_playbook, restore_replaced_playbook,
};
pub use report::{DEFAULTS_HINT, ReconcileNote, describe, print_notes, reconcile_defaults};
pub use role_trust::note_role_profile;
pub use roles::{
    Independence, PlanError, ROLE_MAIN, ROLE_RESEARCHER, ROLE_REVIEWER, RoleFallback, RolePlan,
    RoleProfile, SkippedAgent, default_model, plan_role_profiles, role_skills,
};
pub use skills::{
    SkillDirs, SkillError, SkillInstalled, SkillUpdates, install_skill, remove_skill,
    replace_modified_skill, restore_replaced_skill, skill_state,
};

/// Seams for the tests of other crates: shipped content of a test's own,
/// installed through the real path. Never compiled into a release build,
/// so nothing outside apb can mint `Bundled` trust for arbitrary content.
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use super::*;

    /// [`BundledPlaybook::from_files`].
    pub fn playbook_from_files(id: &str, files: &FileMap) -> Option<BundledPlaybook> {
        BundledPlaybook::from_files(id, files)
    }

    /// [`BundledSkill::from_files`].
    pub fn skill_from_files(name: &str, files: FileMap) -> Option<BundledSkill> {
        BundledSkill::from_files(name, files)
    }

    /// [`install_playbook`] of a test's own shipped playbook.
    pub fn install_playbook(
        shipped: &BundledPlaybook,
        force: bool,
    ) -> Result<PlaybookInstalled, BundledError> {
        let _lock = hold()?;
        playbooks::install_shipped(shipped, force)
    }

    /// [`install_skill`] of a test's own shipped skill into the global
    /// skills directory.
    pub fn install_skill(skill: &BundledSkill, force: bool) -> Result<SkillInstalled, SkillError> {
        let dirs = SkillDirs::global().ok_or(SkillError::NoSkillsDir)?;
        let _lock = hold().map_err(|e| SkillError::Io(e.to_string()))?;
        skills::install_skill_in(&dirs, skill, force)
    }
}

/// The folder under the config dir that holds the lock of shipped content
/// and the record of the role profiles setup wrote.
const STATE_DIR: &str = "bundled";
/// The lock every write of shipped content holds.
const LOCK_FILE: &str = ".bundled.lock";

/// `<config_dir>/bundled`.
fn state_dir() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join(STATE_DIR))
}

fn no_state_dir() -> std::io::Error {
    std::io::Error::other("no config directory available")
}

/// Takes the lock of shipped content, waiting a short while for a holder.
pub(crate) fn hold() -> std::io::Result<crate::fsutil::DirLock> {
    crate::fsutil::lock_dir(&state_dir().ok_or_else(no_state_dir)?, LOCK_FILE)
}

/// Takes the lock of shipped content without waiting: `None` when another
/// process holds it.
pub(crate) fn try_hold() -> std::io::Result<Option<crate::fsutil::DirLock>> {
    match crate::fsutil::try_lock_dir(&state_dir().ok_or_else(no_state_dir)?, LOCK_FILE) {
        Ok(lock) => Ok(Some(lock)),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(e) => Err(e),
    }
}

/// Whether the shipped `embedded` version is older than the `installed`
/// one, both semver; a version that does not parse is never older.
pub(crate) fn is_older(embedded: &str, installed: &str) -> bool {
    match (
        crate::registry::parse_version(embedded),
        crate::registry::parse_version(installed),
    ) {
        (Some(e), Some(i)) => e < i,
        _ => false,
    }
}

/// The reconcile outcome shared with the official connectors.
pub use crate::connector::install::Reconciled;

/// Where the installed copy of a shipped playbook or skill stands, as every
/// read-only surface reports it (`apb setup plan`, `apb doctor`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum InstallState {
    /// Not installed.
    Absent,
    /// Something of that name apb did not install: never touched.
    Foreign,
    /// Holds the shipped content.
    UpToDate { installed: String },
    /// Untouched since apb installed it, but older content: an upgrade
    /// moves it on its own.
    Pristine { installed: String },
    /// Changed by someone since apb installed it: an update is available
    /// and never applied on its own.
    Modified { installed: String },
}

impl InstallState {
    /// Whether a copy apb installed is there (up to date, pristine or
    /// modified).
    pub fn is_installed(&self) -> bool {
        matches!(
            self,
            InstallState::UpToDate { .. }
                | InstallState::Pristine { .. }
                | InstallState::Modified { .. }
        )
    }
}

/// The embedded `bundled/` tree, baked in at build time (release) or read
/// from the repository folder at run time (debug).
#[derive(rust_embed::Embed)]
#[folder = "../../bundled"]
struct BundledAssets;

/// The id prefix reserved for the playbooks, skills and role profiles apb
/// ships.
pub const RESERVED_PREFIX: &str = "apb-";

/// The stable code of the refusal of a reserved id.
pub const RESERVED_ID_CODE: &str = "reserved_id";

/// Whether `id` carries [`RESERVED_PREFIX`].
pub fn is_reserved_id(id: &str) -> bool {
    id.starts_with(RESERVED_PREFIX)
}

/// The sentence a refusal of the reserved `id` carries.
pub fn reserved_id_message(id: &str) -> String {
    format!(
        "{RESERVED_ID_CODE}: the id `{id}` starts with `{RESERVED_PREFIX}`, which is reserved for the playbooks apb ships; pick another id (to adapt a shipped playbook, save a copy under your own id)"
    )
}

/// Refuses creating a playbook with a reserved id: every create, capture and
/// import path calls it for a new playbook. The one writer of a reserved id
/// is [`install_playbook`].
pub(crate) fn refuse_reserved_id(id: &str) -> Result<(), VersioningError> {
    if !is_reserved_id(id) {
        return Ok(());
    }
    Err(VersioningError::Validation(vec![Issue::new::<String>(
        RESERVED_ID_CODE,
        Severity::Error,
        None,
        reserved_id_message(id),
    )]))
}

/// One shipped playbook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundledPlaybook {
    pub id: String,
    /// The version its `playbook.yaml` names.
    pub version: String,
    pub yaml: String,
    /// Its `scripts/`, paths relative to that folder.
    pub scripts: Vec<TreeFile>,
    /// Its `evals/`, paths relative to that folder.
    pub evals: FileMap,
}

/// One shipped skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundledSkill {
    pub name: String,
    /// `metadata.version` (or `version`) of its `SKILL.md` frontmatter,
    /// `0.0.0` when it names none.
    pub version: String,
    pub files: FileMap,
}

/// The embedded files under `<section>/`, grouped by their folder.
fn embedded(section: &str) -> BTreeMap<String, FileMap> {
    let prefix = format!("{section}/");
    crate::tree_install::group_by_folder(BundledAssets::iter().filter_map(|path| {
        let rel = path.strip_prefix(&prefix)?.to_string();
        let file = BundledAssets::get(path.as_ref())?;
        Some((rel, file.data.into_owned()))
    }))
}

/// The files of `files` under `<dir>/`, with that prefix stripped.
fn sub_tree(files: &FileMap, dir: &str) -> FileMap {
    let prefix = format!("{dir}/");
    files
        .iter()
        .filter_map(|(rel, bytes)| Some((rel.strip_prefix(&prefix)?.to_string(), bytes.clone())))
        .collect()
}

impl BundledPlaybook {
    /// The playbook in the folder `id` with `files`, or `None` when the id
    /// is not reserved, `playbook.yaml` is missing or does not parse, or it
    /// names another id.
    pub(crate) fn from_files(id: &str, files: &FileMap) -> Option<Self> {
        if !is_reserved_id(id) {
            return None;
        }
        let yaml = std::str::from_utf8(files.get("playbook.yaml")?).ok()?;
        let playbook = crate::schema::Playbook::from_yaml(yaml).ok()?;
        if playbook.id != id {
            return None;
        }
        let scripts = sub_tree(files, "scripts")
            .into_iter()
            .map(|(rel, bytes)| TreeFile {
                executable: bytes.starts_with(b"#!"),
                rel,
                bytes,
            })
            .collect();
        Some(Self {
            id: id.to_string(),
            version: playbook.version,
            yaml: yaml.to_string(),
            scripts,
            evals: sub_tree(files, "evals"),
        })
    }
}

impl BundledSkill {
    /// The skill in the folder `name` with `files`, or `None` when the name
    /// is not reserved or it has no `SKILL.md`.
    pub(crate) fn from_files(name: &str, files: FileMap) -> Option<Self> {
        if !is_reserved_id(name) {
            return None;
        }
        let skill_md = std::str::from_utf8(files.get("SKILL.md")?).ok()?;
        Some(Self {
            name: name.to_string(),
            version: frontmatter_version(skill_md).unwrap_or_else(|| "0.0.0".to_string()),
            files,
        })
    }
}

/// `metadata.version`, else `version`, of the YAML frontmatter of `md`.
fn frontmatter_version(md: &str) -> Option<String> {
    let body = md.strip_prefix("---")?;
    let end = body.find("\n---")?;
    let doc: yaml_serde::Value = yaml_serde::from_str(&body[..end]).ok()?;
    let pick = |v: &yaml_serde::Value| match v {
        yaml_serde::Value::String(s) => Some(s.clone()),
        yaml_serde::Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    doc.get("metadata")
        .and_then(|m| m.get("version"))
        .and_then(pick)
        .or_else(|| doc.get("version").and_then(pick))
}

/// Every shipped playbook, sorted by id. A folder that does not make a
/// [`BundledPlaybook`] is skipped.
pub fn playbooks() -> Vec<BundledPlaybook> {
    embedded("playbooks")
        .iter()
        .filter_map(|(id, files)| BundledPlaybook::from_files(id, files))
        .collect()
}

/// The shipped playbook `id`.
pub fn playbook(id: &str) -> Option<BundledPlaybook> {
    playbooks().into_iter().find(|p| p.id == id)
}

/// Every shipped skill, sorted by name. A folder that does not make a
/// [`BundledSkill`] is skipped.
pub fn skills() -> Vec<BundledSkill> {
    embedded("skills")
        .into_iter()
        .filter_map(|(name, files)| BundledSkill::from_files(&name, files))
        .collect()
}

/// The shipped skill `name`.
pub fn skill(name: &str) -> Option<BundledSkill> {
    skills().into_iter().find(|s| s.name == name)
}
