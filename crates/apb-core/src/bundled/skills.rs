//! Installing the shipped skills into the global skills directory
//! (`~/.agents/skills/<name>`, bridged into `~/.claude/skills`) and keeping
//! them current across upgrades.
//!
//! Skills resolve only from a project or the global skills directory, so a
//! shipped skill is copied there, serving both the role profiles and a host
//! that reads skills directly. Next to each copy a sidecar
//! `.<name>.apb-bundled.json` keeps the name, version and tree digest of
//! what apb wrote: a copy whose digest still equals it is pristine, the same
//! rule as the official connectors. A copy that holds exactly the shipped
//! files is apb's even without its sidecar (an install interrupted between
//! the tree and the sidecar), and the sidecar is written again. Any other
//! skill of the same name without that sidecar is someone else's and is
//! never overwritten. A copy is never moved to an older shipped version
//! than its sidecar names.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{BundledSkill, InstallState, Reconciled};
use crate::tree_install::{TreeInstallError, dir_matches, install_tree};

/// The suffix of the sidecar beside a shipped skill.
const SIDECAR_SUFFIX: &str = ".apb-bundled.json";
/// The infix of the backup of a changed copy a person replaced
/// (`.<name>.apb-bundled-backup-<ms>` beside it).
const BACKUP_INFIX: &str = ".apb-bundled-backup-";

/// The directories a shipped skill is installed into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDirs {
    /// The canonical skills directory the copy lives in.
    pub skills: PathBuf,
    /// The claude convention's directory the copy is bridged into, if any.
    pub claude: Option<PathBuf>,
}

impl SkillDirs {
    /// `~/.agents/skills`, bridged into `~/.claude/skills`. None if HOME is
    /// not set.
    pub fn global() -> Option<Self> {
        Some(Self {
            skills: crate::skills::global_skills_dir()?,
            claude: crate::agents::claude::global_skills_dir(),
        })
    }
}

/// What [`install_skill`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillInstalled {
    pub name: String,
    pub version: String,
    /// The tree digest of the installed copy.
    pub digest: String,
    /// The copy already held the shipped content.
    pub no_op: bool,
    /// Why the claude bridge was not made, if it was not.
    pub bridge_notes: Vec<String>,
    /// Where the changed copy a replace moved aside now is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<PathBuf>,
    /// Role profiles whose approval could not follow the new skill.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub trust_notes: Vec<String>,
}

/// Why a shipped skill was not installed.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("not_bundled: `{0}` is not a skill apb ships")]
    NotBundled(String),
    #[error("no_skills_dir: no global skills directory (HOME is not set)")]
    NoSkillsDir,
    #[error(
        "foreign: `{0}` is a skill apb did not install; it is never overwritten (rename or remove it first)"
    )]
    Foreign(String),
    #[error(
        "modified: `{0}` was changed after apb installed it; replacing it on request moves the changed copy aside and installs the shipped version"
    )]
    Modified(String),
    #[error(
        "newer_installed: newer defaults installed, leaving them: the skill `{name}` is the shipped {installed}, newer than this apb's {shipped}"
    )]
    NewerInstalled {
        name: String,
        installed: String,
        shipped: String,
    },
    #[error("io: {0}")]
    Io(String),
}

impl SkillError {
    /// The machine-readable code every surface answers with.
    pub fn code(&self) -> &'static str {
        match self {
            SkillError::NotBundled(_) => "not_bundled",
            SkillError::NoSkillsDir => "no_skills_dir",
            SkillError::Foreign(_) => "foreign",
            SkillError::Modified(_) => "modified",
            SkillError::NewerInstalled { .. } => "newer_installed",
            SkillError::Io(_) => "io",
        }
    }
}

/// The sidecar of a shipped skill: what apb wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Sidecar {
    name: String,
    version: String,
    digest: String,
}

fn sidecar_path(skills_dir: &Path, name: &str) -> PathBuf {
    skills_dir.join(format!(".{name}{SIDECAR_SUFFIX}"))
}

fn read_sidecar(skills_dir: &Path, name: &str) -> Option<Sidecar> {
    let raw = std::fs::read_to_string(sidecar_path(skills_dir, name)).ok()?;
    serde_json::from_str::<Sidecar>(&raw)
        .ok()
        .filter(|s| s.name == name)
}

