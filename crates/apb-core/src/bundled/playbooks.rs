//! Installing the shipped playbooks into the global store and keeping them
//! current across upgrades.
//!
//! A shipped playbook is an ordinary playbook of the global store: each
//! shipped content is one version (`created_by: bundled`), its exact digest
//! is trusted with origin `Bundled`, and `current` moves only through
//! [`current::move_current`], so every move has its history line and
//! `apb version use` rolls one back.
//!
//! A copy is pristine while the user never touched it: its current version
//! is one apb wrote and trusted as `Bundled`, no version after it is
//! anybody else's, no candidate is on trial, it is not frozen, and the last
//! move of `current` was not a person's (unless that move went back to the
//! newest shipped version apb installed). Only a pristine copy is moved to
//! new shipped content on its own ([`reconcile_playbooks_in`]); anything else
//! is reported as an update available, and a person replaces it on request
//! ([`replace_modified_playbook`]). A copy is never moved to an older
//! shipped version than the newest one installed.
//!
//! The install state lives on the machinery of [`current`]: every write
//! takes the playbook's pointer lock and runs [`current::recover`] first,
//! so an interrupted move (a person's rollback included) is settled before
//! anything is decided; one function ([`classify`]) decides from what
//! recovery left; and a new shipped version is written inside the move
//! itself ([`current::move_current_writing_locked`]), after its intent and
//! before `current` changes. A crash in between is an aborted move whose
//! written version stays apb's own: with `current` unset (a first install)
//! the copy reads as not installed and the next install reuses it, unless
//! it holds newer shipped content, which every path refuses to move back.
//! The read-only state ([`playbook_state`]) neither locks nor recovers: it
//! projects a pending intent the way recovery will settle it.

use std::path::{Path, PathBuf};

use super::{BundledPlaybook, InstallState, Reconciled};
use crate::fsutil::DirLock;
use crate::launch::floor::SaveChannel;
use crate::profile_store::PlaybookOrigin;
use crate::registry::list_versions;
use crate::trust::{Kind, OriginKind, TrustStore};
use crate::versioning::current::{self, MoveCurrent, MovedBy};
use crate::versioning::{
    ShippedVersion, VersioningError, canonical_definition, read_current_pointer,
    read_provenance_in, same_scripts, shipped_version_number, write_version_in, written_digest,
};

/// The `created_by` of a version apb wrote from shipped content.
pub const CREATED_BY: &str = "bundled";
/// The reason of the history line of an install.
const INSTALL_REASON: &str = "bundled install";
/// The reason of the history line of an upgrade.
const UPGRADE_REASON: &str = "bundled upgrade";
/// The reason of the history line of a replace a person asked for.
const REPLACE_REASON: &str = "bundled replace";
/// The reason of the history line that undoes a replace.
const RESTORE_REASON: &str = "bundled replace undone";
/// Where the digest of the shipped `evals/` last written is kept.
const EVALS_RECORD: &str = "meta/bundled-evals.digest";

/// What [`install_playbook`] did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PlaybookInstalled {
    pub id: String,
    /// The version `current` names afterwards.
    pub version: String,
    /// Its trust digest.
    pub digest: String,
    /// Nothing changed: `current` already held the shipped content.
    pub no_op: bool,
    /// The playbook is installed, but its trust could not be recorded.
    pub trust_warning: Option<String>,
}