/// Where the installed copy of a shipped skill stands.
enum State {
    Absent,
    /// Not apb's: a link, a file, or a folder without a sidecar that does
    /// not hold the shipped files.
    Foreign,
    /// Holds exactly the shipped files; `sidecar_ok` is false when the
    /// sidecar is missing or out of date and must be written again.
    UpToDate {
        sidecar_ok: bool,
    },
    Pristine(Sidecar),
    Modified(Sidecar),
}

fn state(skills_dir: &Path, skill: &BundledSkill) -> State {
    let target = skills_dir.join(&skill.name);
    match std::fs::symlink_metadata(&target) {
        Err(_) => return State::Absent,
        Ok(meta) if !meta.file_type().is_dir() => return State::Foreign,
        Ok(_) => {}
    }
    let sidecar = read_sidecar(skills_dir, &skill.name);
    if dir_matches(&target, &skill.files) {
        let sidecar_ok = sidecar
            .is_some_and(|s| s.version == skill.version && Some(s.digest) == tree_digest(&target));
        return State::UpToDate { sidecar_ok };
    }
    let Some(sidecar) = sidecar else {
        return State::Foreign;
    };
    if tree_digest(&target).as_deref() == Some(sidecar.digest.as_str()) {
        State::Pristine(sidecar)
    } else {
        State::Modified(sidecar)
    }
}

fn tree_digest(dir: &Path) -> Option<String> {
    crate::content::tree_digest(dir, &crate::content::TreeLimits::default()).ok()
}

/// The version a pristine or changed copy was installed as, when it is
/// newer than `skill`: a copy this binary must leave alone.
fn newer_installed(state: &State, skill: &BundledSkill) -> Option<String> {
    match state {
        State::Pristine(s) | State::Modified(s) if super::is_older(&skill.version, &s.version) => {
            Some(s.version.clone())
        }
        _ => None,
    }
}

/// Where the installed copy of the shipped skill `skill` stands. Read-only.
/// A newer shipped copy than this binary's reads as up to date: there is
/// nothing for this binary to do.
pub fn skill_state(skill: &BundledSkill) -> InstallState {
    let Some(dirs) = SkillDirs::global() else {
        return InstallState::Absent;
    };
    let state = state(&dirs.skills, skill);
    if let Some(installed) = newer_installed(&state, skill) {
        return InstallState::UpToDate { installed };
    }
    match state {
        State::Absent => InstallState::Absent,
        State::Foreign => InstallState::Foreign,
        State::UpToDate { .. } => InstallState::UpToDate {
            installed: skill.version.clone(),
        },
        State::Pristine(s) => InstallState::Pristine {
            installed: s.version,
        },
        State::Modified(s) => InstallState::Modified {
            installed: s.version,
        },
    }
}

/// Removes the installed shipped skill `name`, its sidecar and its claude
/// bridge (only a link that points at the copy). Only a copy nobody changed
/// goes: a modified one is refused ([`SkillError::Modified`]) and one apb
/// did not install is refused ([`SkillError::Foreign`]). `Ok(false)` when
/// it is not installed.
pub fn remove_skill(name: &str) -> Result<bool, SkillError> {
    let skill = super::skill(name).ok_or_else(|| SkillError::NotBundled(name.to_string()))?;
    let dirs = SkillDirs::global().ok_or(SkillError::NoSkillsDir)?;
    let _lock = lock()?;
    remove_skill_in(&dirs, &skill)
}

/// The lock of shipped content, as a skill error.
fn lock() -> Result<crate::fsutil::DirLock, SkillError> {
    super::hold().map_err(|e| SkillError::Io(e.to_string()))
}