/// Why a shipped playbook was not installed.
#[derive(Debug, thiserror::Error)]
pub enum BundledError {
    #[error("not_bundled: `{0}` is not a playbook apb ships")]
    NotBundled(String),
    #[error("no_config_dir: no config directory available")]
    NoConfigDir,
    #[error(
        "foreign: the global playbook `{0}` was not installed by apb; it is left as is (remove it or pick another id for it first)"
    )]
    Foreign(String),
    #[error(
        "modified: the global playbook `{id}` was changed or chosen locally after apb installed it (current {current}); replacing it on request installs the shipped {shipped} as a new version"
    )]
    Modified {
        id: String,
        current: String,
        shipped: String,
    },
    #[error(
        "newer_installed: newer defaults installed, leaving them: the global playbook `{id}` holds the shipped {installed}, newer than this apb's {shipped}"
    )]
    NewerInstalled {
        id: String,
        installed: String,
        shipped: String,
    },
    #[error(
        "own_versions: the global playbook `{0}` holds versions apb did not ship (or a candidate on trial); it is kept"
    )]
    OwnVersions(String),
    #[error(transparent)]
    Versioning(#[from] VersioningError),
    #[error("{0}")]
    Move(#[from] current::MoveError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl BundledError {
    /// The machine-readable code every surface answers with.
    pub fn code(&self) -> &'static str {
        match self {
            BundledError::NotBundled(_) => "not_bundled",
            BundledError::NoConfigDir => "no_config_dir",
            BundledError::Foreign(_) => "foreign",
            BundledError::Modified { .. } => "modified",
            BundledError::NewerInstalled { .. } => "newer_installed",
            BundledError::OwnVersions(_) => "own_versions",
            BundledError::Versioning(_) => "versioning",
            BundledError::Move(e) => e.code(),
            BundledError::Io(_) => "io",
        }
    }
}

/// The store shipped playbooks live in: the global one (`<config_dir>`,
/// holding `playbooks/`).
pub fn playbook_store() -> Option<PathBuf> {
    crate::store::global_playbooks_parent()
}

/// Where the installed copy of a shipped playbook stands, as [`classify`]
/// decides it from the settled pointers ([`current::settled`]).
#[derive(Debug)]
enum State {
    /// Not installed. `leftover`: versions of an install a crash
    /// interrupted before `current` existed are there, every one apb's own
    /// shipped content; installing reuses them, removing takes them.
    Absent { leftover: bool },
    /// A playbook of this id apb never wrote.
    Foreign,
    /// `current` holds exactly the shipped content.
    UpToDate { current: String },
    /// Untouched since apb installed it, but older content.
    Pristine { current: String },
    /// Changed or chosen by someone since apb installed it.
    Modified { current: String },
    /// apb installed newer shipped content than this binary's (`installed`,
    /// stored as `stored`): nothing here moves it back. `current` is unset
    /// for an install a newer binary's crash interrupted; `modified` says
    /// whether the copy is changed or chosen by someone.
    NewerInstalled {
        installed: String,
        stored: String,
        current: Option<String>,
        modified: bool,
    },
}

/// The versions of one installed playbook, read once.
struct Installed<'a> {
    dir: &'a Path,
    versions: Vec<String>,
    trust: &'a TrustStore,
    shipped: &'a BundledPlaybook,
}

impl<'a> Installed<'a> {
    fn read(dir: &'a Path, trust: &'a TrustStore, shipped: &'a BundledPlaybook) -> Self {
        Self {
            dir,
            versions: list_versions(dir).unwrap_or_default(),
            trust,
            shipped,
        }
    }

    /// Whether apb wrote `v` from shipped content.
    fn by_apb(&self, v: &str) -> bool {
        read_provenance_in(self.dir, v).is_some_and(|p| p.created_by == CREATED_BY)
    }

    /// Whether `v` is apb's own shipped content: written by apb and either
    /// trusted as `Bundled`, or holding exactly this binary's shipped
    /// content (written, then interrupted before its trust was recorded;
    /// installing records it, as it would for a fresh write).
    fn ours(&self, v: &str) -> bool {
        self.by_apb(v)
            && (written_digest(&self.dir.join(v))
                .is_ok_and(|d| self.trust.origin(&d) == Some(OriginKind::Bundled))
                || holds_shipped(self.dir, v, self.shipped))
    }

    /// The shipped version `v` was written from (the stored number when
    /// its provenance does not say).
    fn shipped_version(&self, v: &str) -> String {
        read_provenance_in(self.dir, v)
            .and_then(|p| p.shipped_version)
            .unwrap_or_else(|| v.to_string())
    }

    /// The newest shipped version apb installed: `(stored version, shipped
    /// version)`.
    fn newest_bundled(&self) -> Option<(String, String)> {
        self.versions
            .iter()
            .filter(|v| self.ours(v))
            .map(|v| (v.clone(), self.shipped_version(v)))
            .max_by_key(|(_, shipped)| crate::registry::parse_version(shipped))
    }

    /// Whether every version is apb's own, with no candidate on trial:
    /// nothing of anybody else's is in the playbook.
    fn all_ours(&self) -> bool {
        self.versions.iter().all(|v| self.ours(v))
            && crate::candidate_pointer::read_candidate(self.dir).is_none()
    }
}

/// The one decision over an installed shipped playbook: what `settled`
/// (the pointers as recovery leaves them) and the versions in
/// `playbook_dir` make of it. Every path (state, install, upgrade,
/// replace, remove, restore, reconcile) decides through it, so newer
/// shipped content is never moved back on any of them.
fn classify(
    playbook_dir: &Path,
    shipped: &BundledPlaybook,
    trust: &TrustStore,
    settled: &current::Settled,
) -> State {
    let installed = Installed::read(playbook_dir, trust, shipped);
    if installed.versions.is_empty() {
        return State::Absent { leftover: false };
    }
    let newest = installed.newest_bundled();
    let newer = newest
        .as_ref()
        .filter(|(_, v)| super::is_older(&shipped.version, v))
        .cloned();
    let Some(current) = settled.current.clone() else {
        if !installed.all_ours() {
            return State::Foreign;
        }
        return match newer {
            Some((stored, installed)) => State::NewerInstalled {
                installed,
                stored,
                current: None,
                modified: false,
            },
            None => State::Absent { leftover: true },
        };
    };
    if !installed.versions.iter().any(|v| installed.by_apb(v)) {
        return State::Foreign;
    }
    if holds_shipped(playbook_dir, &current, shipped) {
        return State::UpToDate { current };
    }
    let later_by_others = installed
        .versions
        .iter()
        .skip_while(|v| **v != current)
        .skip(1)
        .any(|v| !installed.ours(v));
    // A person's move holds the copy, unless it went back to the newest
    // shipped version apb installed.
    let by_person = matches!(settled.last_move_by, Some(MovedBy::Person { .. }));
    let held_by_person =
        by_person && newest.as_ref().map(|(v, _)| v.as_str()) != Some(current.as_str());
    let pristine = installed.ours(&current)
        && !later_by_others
        && crate::candidate_pointer::read_candidate(playbook_dir).is_none()
        && !crate::registry::is_frozen_dir(playbook_dir)
        && !held_by_person;
    match newer {
        Some((stored, installed)) => State::NewerInstalled {
            installed,
            stored,
            current: Some(current),
            modified: !pristine,
        },
        None if pristine => State::Pristine { current },
        None => State::Modified { current },
    }
}

impl From<State> for InstallState {
    fn from(s: State) -> Self {
        match s {
            State::Absent { .. } => InstallState::Absent,
            State::Foreign => InstallState::Foreign,
            State::UpToDate { current } => InstallState::UpToDate { installed: current },
            State::Pristine { current } => InstallState::Pristine { installed: current },
            State::Modified { current } => InstallState::Modified { installed: current },
            // Newer shipped content than this binary's: nothing for this
            // binary to do.
            State::NewerInstalled {
                current, stored, ..
            } => InstallState::UpToDate {
                installed: current.unwrap_or(stored),
            },
        }
    }
}

/// [`classify`] as the store stands, without the lock and without
/// writing: an interrupted move is projected the way recovery settles it
/// ([`current::settled`]). An intent that does not read is never acted on
/// by itself: the copy reads as modified (or foreign without `current`).
fn read_state(playbook_dir: &Path, shipped: &BundledPlaybook, trust: &TrustStore) -> State {
    match current::settled(playbook_dir) {
        Ok(settled) => classify(playbook_dir, shipped, trust, &settled),
        Err(_) => match read_current_pointer(playbook_dir) {
            Some(current) => State::Modified { current },
            None => State::Foreign,
        },
    }
}

/// Runs `op` on the playbook in `playbook_dir` under its pointer lock,
/// after recovering an interrupted move, with the state [`classify`]
/// decides from what recovery left. Every write of shipped content goes
/// through it. A playbook directory that does not exist is
/// `State::Absent` and is not created.
fn with_state<T>(
    playbook_dir: &Path,
    shipped: &BundledPlaybook,
    op: impl FnOnce(State, Option<&DirLock>) -> Result<T, BundledError>,
) -> Result<T, BundledError> {
    if !playbook_dir.is_dir() {
        return op(State::Absent { leftover: false }, None);
    }
    let held = current::lock(playbook_dir)?;
    current::recover(playbook_dir, &held)?;
    let settled = current::settled(playbook_dir)?;
    let state = classify(playbook_dir, shipped, &TrustStore::load(), &settled);
    op(state, Some(&held))
}