pub(crate) fn remove_skill_in(dirs: &SkillDirs, skill: &BundledSkill) -> Result<bool, SkillError> {
    match state(&dirs.skills, skill) {
        State::Absent => return Ok(false),
        State::Foreign => return Err(SkillError::Foreign(skill.name.clone())),
        State::Modified(_) => return Err(SkillError::Modified(skill.name.clone())),
        State::UpToDate { .. } | State::Pristine(_) => {}
    }
    let target = dirs.skills.join(&skill.name);
    if let Some(claude) = &dirs.claude {
        let link = claude.join(&skill.name);
        let ours = std::fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink())
            && std::fs::read_link(&link).is_ok_and(|t| t == target);
        if ours {
            std::fs::remove_file(&link).map_err(|e| SkillError::Io(e.to_string()))?;
        }
    }
    std::fs::remove_dir_all(&target).map_err(|e| SkillError::Io(e.to_string()))?;
    match std::fs::remove_file(sidecar_path(&dirs.skills, &skill.name)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(SkillError::Io(e.to_string())),
    }
}

/// Installs the shipped skill `name` into the global skills directory and
/// bridges it for claude. Absent, already current or pristine: written (a
/// no-op when current). Changed after apb installed it: refused
/// ([`SkillError::Modified`]) unless `force`, which is
/// [`replace_modified_skill`]. A skill of that name apb did not install is
/// always refused ([`SkillError::Foreign`]), and so is a copy of a newer
/// shipped version ([`SkillError::NewerInstalled`]). Role profiles setup
/// wrote keep their approval across the new skill ([`super::role_trust`]).
/// Several installs in a row go through one [`SkillUpdates`] instead.
pub fn install_skill(name: &str, force: bool) -> Result<SkillInstalled, SkillError> {
    let mut batch = SkillUpdates::new();
    let mut done = batch.install(name, force)?;
    done.trust_notes = batch.finish();
    Ok(done)
}

/// Several installs of shipped skills that carry the role profiles' approval
/// once, after all of them: a role profile keeps its approval only when
/// every skill it carries holds the shipped files, so judging it after each
/// install would drop it while a later skill of the same batch is still
/// old. The approvals are read before the first install and carried by
/// [`SkillUpdates::finish`], which the caller runs once the batch is over
/// (whatever failed in between).
#[derive(Default)]
pub struct SkillUpdates {
    /// The role profiles as they stood before the first install.
    held: Option<Vec<super::role_trust::Held>>,
    /// The skills an install changed.
    updated: Vec<String>,
}

impl SkillUpdates {
    pub fn new() -> Self {
        Self::default()
    }

    /// [`install_skill`] of `name` without carrying the approvals yet.
    pub fn install(&mut self, name: &str, force: bool) -> Result<SkillInstalled, SkillError> {
        let skill = super::skill(name).ok_or_else(|| SkillError::NotBundled(name.to_string()))?;
        let dirs = SkillDirs::global().ok_or(SkillError::NoSkillsDir)?;
        let _lock = lock()?;
        if self.held.is_none() {
            self.held = Some(super::role_trust::snapshot());
        }
        let done = install_skill_in(&dirs, &skill, force)?;
        if !done.no_op {
            self.updated.push(skill.name.clone());
        }
        Ok(done)
    }

    /// Approves the new bundle of each role profile whose skills the batch
    /// changed, when only shipped content changed; a line for each approval
    /// that could not follow.
    pub fn finish(self) -> Vec<String> {
        let Some(held) = self.held else {
            return Vec::new();
        };
        if self.updated.is_empty() {
            return Vec::new();
        }
        match lock() {
            Ok(_lock) => super::role_trust::carry(held, &self.updated),
            Err(e) => vec![format!(
                "role profiles were not approved for the updated skills: {e}"
            )],
        }
    }
}

/// Replaces the shipped skill `name` a person changed with the shipped
/// version, on that person's explicit request: the changed copy is moved
/// aside to `.<name>.apb-bundled-backup-<ms>` in the same skills directory
/// (named in [`SkillInstalled::backup`]) and the shipped version is
/// installed. A copy nobody changed is simply installed or upgraded.
pub fn replace_modified_skill(name: &str) -> Result<SkillInstalled, SkillError> {
    install_skill(name, true)
}