/// Where the installed copy of the shipped playbook `shipped` stands.
/// Read-only: nothing is locked, recovered or written; an interrupted move
/// reads as recovery will settle it.
/// Newer shipped content than this binary's reads as up to date: there is
/// nothing for this binary to do.
pub fn playbook_state(shipped: &BundledPlaybook) -> InstallState {
    match playbook_store() {
        Some(parent) => state_in(&parent, shipped),
        None => InstallState::Absent,
    }
}

/// [`playbook_state`] of `shipped` in the store `parent`.
pub(crate) fn state_in(parent: &Path, shipped: &BundledPlaybook) -> InstallState {
    read_state(
        &parent.join("playbooks").join(&shipped.id),
        shipped,
        &TrustStore::load(),
    )
    .into()
}

/// Moves the installed shipped playbook `id` to the global trash
/// (`<config_dir>/trash/<id>-<ts_millis>`, restorable like any trash
/// entry). Only a playbook that holds nothing but the versions apb shipped
/// goes: one with a version or a candidate of anybody else's is kept
/// ([`BundledError::OwnVersions`]), a modified one is refused
/// ([`BundledError::Modified`]) and one apb never wrote is refused
/// ([`BundledError::Foreign`]). `Ok(None)` when it is not installed.
pub fn remove_playbook(id: &str, ts_millis: u128) -> Result<Option<PathBuf>, BundledError> {
    let shipped = super::playbook(id).ok_or_else(|| BundledError::NotBundled(id.to_string()))?;
    let _lock = super::hold()?;
    remove_shipped(&shipped, ts_millis)
}

/// [`remove_playbook`] of the playbook `shipped`, under the caller's lock.
pub(crate) fn remove_shipped(
    shipped: &BundledPlaybook,
    ts_millis: u128,
) -> Result<Option<PathBuf>, BundledError> {
    let id = shipped.id.as_str();
    let parent = playbook_store().ok_or(BundledError::NoConfigDir)?;
    let dir = parent.join("playbooks").join(id);
    with_state(&dir, shipped, |state, _held| {
        let modified = |current: String| BundledError::Modified {
            id: id.to_string(),
            current,
            shipped: shipped.version.clone(),
        };
        match state {
            State::Absent { leftover: false } => return Ok(None),
            State::Foreign => return Err(BundledError::Foreign(id.to_string())),
            State::Modified { current }
            | State::NewerInstalled {
                current: Some(current),
                modified: true,
                ..
            } => return Err(modified(current)),
            State::Absent { leftover: true } | State::NewerInstalled { current: None, .. } => {}
            State::UpToDate { .. } | State::Pristine { .. } | State::NewerInstalled { .. } => {
                let trust = TrustStore::load();
                if !Installed::read(&dir, &trust, shipped).all_ours() {
                    return Err(BundledError::OwnVersions(id.to_string()));
                }
            }
        }
        let trashed = crate::versioning::delete_global_playbook(id, ts_millis)?;
        // The pointer lock moved with the directory: a trash entry keeps no
        // lock of its own.
        let _ = std::fs::remove_file(trashed.join(crate::candidate_pointer::CANDIDATE_LOCK_FILE));
        Ok(Some(trashed))
    })
}

/// Whether version `version` holds exactly what apb writes from the
/// shipped content at that version number: the same `playbook.yaml` bytes
/// and the same scripts. An equivalent but different text (reformatted,
/// commented) is not the shipped content, so its digest is never trusted as
/// `Bundled`.
fn holds_shipped(playbook_dir: &Path, version: &str, shipped: &BundledPlaybook) -> bool {
    let Ok(stored) = std::fs::read_to_string(playbook_dir.join(version).join("playbook.yaml"))
    else {
        return false;
    };
    canonical_definition(&shipped.yaml, &shipped.id, version).is_ok_and(|want| want == stored)
        && same_scripts(&playbook_dir.join(version), &shipped.scripts)
}

/// Installs the shipped playbook `id` into the global store and trusts its
/// exact digest as `Bundled`. Not installed: written and made current.
/// Already current: a no-op that re-records trust when it is missing.
/// Pristine but older: upgraded. Changed by someone: refused
/// ([`BundledError::Modified`]) unless `force`, which is
/// [`replace_modified_playbook`]. A playbook of this id apb never wrote is
/// always refused ([`BundledError::Foreign`]), and so is a move to older
/// content than the newest shipped version installed
/// ([`BundledError::NewerInstalled`]).
pub fn install_playbook(id: &str, force: bool) -> Result<PlaybookInstalled, BundledError> {
    let shipped = super::playbook(id).ok_or_else(|| BundledError::NotBundled(id.to_string()))?;
    let _lock = super::hold()?;
    install_shipped(&shipped, force)
}

/// Replaces the shipped playbook `id` a person changed (or held at another
/// version) with the shipped content, on that person's explicit request:
/// the shipped content becomes a new version and `current` moves to it
/// (history reason `bundled replace`). The person's versions stay in the
/// history, and `apb version use <version>` returns to one. A copy nobody
/// changed is simply installed or upgraded. Older shipped content than the
/// newest installed is refused ([`BundledError::NewerInstalled`]).
pub fn replace_modified_playbook(id: &str) -> Result<PlaybookInstalled, BundledError> {
    install_playbook(id, true)
}

/// Undoes a replace a person asked for ([`replace_modified_playbook`]):
/// `current` of the shipped playbook `id` moves back to `previous`, the
/// person's version the replace moved away from, as the person's own move
/// (so the copy stays theirs and is never upgraded on its own). Only while
/// `current` still holds shipped content nobody changed or chose since:
/// anything else is refused ([`BundledError::Modified`]). Returns the
/// version `current` moved away from; the shipped version stays in the
/// history.
pub fn restore_replaced_playbook(id: &str, previous: &str) -> Result<String, BundledError> {
    let shipped = super::playbook(id).ok_or_else(|| BundledError::NotBundled(id.to_string()))?;
    let _lock = super::hold()?;
    let parent = playbook_store().ok_or(BundledError::NoConfigDir)?;
    restore_replaced_in(&parent, &shipped, previous)
}

pub(crate) fn restore_replaced_in(
    parent: &Path,
    shipped: &BundledPlaybook,
    previous: &str,
) -> Result<String, BundledError> {
    let dir = parent.join("playbooks").join(&shipped.id);
    with_state(&dir, shipped, |state, held| {
        let current = match state {
            State::UpToDate { current }
            | State::Pristine { current }
            | State::NewerInstalled {
                current: Some(current),
                modified: false,
                ..
            } => current,
            State::Modified { current }
            | State::NewerInstalled {
                current: Some(current),
                ..
            } => {
                return Err(BundledError::Modified {
                    id: shipped.id.clone(),
                    current,
                    shipped: shipped.version.clone(),
                });
            }
            State::Foreign => return Err(BundledError::Foreign(shipped.id.clone())),
            State::Absent { .. } | State::NewerInstalled { current: None, .. } => {
                return Err(BundledError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "the global playbook `{}` is no longer installed",
                        shipped.id
                    ),
                )));
            }
        };
        let held = held.expect("an installed playbook has its directory");
        current::move_current_locked(
            &dir,
            &MoveCurrent {
                expected_from: Some(Some(&current)),
                reason: Some(RESTORE_REASON),
                ..MoveCurrent::new(
                    previous,
                    MovedBy::Person {
                        surface: current::Surface::Cli,
                    },
                )
            },
            held,
        )?;
        Ok(current)
    })
}

/// [`install_playbook`] of the playbook `shipped` (one of
/// [`super::playbooks`], or a test's own fixture), under the caller's lock.
pub(crate) fn install_shipped(
    shipped: &BundledPlaybook,
    force: bool,
) -> Result<PlaybookInstalled, BundledError> {
    let parent = playbook_store().ok_or(BundledError::NoConfigDir)?;
    install_playbook_in(&parent, shipped, force)
}