/// Undoes a replace a person asked for ([`replace_modified_skill`]): the
/// changed copy kept at `backup` goes back in place of the shipped one, and
/// the sidecar goes, so the copy is the person's own again and apb never
/// touches it on its own. Only while the shipped copy there is unchanged
/// (or gone): one changed since is refused ([`SkillError::Modified`]).
/// `backup` must be a backup folder of `name` in the skills directory.
pub fn restore_replaced_skill(name: &str, backup: &Path) -> Result<(), SkillError> {
    let skill = super::skill(name).ok_or_else(|| SkillError::NotBundled(name.to_string()))?;
    let dirs = SkillDirs::global().ok_or(SkillError::NoSkillsDir)?;
    let _lock = lock()?;
    restore_replaced_in(&dirs, &skill, backup)
}

pub(crate) fn restore_replaced_in(
    dirs: &SkillDirs,
    skill: &BundledSkill,
    backup: &Path,
) -> Result<(), SkillError> {
    let ours = backup.parent() == Some(dirs.skills.as_path())
        && backup.file_name().is_some_and(|n| {
            n.to_string_lossy()
                .starts_with(&format!(".{}{BACKUP_INFIX}", skill.name))
        });
    if !ours {
        return Err(SkillError::Io(format!(
            "{} is not a backup of the skill `{}`",
            backup.display(),
            skill.name
        )));
    }
    if !std::fs::symlink_metadata(backup).is_ok_and(|m| m.file_type().is_dir()) {
        return Err(SkillError::Io(format!(
            "the changed copy is no longer at {}",
            backup.display()
        )));
    }
    let target = dirs.skills.join(&skill.name);
    match state(&dirs.skills, skill) {
        State::Foreign => return Err(SkillError::Foreign(skill.name.clone())),
        State::Modified(_) => return Err(SkillError::Modified(skill.name.clone())),
        State::Absent => {}
        State::UpToDate { .. } | State::Pristine(_) => {
            std::fs::remove_dir_all(&target).map_err(|e| SkillError::Io(e.to_string()))?;
        }
    }
    std::fs::rename(backup, &target).map_err(|e| {
        SkillError::Io(format!(
            "cannot put the changed copy back from {}: {e}",
            backup.display()
        ))
    })?;
    match std::fs::remove_file(sidecar_path(&dirs.skills, &skill.name)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(SkillError::Io(e.to_string())),
    }
}

pub(crate) fn install_skill_in(
    dirs: &SkillDirs,
    skill: &BundledSkill,
    force: bool,
) -> Result<SkillInstalled, SkillError> {
    let state = state(&dirs.skills, skill);
    if let Some(installed) = newer_installed(&state, skill) {
        return Err(SkillError::NewerInstalled {
            name: skill.name.clone(),
            installed,
            shipped: skill.version.clone(),
        });
    }
    let backup = match state {
        State::Foreign => return Err(SkillError::Foreign(skill.name.clone())),
        State::Modified(_) if !force => return Err(SkillError::Modified(skill.name.clone())),
        State::Modified(_) => Some(move_aside(dirs, skill)?),
        State::Absent | State::UpToDate { .. } | State::Pristine(_) => None,
    };
    let mut done = match (put_skill(dirs, skill), &backup) {
        (Ok(done), _) => done,
        (Err(e), None) => return Err(e),
        (Err(e), Some(aside)) => return Err(put_back(dirs, skill, aside, e)),
    };
    done.backup = backup;
    Ok(done)
}

/// Puts the changed copy a failed replace moved `aside` back in place, so
/// the skill is never left missing; the error says where the copy is when
/// even that fails.
fn put_back(dirs: &SkillDirs, skill: &BundledSkill, aside: &Path, e: SkillError) -> SkillError {
    let target = dirs.skills.join(&skill.name);
    let cleared = match std::fs::symlink_metadata(&target) {
        Err(_) => Ok(()),
        Ok(_) => std::fs::remove_dir_all(&target),
    };
    match cleared.and_then(|()| std::fs::rename(aside, &target)) {
        Ok(()) => e,
        Err(back) => SkillError::Io(format!(
            "{e}; the changed copy could not be put back ({back}) and stays at {}",
            aside.display()
        )),
    }
}

/// Moves the changed copy of `skill` aside, beside it, and returns where.
fn move_aside(dirs: &SkillDirs, skill: &BundledSkill) -> Result<PathBuf, SkillError> {
    let backup = dirs.skills.join(format!(
        ".{}{BACKUP_INFIX}{}",
        skill.name,
        crate::clock::now_ms()
    ));
    std::fs::rename(dirs.skills.join(&skill.name), &backup)
        .map_err(|e| SkillError::Io(format!("cannot move the changed copy aside: {e}")))?;
    Ok(backup)
}