pub(crate) fn install_playbook_in(
    parent: &Path,
    shipped: &BundledPlaybook,
    force: bool,
) -> Result<PlaybookInstalled, BundledError> {
    std::fs::create_dir_all(parent.join("playbooks"))?;
    let playbook_dir = parent.join("playbooks").join(&shipped.id);
    let fresh = !playbook_dir.is_dir();
    if fresh {
        std::fs::create_dir(&playbook_dir)?;
    }
    let done = with_state(&playbook_dir, shipped, |state, held| {
        let held = held.expect("the playbook directory exists");
        match state {
            State::NewerInstalled { installed, .. } => Err(BundledError::NewerInstalled {
                id: shipped.id.clone(),
                installed,
                shipped: shipped.version.clone(),
            }),
            State::Absent { .. } => put_shipped(&playbook_dir, held, shipped, None, INSTALL_REASON),
            State::Foreign => Err(BundledError::Foreign(shipped.id.clone())),
            State::UpToDate { current } => {
                let digest = written_digest(&playbook_dir.join(&current))?;
                let trust_warning = if TrustStore::load().is_approved(&digest) {
                    None
                } else {
                    crate::trust::record_bundled(&digest, &shipped.id, Kind::Playbook)
                };
                sync_evals(&playbook_dir, shipped);
                Ok(PlaybookInstalled {
                    id: shipped.id.clone(),
                    version: current,
                    digest,
                    no_op: true,
                    trust_warning,
                })
            }
            State::Pristine { current } => {
                put_shipped(&playbook_dir, held, shipped, Some(&current), UPGRADE_REASON)
            }
            State::Modified { current } if force => {
                put_shipped(&playbook_dir, held, shipped, Some(&current), REPLACE_REASON)
            }
            State::Modified { current } => Err(BundledError::Modified {
                id: shipped.id.clone(),
                current,
                shipped: shipped.version.clone(),
            }),
        }
    });
    // A first install that failed before any version was written leaves
    // nothing behind.
    if done.is_err() && fresh && list_versions(&playbook_dir).is_ok_and(|v| v.is_empty()) {
        let _ = std::fs::remove_dir_all(&playbook_dir);
    }
    done
}

/// Makes the shipped content current, as one move of `current`: an
/// existing version apb wrote with exactly that content is reused (its
/// digest trusted as `Bundled`), else the move writes the new version and
/// trusts it after its intent and before `current` changes
/// ([`current::move_current_writing_locked`]), so a crash leaves a state
/// recovery finishes or records as aborted. `current` moves from `from`
/// (refused when it moved meanwhile) with `reason` in the history. The
/// caller holds the pointer lock (`held`) and has recovered.
fn put_shipped(
    playbook_dir: &Path,
    held: &DirLock,
    shipped: &BundledPlaybook,
    from: Option<&str>,
    reason: &str,
) -> Result<PlaybookInstalled, BundledError> {
    let trust = TrustStore::load();
    let installed = Installed::read(playbook_dir, &trust, shipped);
    let reusable = installed
        .versions
        .iter()
        .rev()
        .find(|v| installed.by_apb(v) && holds_shipped(playbook_dir, v, shipped))
        .cloned();
    let record = |version: &str| -> Result<(String, Option<String>), BundledError> {
        let digest = written_digest(&playbook_dir.join(version))?;
        let warning = crate::trust::record_bundled(&digest, &shipped.id, Kind::Playbook);
        Ok((digest, warning))
    };
    let parent = playbook_dir
        .parent()
        .and_then(Path::parent)
        .ok_or(BundledError::NoConfigDir)?;
    let (version, digest, trust_warning) = match reusable {
        Some(version) => {
            let (digest, warning) = record(&version)?;
            current::move_current_locked(
                playbook_dir,
                &MoveCurrent {
                    expected_from: Some(from),
                    reason: Some(reason),
                    ..MoveCurrent::created(&version)
                },
                held,
            )?;
            (version, digest, warning)
        }
        None => {
            let version = shipped_version_number(&installed.versions, &shipped.version);
            let mut recorded = None;
            current::move_current_writing_locked(
                playbook_dir,
                &MoveCurrent {
                    expected_from: Some(from),
                    reason: Some(reason),
                    ..MoveCurrent::created(&version)
                },
                held,
                &mut || {
                    let written = write_version_in(
                        parent,
                        PlaybookOrigin::Global,
                        &shipped.id,
                        &ShippedVersion {
                            yaml: &shipped.yaml,
                            version: &shipped.version,
                            scripts: &shipped.scripts,
                            created_by: CREATED_BY,
                        },
                        // An agent channel: shipped content never lowers a
                        // launch floor.
                        SaveChannel::Agent,
                    )
                    .map_err(current::MoveError::from)?;
                    if written != version {
                        return Err(current::MoveError::Io(std::io::Error::other(format!(
                            "the shipped version was written as {written}, not {version}"
                        ))));
                    }
                    recorded = Some(record(&version).map_err(|e| match e {
                        BundledError::Move(m) => m,
                        other => current::MoveError::Io(std::io::Error::other(other.to_string())),
                    })?);
                    Ok(())
                },
            )?;
            let (digest, warning) = recorded.expect("the move wrote the version");
            (version, digest, warning)
        }
    };
    sync_evals(playbook_dir, shipped);
    Ok(PlaybookInstalled {
        id: shipped.id.clone(),
        version,
        digest,
        no_op: false,
        trust_warning,
    })
}

/// Writes the shipped `evals/` when the playbook has none, or still holds
/// the ones apb wrote last (by the digest kept in [`EVALS_RECORD`]); a suite
/// someone changed is left alone. Best effort: an eval suite never decides
/// whether the playbook itself is installed.
fn sync_evals(playbook_dir: &Path, shipped: &BundledPlaybook) {
    if shipped.evals.is_empty() {
        return;
    }
    let record = playbook_dir.join(EVALS_RECORD);
    let recorded = std::fs::read_to_string(&record).ok();
    let limits = crate::content::TreeLimits::default();
    let installed = crate::tree_install::install_tree(
        playbook_dir,
        crate::eval::EVALS_DIR,
        &shipped.evals,
        |target| {
            crate::content::tree_digest(target, &limits)
                .ok()
                .is_some_and(|d| recorded.as_deref() == Some(d.as_str()))
        },
    );
    if let Ok(done) = installed
        && let Err(e) = crate::fsutil::atomic_write(&record, done.digest.as_bytes())
    {
        eprintln!(
            "apb: could not record the shipped eval suite of {}: {e}",
            playbook_dir.display()
        );
    }
}

/// Brings the installed shipped playbooks in the store `parent` in line with
/// `shipped` after an upgrade, under the caller's lock: a pristine copy is
/// moved to the new shipped content (history reason `bundled upgrade`), a
/// changed one is left alone and reported as [`Reconciled::UpdateAvailable`],
/// and one that holds newer shipped content than `shipped` is left alone and
/// reported as [`Reconciled::NewerInstalled`]. Playbooks that are not
/// installed, already current, or not apb's produce no entry, so a second
/// run of the same binary changes nothing.
pub(crate) fn reconcile_playbooks_in(
    parent: &Path,
    shipped: &[BundledPlaybook],
) -> Vec<Reconciled> {
    let mut out = Vec::new();
    for p in shipped {
        let playbook_dir = parent.join("playbooks").join(&p.id);
        let done = with_state(&playbook_dir, p, |state, held| {
            Ok(match state {
                State::NewerInstalled {
                    installed,
                    current: Some(_),
                    ..
                } => Some(Reconciled::NewerInstalled {
                    name: p.id.clone(),
                    installed_version: installed,
                    embedded_version: p.version.clone(),
                }),
                State::Absent { .. }
                | State::NewerInstalled { current: None, .. }
                | State::Foreign
                | State::UpToDate { .. } => None,
                State::Modified { current } => Some(Reconciled::UpdateAvailable {
                    name: p.id.clone(),
                    installed_version: current,
                    embedded_version: p.version.clone(),
                }),
                State::Pristine { current } => {
                    let held = held.expect("an installed playbook has its directory");
                    Some(
                        match put_shipped(&playbook_dir, held, p, Some(&current), UPGRADE_REASON) {
                            Ok(done) => Reconciled::Updated {
                                name: p.id.clone(),
                                from_version: current,
                                to_version: done.version,
                            },
                            Err(e) => Reconciled::Failed {
                                name: p.id.clone(),
                                error: e.to_string(),
                            },
                        },
                    )
                }
            })
        });
        match done {
            Ok(Some(r)) => out.push(r),
            Ok(None) => {}
            Err(e) => out.push(Reconciled::Failed {
                name: p.id.clone(),
                error: e.to_string(),
            }),
        }
    }
    out
}