/// Writes the shipped skill, its sidecar and its claude bridge. The target
/// is replaced only when it holds an older copy apb installed (pristine or
/// already moved aside): the caller decided that from its state.
fn put_skill(dirs: &SkillDirs, skill: &BundledSkill) -> Result<SkillInstalled, SkillError> {
    std::fs::create_dir_all(&dirs.skills).map_err(|e| SkillError::Io(e.to_string()))?;
    let installed =
        install_tree(&dirs.skills, &skill.name, &skill.files, |_| true).map_err(|e| match e {
            TreeInstallError::NeedsForce { .. } => SkillError::Modified(skill.name.clone()),
            TreeInstallError::Io(m) => SkillError::Io(m),
        })?;
    write_sidecar(dirs, skill, &installed.digest)?;
    let bridge_notes = dirs
        .claude
        .as_deref()
        .and_then(|claude| crate::skills::bridge_one(&dirs.skills.join(&skill.name), claude))
        .into_iter()
        .collect();
    Ok(SkillInstalled {
        name: skill.name.clone(),
        version: skill.version.clone(),
        digest: installed.digest,
        no_op: installed.no_op,
        bridge_notes,
        backup: None,
        trust_notes: Vec::new(),
    })
}

fn write_sidecar(dirs: &SkillDirs, skill: &BundledSkill, digest: &str) -> Result<(), SkillError> {
    let sidecar = Sidecar {
        name: skill.name.clone(),
        version: skill.version.clone(),
        digest: digest.to_string(),
    };
    let json = serde_json::to_vec_pretty(&sidecar).map_err(|e| SkillError::Io(e.to_string()))?;
    crate::fsutil::atomic_write_private(&sidecar_path(&dirs.skills, &skill.name), &json)
        .map_err(|e| SkillError::Io(e.to_string()))
}

/// Brings the installed shipped skills in `dirs` in line with `skills`,
/// under the caller's lock: a pristine copy is replaced, a changed one is
/// reported as [`Reconciled::UpdateAvailable`], a newer one as
/// [`Reconciled::NewerInstalled`], and a current copy whose sidecar is
/// missing or stale gets it written again. A skill that is not installed is
/// not installed here (only [`install_skill`] does that), and a skill apb
/// did not install is never touched. Idempotent: a second run reports
/// nothing.
pub(crate) fn reconcile_skills_in(dirs: &SkillDirs, skills: &[BundledSkill]) -> Vec<Reconciled> {
    let mut out = Vec::new();
    for skill in skills {
        let state = state(&dirs.skills, skill);
        if let Some(installed) = newer_installed(&state, skill) {
            out.push(Reconciled::NewerInstalled {
                name: skill.name.clone(),
                installed_version: installed,
                embedded_version: skill.version.clone(),
            });
            continue;
        }
        match state {
            State::Absent | State::Foreign | State::UpToDate { sidecar_ok: true } => {}
            State::UpToDate { sidecar_ok: false } => {
                let target = dirs.skills.join(&skill.name);
                let repaired = tree_digest(&target)
                    .ok_or_else(|| SkillError::Io("cannot digest the copy".to_string()))
                    .and_then(|d| write_sidecar(dirs, skill, &d));
                if let Err(e) = repaired {
                    out.push(Reconciled::Failed {
                        name: skill.name.clone(),
                        error: e.to_string(),
                    });
                }
            }
            State::Modified(sidecar) => out.push(Reconciled::UpdateAvailable {
                name: skill.name.clone(),
                installed_version: sidecar.version,
                embedded_version: skill.version.clone(),
            }),
            State::Pristine(sidecar) => out.push(match put_skill(dirs, skill) {
                Ok(done) => Reconciled::Updated {
                    name: skill.name.clone(),
                    from_version: sidecar.version,
                    to_version: done.version,
                },
                Err(e) => Reconciled::Failed {
                    name: skill.name.clone(),
                    error: e.to_string(),
                },
            }),
        }
    }
    out
}
