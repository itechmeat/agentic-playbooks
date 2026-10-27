//! Playbook minor-version machinery: the single point where the core emits
//! a version number (spec 10.2). Version folders are immutable; each change creates
//! a new version via atomic rename from a temporary folder.
//!
//! Note on YAML: `create_version` serializes the playbook back through
//! serde (canonical YAML), not preserving the original text verbatim, so
//! comments and formatting of the input YAML do not survive writing. For
//! the editor this is expected (source of truth - model), but hand edits with
//! comments will lose formatting.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::content::TreeFile;
use crate::fsutil::atomic_write;
use crate::registry::{Registry, is_frozen_dir, is_safe_segment, list_versions, parse_version};
use crate::schema::{Playbook, SchemaError};
use crate::validate::{Issue, Severity, ValidationContext, validate};

pub mod approval;
mod diff;
mod imported;

pub use diff::{VersionDiff, diff_definitions, line_diff};
pub use imported::{
    BundleOrigin, ImportProvenance, MAX_FILE_NAME, ScriptEntry, sanitize_file_name,
};

const MAX_RENAME_ATTEMPTS: u32 = 100;

#[derive(Debug, thiserror::Error)]
pub enum VersioningError {
    #[error("playbook `{0}` not found")]
    NotFound(String),
    #[error("{}", crate::validate::render_issues(.0))]
    Validation(Vec<Issue>),
    #[error("schema error: {0}")]
    Schema(String),
    #[error("version conflict: {0}")]
    Conflict(String),
    #[error("playbook `{0}` is frozen: definition changes are disabled")]
    Frozen(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Who made a version, stored beside it in `meta/<version>.yaml`. Which
/// version is in use is not part of it: the `current` pointer is the one
/// authority for that (see [`VersionInfo::is_current`]). Sidecars written by
/// older builds carry a `promoted` flag that drifted from `current` (a
/// rollback or a save that did not move `current` left it set); it is
/// ignored on read.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
pub struct VersionProvenance {
    pub created_by: String,
    pub run_id: Option<String>,
    pub classification: Option<String>,
    // --- forward patches and candidate trials (issue #192) ---
    /// `next_runs` for a forward patch (a candidate for the next runs);
    /// absent for a patch the run migrated onto (`current_run`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub scope: Option<String>,
    /// The version the patch was made from: the lineage of a candidate that
    /// replaced an earlier one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub base_version: Option<String>,
    /// Why the supervisor made the forward patch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub rationale: Option<String>,
    /// What the rationale rests on: journal seqs, node ids, durations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]
    pub evidence: Vec<String>,
    /// How the candidate fared in its trials; absent until a trial run ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub trial: Option<TrialRecord>,
    // --- end forward patches ---
    /// Where an imported version came from (`created_by: import`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub imported: Option<ImportProvenance>,
}

/// The trial record of a candidate version (issue #192): how many trial runs
/// it passed, and its fate once decided. A historical fact like the rest of
/// the sidecar: whether the version is in use is still `current` alone.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
pub struct TrialRecord {
    /// Trial runs that succeeded with every goal criterion holding.
    #[serde(default)]
    pub successes: u32,
    /// `promoted`, `rejected` or `superseded`; absent while it is still on
    /// trial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub outcome: Option<String>,
    /// The trial run that decided the outcome (for `superseded`: the run
    /// whose forward patch replaced it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub run_id: Option<String>,
    /// Why it was rejected, or the version that superseded it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotePolicy {
    OnSuccess,
    AfterNSuccesses(u32),
    Manual,
    Always,
}

fn advance_minor_pair(major: u32, minor: u32) -> Result<(u32, u32), ()> {
    match minor.checked_add(1) {
        Some(next_minor) => Ok((major, next_minor)),
        None => major
            .checked_add(1)
            .map(|next_major| (next_major, 0))
            .ok_or(()),
    }
}

/// Next available minor version from `base` = `X.Y.Z`: candidate `X.(Y+1).0`,
/// if collision with `existing` minor, increment, patch reset to 0.
/// Invalid `base` (not three numeric segments) yields safe default `1.0.0`.
/// If minor is exhausted, major is incremented (explicit safeguard against looping on `u32::MAX`).
pub fn next_minor_version(base: &str, existing: &[String]) -> String {
    let Some((mut major, minor, _)) = parse_version(base) else {
        return "1.0.0".to_string();
    };
    let taken: HashSet<&str> = existing.iter().map(String::as_str).collect();
    let Ok((next_major, mut next_minor)) = advance_minor_pair(major, minor) else {
        return format!("{major}.{minor}.0");
    };
    major = next_major;
    loop {
        let candidate = format!("{major}.{next_minor}.0");
        if !taken.contains(candidate.as_str()) {
            return candidate;
        }
        let Ok((next_major, next)) = advance_minor_pair(major, next_minor) else {
            return candidate;
        };
        major = next_major;
        next_minor = next;
    }
}

/// Next available patch version from `base` = `X.Y.Z`: candidate `X.Y.(Z+1)`.
/// Invalid `base` yields safe default `1.0.0`; create_patch_version
/// separately rejects such base before creating the version.
pub fn next_patch_version(base: &str, existing: &[String]) -> String {
    let Some((major, minor, patch)) = parse_version(base) else {
        return "1.0.0".to_string();
    };
    let taken: HashSet<&str> = existing.iter().map(String::as_str).collect();
    let Some(mut next_patch) = patch.checked_add(1) else {
        return next_minor_version(base, existing);
    };

    loop {
        let candidate = format!("{major}.{minor}.{next_patch}");
        if !taken.contains(candidate.as_str()) {
            return candidate;
        }
        let Some(next) = next_patch.checked_add(1) else {
            return next_minor_version(base, existing);
        };
        next_patch = next;
    }
}

/// Creates a new immutable minor version of a playbook: validation, temp build, atomic rename.
pub fn create_version(
    root: &Path,
    id: &str,
    new_yaml: &str,
    base_version: Option<&str>,
    make_current: bool,
) -> Result<String, VersioningError> {
    create_version_with_override(root, id, new_yaml, base_version, None, make_current)
}

/// Same as `create_version`, but when `version_override` is `Some`, that exact
/// `major.minor.patch` is used instead of the computed next minor version,
/// provided it is free. A `version_override` that is not valid semver, or
/// that already exists for this playbook, is rejected with
/// `VersioningError::Conflict` naming the problem. `version_override: None`
/// keeps today's auto-assign behavior unchanged.
pub fn create_version_with_override(
    root: &Path,
    id: &str,
    new_yaml: &str,
    base_version: Option<&str>,
    version_override: Option<&str>,
    make_current: bool,
) -> Result<String, VersioningError> {
    create_version_impl(
        root,
        id,
        &NewVersion {
            yaml: new_yaml,
            base_version,
            version_override,
            make_current,
            imported_scripts: None,
            imported: None,
        },
    )
}

/// What [`create_version_impl`] writes. `imported_scripts` is `Some` only for
/// an import of a format-2 bundle (see [`crate::bundle`]): those files (maybe
/// none) become the new version's `scripts/` and the base version's scripts
/// are not carried. `None` carries the base's scripts along, as every
/// ordinary save does. `imported` is `Some` for every import: the version
/// is then recorded as `created_by: import` with that provenance, its
/// `digest` set to the trust digest of the version written.
struct NewVersion<'a> {
    yaml: &'a str,
    base_version: Option<&'a str>,
    version_override: Option<&'a str>,
    make_current: bool,
    imported_scripts: Option<&'a [TreeFile]>,
    imported: Option<&'a ImportProvenance>,
}

fn create_version_impl(
    root: &Path,
    id: &str,
    req: &NewVersion<'_>,
) -> Result<String, VersioningError> {
    if !is_safe_segment(id) {
        return Err(VersioningError::NotFound(id.to_string()));
    }

    let playbook_dir = playbooks_dir(root).join(id);
    let is_new = !playbook_dir.is_dir();

    // A frozen playbook refuses every new version. A brand-new playbook cannot
    // be frozen (it does not exist yet), so only guard existing ones.
    if !is_new && is_frozen_dir(&playbook_dir) {
        return Err(VersioningError::Frozen(id.to_string()));
    }

    let base = if is_new {
        None
    } else if let Some(v) = req.base_version {
        if !is_safe_segment(v) {
            return Err(VersioningError::NotFound(format!("{id}@{v}")));
        }
        Some(v.to_string())
    } else {
        Some(read_current(&playbook_dir)?)
    };

    let existing = if is_new {
        Vec::new()
    } else {
        list_versions(&playbook_dir)?
    };

    let (mut version, bump) = if let Some(v) = req.version_override {
        if !is_safe_segment(v) {
            return Err(VersioningError::NotFound(format!("{id}@{v}")));
        }
        if parse_version(v).is_none() {
            return Err(VersioningError::Conflict(format!("invalid version `{v}`")));
        }
        if existing.iter().any(|e| e.as_str() == v) {
            return Err(VersioningError::Conflict(format!(
                "version `{v}` already exists for playbook `{id}`"
            )));
        }
        (v.to_string(), VersionBump::Fixed)
    } else if is_new {
        ("1.0.0".to_string(), VersionBump::Minor)
    } else {
        (
            next_minor_version(base.as_deref().unwrap_or("1.0.0"), &existing),
            VersionBump::Minor,
        )
    };

    let mut playbook = Playbook::from_yaml(req.yaml).map_err(schema_err)?;
    playbook.version = version.clone();
    playbook.id = id.to_string();

    validate_playbook(root, &playbook)?;

    if is_new {
        fs::create_dir_all(&playbook_dir)?;
    }

    let scripts = match req.imported_scripts {
        Some(files) => VersionScripts::Imported(files),
        None => VersionScripts::CarryFrom(base.as_deref()),
    };
    // Scripts carried from a base an import wrote, and nobody approved yet,
    // are still that import's code: the new version keeps its record, so
    // the approval by digest stays required (a save is no way around it).
    let inherited = match req.imported_scripts {
        None => inherited_import(&playbook_dir, base.as_deref()),
        Some(_) => None,
    };
    let imported = match req.imported {
        Some(own) if own.scripts.is_empty() => Some(ImportProvenance {
            scripts: inherited.map(|i| i.scripts).unwrap_or_default(),
            ..own.clone()
        }),
        Some(own) => Some(own.clone()),
        None => inherited,
    };
    let version_path = match commit_version_dir(&playbook_dir, &version, &playbook, scripts, bump) {
        Ok(path) => path,
        Err(e) => {
            // A playbook this call created has no version and no `current`:
            // leave nothing, so the next attempt starts it afresh.
            if is_new {
                discard_new_playbook_dir(&playbook_dir);
            }
            return Err(e);
        }
    };
    version = version_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let provenance = VersionProvenance {
        created_by: if req.imported.is_some() {
            "import"
        } else {
            "user"
        }
        .to_string(),
        ..Default::default()
    };
    if let Err(e) = finish_version(root, id, &version, base.as_deref(), provenance, imported) {
        let _ = remove_dir_all(&version_path);
        if is_new {
            discard_new_playbook_dir(&playbook_dir);
        }
        return Err(e);
    }

    if is_new || req.make_current {
        atomic_write(&playbook_dir.join("current"), version.as_bytes())?;
    }
    // A version a person writes ends the trial of a forward patch made from
    // an older line: promoting that candidate later would undo this one.
    if !is_new {
        supersede_candidate(&playbook_dir, None, &format!("a person created {version}"))?;
    }

    Ok(version)
}

/// The trust digest of the committed version directory `version_dir`.
fn written_digest(version_dir: &Path) -> io::Result<String> {
    let yaml = fs::read_to_string(version_dir.join("playbook.yaml"))?;
    crate::scope::definition_digest(&yaml, version_dir).map_err(io::Error::other)
}

/// The import record of `base` (a version of the playbook in
/// `playbook_dir`) that a version carrying its scripts keeps: the base was
/// imported with scripts (or kept such a record itself) and its content is
/// not approved. A base whose digest cannot be read counts as not approved.
pub(crate) fn inherited_import(
    playbook_dir: &Path,
    base: Option<&str>,
) -> Option<ImportProvenance> {
    let base = base?;
    let imported = read_provenance_in(playbook_dir, base)?.imported?;
    let approved = written_digest(&playbook_dir.join(base))
        .is_ok_and(|d| crate::trust::TrustStore::load().is_approved(&d));
    (!imported.scripts.is_empty() && !approved).then_some(imported)
}

/// Everything a committed `version` gets after its directory: the layout
/// of `base`, and its provenance with `imported` (see [`write_with_import`]).
/// On a failure the caller removes the version: one whose import record
/// did not land would run without the approval it needs.
fn finish_version(
    root: &Path,
    id: &str,
    version: &str,
    base: Option<&str>,
    provenance: VersionProvenance,
    imported: Option<ImportProvenance>,
) -> Result<(), VersioningError> {
    if let Some(base) = base {
        copy_parent_layout(&playbooks_dir(root).join(id), base, version)?;
    }
    write_with_import(root, id, version, provenance, imported)
}

/// Writes `provenance` for the committed `version`, with `imported` (when
/// there is one) stamped with the trust digest of the version written: what
/// an approval of it must name.
fn write_with_import(
    root: &Path,
    id: &str,
    version: &str,
    mut provenance: VersionProvenance,
    imported: Option<ImportProvenance>,
) -> Result<(), VersioningError> {
    if let Some(imported) = imported {
        let digest = written_digest(&playbooks_dir(root).join(id).join(version))?;
        provenance.imported = Some(ImportProvenance { digest, ..imported });
    }
    write_provenance(root, id, version, &provenance)
}

/// The one save path for a playbook definition written through apb: MCP
/// `playbook_create` / `playbook_update`, the dashboard editor, and
/// `apb import`. Creates the new version exactly like
/// [`create_version_with_override`] and then applies the one trust rule for
/// saves: a definition saved through apb is a user-authorized local write,
/// so the digest of the version it wrote is approved (`LocallyApproved`,
/// spec 3.1 and the profiles spec 9.4: auto-approve is trust in the result of
/// an authorized operation). Content that changes outside apb - a hand edit,
/// a `git pull`, a copied directory - never passes here and stays untrusted.
///
/// The save writes the YAML, not the scripts: the new version carries the
/// base version's `scripts/` along. So the result is approved only when that
/// carried code was already approved - the new version has no scripts, or the
/// base version's own digest is approved and the scripts are unchanged. A save
/// on top of scripts nobody approved (a repository update, a cloned project)
/// leaves the new version untrusted like its base.
///
/// Approval is best effort: the version is already committed when it runs,
/// so a trust-store write failure is reported on stderr rather than turning
/// a completed save into an error (the playbook then runs only with an
/// explicit acknowledge, like any untrusted one).
///
/// A save whose definition equals the current version's (issue #67 item 9:
/// compared in the stored, normalized form with the `version:` field set
/// aside, so a re-sent copy or a bare version bump counts as no change)
/// writes nothing: no version is created and the current one is returned
/// with [`Saved::unchanged`] set. Trust is untouched, because nothing new was
/// written. An explicit `version_override` always creates that version.
pub fn save_definition(
    root: &Path,
    id: &str,
    yaml: &str,
    version_override: Option<&str>,
    make_current: bool,
) -> Result<Saved, VersioningError> {
    let base = if is_safe_segment(id) {
        read_current_pointer(&playbooks_dir(root).join(id))
    } else {
        None
    };
    if version_override.is_none()
        && let Some(current) = base.as_deref()
        && same_definition(&playbooks_dir(root).join(id), current, id, yaml)
    {
        return Ok(Saved {
            version: current.to_string(),
            unchanged: true,
        });
    }
    let version =
        create_version_with_override(root, id, yaml, None, version_override, make_current)?;
    approve_written(root, id, &version, base.as_deref(), "saved");
    Ok(Saved {
        version,
        unchanged: false,
    })
}

/// What [`save_definition`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    /// The version the definition now lives in: the new one, or the current
    /// one when nothing changed.
    pub version: String,
    /// The definition equaled the current version's, so no version was
    /// created.
    pub unchanged: bool,
}

impl Saved {
    /// The answer every save surface (MCP, HTTP) gives for playbook `id`: the
    /// version, plus `unchanged: true` when nothing was written.
    pub fn answer(&self, id: &str) -> serde_json::Value {
        if self.unchanged {
            serde_json::json!({ "id": id, "version": self.version, "unchanged": true })
        } else {
            serde_json::json!({ "id": id, "version": self.version })
        }
    }
}

/// Where the scripts of an imported version come from.
#[derive(Debug, Clone, Copy)]
pub enum ImportedScripts<'a> {
    /// A format-2 bundle is authoritative: these files, and only these,
    /// become the version's `scripts/` (an empty list means none).
    Bundle(&'a [TreeFile]),
    /// A format-1 bundle has no notion of scripts: the current version's
    /// `scripts/` is carried along, as an ordinary save does.
    CarryCurrent,
}

/// The import path behind `apb import` (see [`crate::bundle`]). Creates the
/// YAML and its scripts as ONE new version: the scripts are written into the
/// version's staging `scripts/` directory and the directory is committed by
/// the usual atomic rename, so the definition digest that trust later binds
/// to ([`crate::scope::definition_digest`]) is computed over the final
/// content of a complete version, never over a YAML written first and
/// patched with scripts afterwards.
///
/// Trust: an imported version is never auto-approved, with or without
/// scripts. A bundle is a file from elsewhere that the local user did not
/// write, so nothing here vouches for it; the version stays untrusted until
/// the normal approval (MCP `playbook_approve` or an explicit acknowledge at
/// run start).
///
/// An import equal to the current version (the same definition, compared as
/// [`save_definition`] does, and for a format-2 bundle the same scripts)
/// writes nothing and returns the current version with [`Saved::unchanged`]
/// set; its trust is untouched. That holds when the bundle names no version
/// or names the current one (the same bundle imported again); any other
/// `version_override` creates that version.
///
/// The `scripts` paths must already be validated by the caller (the bundle's
/// shared script path check and its size limits); this function writes what
/// it is given.
pub fn save_imported(
    root: &Path,
    id: &str,
    yaml: &str,
    version_override: Option<&str>,
    make_current: bool,
    scripts: ImportedScripts<'_>,
    provenance: &ImportProvenance,
) -> Result<Saved, VersioningError> {
    if let Some(current) = unchanged_import(root, id, yaml, version_override, scripts) {
        return Ok(Saved {
            version: current,
            unchanged: true,
        });
    }
    let version = create_version_impl(
        root,
        id,
        &NewVersion {
            yaml,
            base_version: None,
            version_override,
            make_current,
            imported_scripts: match scripts {
                ImportedScripts::Bundle(files) => Some(files),
                ImportedScripts::CarryCurrent => None,
            },
            imported: Some(provenance),
        },
    )?;
    // No approve_written here, by design: an import cannot vouch for a
    // definition it did not produce.
    Ok(Saved {
        version,
        unchanged: false,
    })
}

/// The current version of playbook `id` when importing `yaml` with
/// `scripts` as `version_override` would write nothing: no other version is
/// named, the same definition (compared as [`save_definition`] does) and,
/// for a format-2 bundle, the same scripts. Shared by the import and its
/// preview, so both say the same.
pub(crate) fn unchanged_import(
    root: &Path,
    id: &str,
    yaml: &str,
    version_override: Option<&str>,
    scripts: ImportedScripts<'_>,
) -> Option<String> {
    if !is_safe_segment(id) {
        return None;
    }
    let playbook_dir = playbooks_dir(root).join(id);
    let current = read_current_pointer(&playbook_dir)?;
    if version_override.is_some_and(|v| v != current) {
        return None;
    }
    let same = same_definition(&playbook_dir, &current, id, yaml)
        && match scripts {
            ImportedScripts::CarryCurrent => true,
            ImportedScripts::Bundle(files) => same_scripts(&playbook_dir.join(&current), files),
        };
    same.then_some(current)
}

/// Whether the `scripts/` of `version_dir` holds exactly `files` (paths,
/// bytes and executable bits). A missing `scripts/` holds nothing; a tree
/// that cannot be read is never the same.
fn same_scripts(version_dir: &Path, files: &[TreeFile]) -> bool {
    let scripts_dir = version_dir.join("scripts");
    let mut have = if scripts_dir.is_dir() {
        match crate::content::read_tree_files(&scripts_dir, &crate::content::TreeLimits::default())
        {
            Ok((have, _)) => have,
            Err(_) => return false,
        }
    } else {
        Vec::new()
    };
    let mut want = files.to_vec();
    have.sort_by(|a, b| a.rel.cmp(&b.rel));
    want.sort_by(|a, b| a.rel.cmp(&b.rel));
    have == want
}

/// Whether `yaml`, stored as playbook `id`, would be the same definition as
/// `<playbook_dir>/<version>`: both sides go through the exact serialization
/// a save writes, with the version field and the id pinned, so formatting,
/// comments and the version number do not count as a change. Anything that
/// does not parse is a change (the save then reports its own error).
fn same_definition(playbook_dir: &Path, version: &str, id: &str, yaml: &str) -> bool {
    let normalized = |text: &str| canonical_definition(text, id, version).ok();
    let Ok(stored) = fs::read_to_string(playbook_dir.join(version).join("playbook.yaml")) else {
        return false;
    };
    match (normalized(&stored), normalized(yaml)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// `yaml` as a save writes it for playbook `id` at `version`: parsed and
/// serialized again with the id and the version pinned. What an export
/// carries, so the digest it records is the one the import computes.
pub(crate) fn canonical_definition(
    yaml: &str,
    id: &str,
    version: &str,
) -> Result<String, VersioningError> {
    let mut playbook = Playbook::from_yaml(yaml).map_err(schema_err)?;
    playbook.id = id.to_string();
    playbook_yaml_for_version(&playbook, version)
}

/// The trust half of a write through apb: approves the digest of
/// `<id>/<version>` (its YAML plus its scripts, see
/// [`crate::scope::definition_digest`]) as `LocallyApproved`, because the
/// user asked for that write. Shared by [`save_definition`] and
/// [`restore_from_trash`]. A version with scripts is approved only when they
/// were carried unchanged from `base`, a version whose own digest is already
/// approved; the write never produced them, so it cannot vouch for them.
/// A failure to record trust does not undo the write (the definition is on
/// disk either way); it is reported and the playbook stays untrusted.
fn approve_written(root: &Path, id: &str, version: &str, base: Option<&str>, what: &str) {
    let playbook_dir = playbooks_dir(root).join(id);
    let digest_of = |v: &str, yaml_of: &str| -> io::Result<(String, String)> {
        let yaml = fs::read_to_string(playbook_dir.join(yaml_of).join("playbook.yaml"))?;
        let d = crate::scope::definition_digest(&yaml, &playbook_dir.join(v))
            .map_err(io::Error::other)?;
        Ok((yaml, d))
    };
    let approved = digest_of(version, version).and_then(|(yaml, digest)| {
        let trust = crate::trust::TrustStore::load();
        let scripts_vouched = digest == crate::scope::digest_str(&yaml)
            || base.is_some_and(|b| {
                // The base's YAML digested over the NEW version's scripts equals
                // the base's approved digest exactly when the scripts are the
                // ones that approval covered.
                matches!(
                    (digest_of(b, b), digest_of(version, b)),
                    (Ok((_, base_digest)), Ok((_, carried)))
                        if carried == base_digest && trust.is_approved(&base_digest)
                )
            });
        if !scripts_vouched {
            return Ok(());
        }
        crate::trust::TrustStore::load().approve(
            &digest,
            id,
            crate::trust::OriginKind::LocallyApproved,
        )
    });
    if let Err(e) = approved {
        eprintln!("apb: {what} `{id}` {version} but could not record its trust: {e}");
    }
}

/// Creates immutable patch version from base version without changing current.
pub fn create_patch_version(
    root: &Path,
    id: &str,
    base_version: &str,
    new_yaml: &str,
    run_id: &str,
    classification: &str,
) -> Result<String, VersioningError> {
    create_patch_version_with(
        root,
        id,
        base_version,
        new_yaml,
        VersionProvenance {
            created_by: "supervisor".to_string(),
            run_id: Some(run_id.to_string()),
            classification: Some(classification.to_string()),
            ..Default::default()
        },
    )
}

/// [`create_patch_version`] with the provenance the caller composed: the
/// supervisor's in-run patch and its forward patch (`crate::candidate`)
/// share every step but the sidecar.
pub(crate) fn create_patch_version_with(
    root: &Path,
    id: &str,
    base_version: &str,
    new_yaml: &str,
    provenance: VersionProvenance,
) -> Result<String, VersioningError> {
    let classification = provenance.classification.as_deref().unwrap_or_default();
    if !is_safe_segment(id) {
        return Err(VersioningError::NotFound(id.to_string()));
    }
    if !is_safe_segment(base_version) {
        return Err(VersioningError::NotFound(format!("{id}@{base_version}")));
    }
    if !matches!(classification, "improvement" | "workaround") {
        return Err(VersioningError::Validation(vec![Issue {
            code: "classification",
            severity: Severity::Error,
            message: format!(
                "classification `{classification}` must be `improvement` or `workaround`"
            ),
            node: None,
        }]));
    }
    if parse_version(base_version).is_none() {
        return Err(VersioningError::Conflict(format!(
            "invalid version `{base_version}`"
        )));
    }

    let playbook_dir = playbooks_dir(root).join(id);
    if !playbook_dir.is_dir() || !playbook_dir.join(base_version).is_dir() {
        return Err(VersioningError::NotFound(format!("{id}@{base_version}")));
    }
    // Frozen playbooks reject supervisor patches too: the supervisor may only
    // intervene within the current run, never fork the definition.
    if is_frozen_dir(&playbook_dir) {
        return Err(VersioningError::Frozen(id.to_string()));
    }

    let existing = list_versions(&playbook_dir)?;
    let version = next_patch_version(base_version, &existing);
    let mut playbook = Playbook::from_yaml(new_yaml).map_err(schema_err)?;
    playbook.version = version.clone();
    playbook.id = id.to_string();
    validate_playbook(root, &playbook)?;

    let version_path = commit_version_dir(
        &playbook_dir,
        &version,
        &playbook,
        VersionScripts::CarryFrom(Some(base_version)),
        VersionBump::Patch,
    )?;
    let version = version_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let imported = provenance
        .imported
        .clone()
        .or_else(|| inherited_import(&playbook_dir, Some(base_version)));
    if let Err(e) = finish_version(root, id, &version, Some(base_version), provenance, imported) {
        let _ = remove_dir_all(&version_path);
        return Err(e);
    }

    Ok(version)
}

/// Writes the provenance of a version outside the immutable version folder.
pub fn write_provenance(
    root: &Path,
    id: &str,
    version: &str,
    provenance: &VersionProvenance,
) -> Result<(), VersioningError> {
    let path = provenance_path(root, id, version)?;
    let yaml =
        yaml_serde::to_string(provenance).map_err(|e| VersioningError::Schema(e.to_string()))?;
    atomic_write(&path, yaml.as_bytes())?;
    Ok(())
}

/// The provenance of `version` of the playbook in `playbook_dir`, `None`
/// when it has none or it does not read.
pub fn read_provenance_in(playbook_dir: &Path, version: &str) -> Option<VersionProvenance> {
    if !is_safe_segment(version) {
        return None;
    }
    let yaml =
        fs::read_to_string(playbook_dir.join("meta").join(format!("{version}.yaml"))).ok()?;
    yaml_serde::from_str(&yaml).ok()
}

/// Ends the trial of the candidate of the playbook in `playbook_dir`,
/// unless it is `keep`: removes the pointer and records the `superseded`
/// outcome with `reason` on its trial record. Returns the version it ended.
/// The caller holds the candidate lock (`_held`).
pub fn supersede_candidate_locked(
    playbook_dir: &Path,
    keep: Option<&str>,
    reason: &str,
    _held: &crate::fsutil::DirLock,
) -> Result<Option<String>, VersioningError> {
    let Some(candidate) = crate::candidate_pointer::read_candidate(playbook_dir) else {
        return Ok(None);
    };
    if keep == Some(candidate.as_str()) {
        return Ok(None);
    }
    crate::candidate_pointer::clear_candidate_if(playbook_dir, &candidate)?;
    let path = playbook_dir.join("meta").join(format!("{candidate}.yaml"));
    if path.is_file() {
        let mut provenance = read_provenance_in(playbook_dir, &candidate).unwrap_or_default();
        let mut trial = provenance.trial.take().unwrap_or_default();
        trial.outcome = Some(crate::candidate_pointer::OUTCOME_SUPERSEDED.to_string());
        trial.reason = Some(reason.to_string());
        provenance.trial = Some(trial);
        let yaml = yaml_serde::to_string(&provenance)
            .map_err(|e| VersioningError::Schema(e.to_string()))?;
        atomic_write(&path, yaml.as_bytes())?;
    }
    Ok(Some(candidate))
}

/// [`supersede_candidate_locked`] under the playbook's candidate lock.
pub fn supersede_candidate(
    playbook_dir: &Path,
    keep: Option<&str>,
    reason: &str,
) -> Result<Option<String>, VersioningError> {
    if crate::candidate_pointer::read_candidate(playbook_dir).is_none() {
        return Ok(None);
    }
    let lock =
        crate::fsutil::lock_dir(playbook_dir, crate::candidate_pointer::CANDIDATE_LOCK_FILE)?;
    supersede_candidate_locked(playbook_dir, keep, reason, &lock)
}

/// A person promotes `version` by hand (the dashboard): [`promote_version`],
/// and any other candidate on trial ends as `superseded`, so a forward
/// patch made from the line the person just moved past never runs or
/// promotes over their choice.
pub fn promote_version_by_person(
    root: &Path,
    id: &str,
    version: &str,
) -> Result<(), VersioningError> {
    promote_version(root, id, version)?;
    supersede_candidate(
        &playbooks_dir(root).join(id),
        None,
        &format!("a person promoted {version}"),
    )?;
    Ok(())
}

/// Returns provenance of version if sidecar already exists.
pub fn read_provenance(
    root: &Path,
    id: &str,
    version: &str,
) -> Result<Option<VersionProvenance>, VersioningError> {
    let path = provenance_path(root, id, version)?;
    if !path.is_file() {
        return Ok(None);
    }
    let yaml = fs::read_to_string(path)?;
    let provenance =
        yaml_serde::from_str(&yaml).map_err(|e| VersioningError::Schema(e.to_string()))?;
    Ok(Some(provenance))
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
pub struct VersionInfo {
    pub version: String,
    /// Whether `current` points at this version: the one source for "in use"
    /// (promoted) and the only one the listings report.
    pub is_current: bool,
    /// Whether the `candidate` pointer names this version: a forward patch
    /// on trial in the next runs (issue #192, see `crate::candidate`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub is_candidate: bool,
    /// An import wrote this version with scripts and its content is not
    /// approved: it runs only after an approval naming its digest
    /// ([`approval`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub awaiting_approval: bool,
    pub provenance: Option<VersionProvenance>,
}

/// Lists playbook versions with provenance and current marker.
/// Oldest first in semver order, like every version listing
/// ([`crate::registry::list_versions`]).
pub fn list_versions_with_provenance(
    root: &Path,
    id: &str,
) -> Result<Vec<VersionInfo>, VersioningError> {
    if !is_safe_segment(id) {
        return Err(VersioningError::NotFound(id.to_string()));
    }
    let playbook_dir = playbooks_dir(root).join(id);
    if !playbook_dir.is_dir() {
        return Err(VersioningError::NotFound(id.to_string()));
    }
    let current = fs::read_to_string(playbook_dir.join("current"))
        .ok()
        .map(|s| s.trim().to_string());
    let candidate = crate::candidate_pointer::read_candidate(&playbook_dir);
    let trust = crate::trust::TrustStore::load();
    let mut out = Vec::new();
    for version in list_versions(&playbook_dir)? {
        let is_current = current.as_deref() == Some(version.as_str());
        let is_candidate = candidate.as_deref() == Some(version.as_str());
        let provenance = read_provenance(root, id, &version)?;
        let awaiting_approval = imported_with_scripts(provenance.as_ref())
            && awaits_approval(root, id, &version, &trust);
        out.push(VersionInfo {
            version,
            is_current,
            is_candidate,
            awaiting_approval,
            provenance,
        });
    }
    Ok(out)
}

fn imported_with_scripts(p: Option<&VersionProvenance>) -> bool {
    p.and_then(|p| p.imported.as_ref())
        .is_some_and(|i| !i.scripts.is_empty())
}

/// Whether a version imported with scripts waits for an approval of its
/// content now (the rule of [`approval::import_pending`]); a version whose
/// digest cannot be read waits too.
fn awaits_approval(root: &Path, id: &str, version: &str, trust: &crate::trust::TrustStore) -> bool {
    !crate::registry::Registry::open(root)
        .and_then(|reg| reg.load(id, Some(version)))
        .and_then(|loaded| loaded.trust_digest())
        .is_ok_and(|digest| trust.is_approved(&digest))
}

/// Makes `version` the current one: a promotion (a supervisor patch that
/// passed its run, the dashboard's "Use") and a rollback are the same move of
/// the one `current` pointer. Any stored version qualifies, newer or older,
/// with or without a provenance sidecar.
///
/// What is required instead is a version that can actually run: the directory
/// must hold a `playbook.yaml`, so `current` never points at a half-written
/// or empty version directory.
pub fn promote_version(root: &Path, id: &str, version: &str) -> Result<(), VersioningError> {
    let playbook_dir = playbook_version_dir(root, id, version)?;
    // Promotion repoints `current`, a definition change - refused while frozen.
    if is_frozen_dir(&playbooks_dir(root).join(id)) {
        return Err(VersioningError::Frozen(id.to_string()));
    }
    if !playbook_dir.join(version).join("playbook.yaml").is_file() {
        return Err(VersioningError::NotFound(format!("{id}@{version}")));
    }
    atomic_write(&playbook_dir.join("current"), version.as_bytes())?;
    // A candidate a person promotes by hand is no longer on trial.
    crate::candidate_pointer::clear_candidate_if(&playbook_dir, version)?;
    Ok(())
}

/// Reads supervisor patch promotion policy from playbook description.
pub fn promote_policy(playbook: &Playbook) -> PromotePolicy {
    let Some(policy) = playbook
        .supervisor
        .as_ref()
        .and_then(|supervisor| supervisor.policy.as_ref())
    else {
        return PromotePolicy::OnSuccess;
    };
    let Ok(value) = serde_json::to_value(policy) else {
        return PromotePolicy::OnSuccess;
    };
    let Some(promotion) = value.get("promote_supervisor_patches") else {
        return PromotePolicy::OnSuccess;
    };

    match promotion {
        serde_json::Value::String(value) => match value.as_str() {
            "manual" => PromotePolicy::Manual,
            "always" => PromotePolicy::Always,
            "on_success" => PromotePolicy::OnSuccess,
            _ => PromotePolicy::OnSuccess,
        },
        serde_json::Value::Object(values) => values
            .get("after_n_successes")
            .and_then(serde_json::Value::as_u64)
            .and_then(|count| u32::try_from(count).ok())
            .map(PromotePolicy::AfterNSuccesses)
            .unwrap_or(PromotePolicy::OnSuccess),
        _ => PromotePolicy::OnSuccess,
    }
}

pub fn should_promote(
    policy: PromotePolicy,
    classification: &str,
    run_succeeded: bool,
    changed_nodes_succeeded: bool,
    prior_successes: u32,
) -> bool {
    if classification == "workaround" {
        return false;
    }

    match policy {
        PromotePolicy::Manual => false,
        PromotePolicy::Always => true,
        PromotePolicy::OnSuccess => run_succeeded && changed_nodes_succeeded,
        PromotePolicy::AfterNSuccesses(required) => {
            run_succeeded
                && changed_nodes_succeeded
                && prior_successes.saturating_add(1) >= required
        }
    }
}

/// Soft deletion of playbook: moves `.apb/playbooks/<id>` to
/// `.apb/trash/<id>-<ts_millis>`. Runs are not affected.
pub fn delete_playbook(root: &Path, id: &str, ts_millis: u128) -> Result<PathBuf, VersioningError> {
    if !is_safe_segment(id) {
        return Err(VersioningError::NotFound(id.to_string()));
    }

    let src = playbooks_dir(root).join(id);
    if !src.is_dir() {
        return Err(VersioningError::NotFound(id.to_string()));
    }

    let trash = trash_dir(root);
    fs::create_dir_all(&trash)?;

    let trash_name = format!("{id}-{ts_millis}");
    let dst = trash.join(&trash_name);
    if dst.exists() {
        return Err(VersioningError::Conflict(format!(
            "trash entry `{trash_name}` already exists"
        )));
    }

    fs::rename(&src, &dst)?;
    Ok(dst)
}

/// One deleted playbook in `.apb/trash/`, as every surface lists it
/// (dashboard, `apb trash list`, MCP `playbook_trash_list`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TrashEntry {
    /// The trash folder name `<id>-<deleted_at_ms>`: the exact handle a
    /// restore takes when one id was deleted more than once.
    pub name: String,
    /// The playbook id it restores to.
    pub id: String,
    /// When it was deleted, in Unix milliseconds.
    pub deleted_at_ms: u128,
    /// Its versions in semver order; a restore brings all of them back.
    pub versions: Vec<String>,
    /// The version its `current` pointer names, if any.
    pub current: Option<String>,
    /// A playbook with this id exists again, so a restore would be refused
    /// until that one is deleted.
    pub conflict: bool,
}

/// What a restore brought back.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RestoredPlaybook {
    pub id: String,
    /// The trash entry it came from.
    pub name: String,
    /// The restored `current` version (its digest is now approved).
    pub current: Option<String>,
    pub versions: Vec<String>,
}

/// The deleted playbooks of the project at `root`, newest deletion first.
/// Folders whose name is not `<id>-<millis>` (nothing apb wrote) are skipped.
/// No trash directory - empty list.
pub fn list_trash(root: &Path) -> Result<Vec<TrashEntry>, VersioningError> {
    let trash = trash_dir(root);
    if !trash.is_dir() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for entry in fs::read_dir(&trash)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let Some((id, deleted_at_ms)) = parse_trash_name(&name) else {
            continue;
        };
        let dir = entry.path();
        out.push(TrashEntry {
            conflict: playbooks_dir(root).join(&id).exists(),
            versions: list_versions(&dir)?,
            current: read_current_pointer(&dir),
            name,
            id,
            deleted_at_ms,
        });
    }
    out.sort_by(|a, b| {
        b.deleted_at_ms
            .cmp(&a.deleted_at_ms)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(out)
}

/// The version `current` names in `playbook_dir`, when it names one that
/// is a safe path segment (a tampered pointer such as `../x` reads as none).
pub(crate) fn read_current_pointer(playbook_dir: &Path) -> Option<String> {
    fs::read_to_string(playbook_dir.join("current"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && is_safe_segment(s))
}

/// Saves canvas layout for version. Layout is mutable: overwriting
/// existing `layouts/<version>.yaml` is allowed (unlike version folders).
pub fn save_layout(
    root: &Path,
    id: &str,
    version: &str,
    layout_yaml: &str,
) -> Result<(), VersioningError> {
    if !is_safe_segment(id) {
        return Err(VersioningError::NotFound(id.to_string()));
    }
    if !is_safe_segment(version) {
        return Err(VersioningError::NotFound(format!("{id}@{version}")));
    }

    let playbook_dir = playbooks_dir(root).join(id);
    if !playbook_dir.is_dir() {
        return Err(VersioningError::NotFound(id.to_string()));
    }

    let _: yaml_serde::Value =
        yaml_serde::from_str(layout_yaml).map_err(|e| VersioningError::Schema(e.to_string()))?;

    let path = playbook_dir.join("layouts").join(format!("{version}.yaml"));
    atomic_write(&path, layout_yaml.as_bytes())?;
    Ok(())
}

/// Compares two versions: structurally (nodes/edges) and line by line (YAML).
pub fn version_diff(
    root: &Path,
    id: &str,
    from: &str,
    to: &str,
) -> Result<VersionDiff, VersioningError> {
    if !is_safe_segment(id) {
        return Err(VersioningError::NotFound(id.to_string()));
    }
    if !is_safe_segment(from) {
        return Err(VersioningError::NotFound(format!("{id}@{from}")));
    }
    if !is_safe_segment(to) {
        return Err(VersioningError::NotFound(format!("{id}@{to}")));
    }

    let base = playbooks_dir(root).join(id);
    if !base.is_dir() {
        return Err(VersioningError::NotFound(id.to_string()));
    }

    let from_yaml = read_version_yaml(&base, id, from)?;
    let to_yaml = read_version_yaml(&base, id, to)?;

    diff_definitions(&from_yaml, &to_yaml)
}

fn read_version_yaml(base: &Path, id: &str, version: &str) -> Result<String, VersioningError> {
    let path = base.join(version).join("playbook.yaml");
    if !path.is_file() {
        return Err(VersioningError::NotFound(format!("{id}@{version}")));
    }
    Ok(fs::read_to_string(path)?)
}

/// Restores a deleted playbook: the one path behind the dashboard's Restore,
/// `apb trash restore` and MCP `playbook_trash_restore`.
///
/// `name` is a trash entry name (`<id>-<millis>`) or a playbook id; an id
/// picks its most recent deletion. The whole folder moves back to
/// `.apb/playbooks/<id>`, so every version, the `current` pointer, layouts
/// and provenance come back as they were (runs never moved). The restored
/// current definition then gets the trust of any other write through apb
/// (see [`save_definition`]): its digest is approved as `LocallyApproved`
/// when it has no scripts. A definition with scripts keeps whatever approval
/// its digest already had, since a restore cannot vouch for code it did not
/// write.
///
/// Errors: `NotFound` when nothing in the trash matches, `Conflict` when a
/// playbook with that id exists again (nothing is moved then).
pub fn restore_from_trash(root: &Path, name: &str) -> Result<RestoredPlaybook, VersioningError> {
    if !is_safe_segment(name) {
        return Err(VersioningError::NotFound(format!("`{name}` in trash")));
    }
    let entries = list_trash(root)?;
    let entry = entries
        .iter()
        .find(|e| e.name == name)
        // Newest first, so the first match by id is its latest deletion.
        .or_else(|| entries.iter().find(|e| e.id == name))
        .ok_or_else(|| VersioningError::NotFound(format!("`{name}` in trash")))?;

    let src = trash_dir(root).join(&entry.name);
    let dst = playbooks_dir(root).join(&entry.id);
    fs::create_dir_all(playbooks_dir(root))?;
    let conflict = || {
        VersioningError::Conflict(format!(
            "a playbook `{}` exists again; delete or rename it, then restore `{}`",
            entry.id, entry.name
        ))
    };
    if dst.exists() {
        return Err(conflict());
    }
    // A playbook created with the same id between the check and the move
    // makes the rename fail (the target is not empty), which is the same
    // conflict.
    match fs::rename(&src, &dst) {
        Ok(()) => {}
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            return Err(conflict());
        }
        Err(e) => return Err(e.into()),
    }
    if let Some(current) = &entry.current {
        approve_written(root, &entry.id, current, None, "restored");
    }
    Ok(RestoredPlaybook {
        id: entry.id.clone(),
        name: entry.name.clone(),
        current: entry.current.clone(),
        versions: entry.versions.clone(),
    })
}

/// The playbook store of a project root: every surface resolves a version
/// directory through the same helper the save paths use.
pub fn playbooks_dir(root: &Path) -> PathBuf {
    root.join(".apb/playbooks")
}

/// Creates draft playbook version 1.0.0 directly in the parent directory
/// (`<parent>/playbooks/`), without binding to the project root. Point for capturing in
/// chosen scope (spec 8.3): project `.apb` or global config dir.
/// Error `Conflict` if id already exists in this scope (dedup before write).
pub fn create_draft_in(
    parent: &Path,
    id: &str,
    yaml: &str,
    origin: crate::profile_store::PlaybookOrigin,
) -> Result<String, VersioningError> {
    if !is_safe_segment(id) {
        return Err(VersioningError::NotFound(id.to_string()));
    }
    fs::create_dir_all(parent.join("playbooks"))?;
    let playbook_dir = parent.join("playbooks").join(id);
    let mut playbook = Playbook::from_yaml(yaml).map_err(schema_err)?;
    playbook.version = "1.0.0".to_string();
    playbook.id = id.to_string();

    let reg = Registry::open_dir(parent).map_err(|e| VersioningError::NotFound(e.to_string()))?;
    // Origin is passed explicitly: global draft with `scope: project` should
    // fail V14 immediately, not only at runtime.
    let ctx = ValidationContext::for_registry(&reg, origin);
    let report = validate(&playbook, &ctx);
    let errors: Vec<Issue> = report
        .issues
        .into_iter()
        .filter(|i| i.severity == Severity::Error)
        .collect();
    if !errors.is_empty() {
        return Err(VersioningError::Validation(errors));
    }

    // Atomic collision check: create the playbook folder non-recursively -
    // if it already exists, it's a duplicate (no TOCTOU between exists() and write).
    match fs::create_dir(&playbook_dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return Err(VersioningError::Conflict(format!(
                "playbook `{id}` already exists in this scope"
            )));
        }
        Err(e) => return Err(e.into()),
    }

    let canonical =
        yaml_serde::to_string(&playbook).map_err(|e| VersioningError::Schema(e.to_string()))?;
    let vdir = playbook_dir.join("1.0.0");
    fs::create_dir_all(&vdir)?;
    atomic_write(&vdir.join("playbook.yaml"), canonical.as_bytes())?;
    atomic_write(&playbook_dir.join("current"), b"1.0.0")?;
    Ok("1.0.0".to_string())
}

fn provenance_path(root: &Path, id: &str, version: &str) -> Result<PathBuf, VersioningError> {
    let playbook_dir = playbook_version_dir(root, id, version)?;
    Ok(playbook_dir.join("meta").join(format!("{version}.yaml")))
}

fn playbook_version_dir(root: &Path, id: &str, version: &str) -> Result<PathBuf, VersioningError> {
    if !is_safe_segment(id) {
        return Err(VersioningError::NotFound(id.to_string()));
    }
    if !is_safe_segment(version) {
        return Err(VersioningError::NotFound(format!("{id}@{version}")));
    }

    let playbook_dir = playbooks_dir(root).join(id);
    if !playbook_dir.is_dir() || !playbook_dir.join(version).is_dir() {
        return Err(VersioningError::NotFound(format!("{id}@{version}")));
    }
    Ok(playbook_dir)
}

fn trash_dir(root: &Path) -> PathBuf {
    root.join(".apb/trash")
}

/// Splits a trash folder name `<id>-<ts_millis>` (at the last `-`) into the
/// id and the deletion time.
fn parse_trash_name(trash_name: &str) -> Option<(String, u128)> {
    let (id, ts) = trash_name.rsplit_once('-')?;
    if id.is_empty() || ts.is_empty() || !ts.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if !is_safe_segment(id) {
        return None;
    }
    Some((id.to_string(), ts.parse().ok()?))
}

fn schema_err(e: SchemaError) -> VersioningError {
    VersioningError::Schema(e.to_string())
}

fn validate_playbook(root: &Path, playbook: &Playbook) -> Result<(), VersioningError> {
    let issues = validation_errors(root, playbook)?;
    if issues.is_empty() {
        Ok(())
    } else {
        Err(VersioningError::Validation(issues))
    }
}

/// The errors (not warnings) that keep `playbook` from being saved in the
/// project at `root`.
pub(crate) fn validation_errors(
    root: &Path,
    playbook: &Playbook,
) -> Result<Vec<Issue>, VersioningError> {
    let reg = Registry::open(root).map_err(|e| VersioningError::NotFound(e.to_string()))?;
    let ctx = ValidationContext::for_registry(&reg, crate::profile_store::PlaybookOrigin::Project);
    Ok(validate(playbook, &ctx)
        .issues
        .into_iter()
        .filter(|issue| issue.severity == Severity::Error)
        .collect())
}

fn bump_minor(version: &str) -> Result<String, VersioningError> {
    let (major, minor, _) = parse_version(version)
        .ok_or_else(|| VersioningError::Conflict(format!("invalid version `{version}`")))?;
    let (next_major, next_minor) = advance_minor_pair(major, minor)
        .map_err(|()| VersioningError::Conflict(format!("version overflow for `{version}`")))?;
    Ok(format!("{next_major}.{next_minor}.0"))
}

fn bump_patch(version: &str) -> Result<String, VersioningError> {
    let (major, minor, patch) = parse_version(version)
        .ok_or_else(|| VersioningError::Conflict(format!("invalid version `{version}`")))?;
    let next_patch = patch
        .checked_add(1)
        .ok_or_else(|| VersioningError::Conflict(format!("version overflow for `{version}`")))?;
    Ok(format!("{major}.{minor}.{next_patch}"))
}

fn read_current(playbook_dir: &Path) -> Result<String, VersioningError> {
    let path = playbook_dir.join("current");
    if !path.is_file() {
        return Err(VersioningError::NotFound(
            playbook_dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
        ));
    }
    let current = fs::read_to_string(path)?.trim().to_string();
    if !is_safe_segment(&current) {
        return Err(VersioningError::NotFound(
            playbook_dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
        ));
    }
    Ok(current)
}

fn temp_dir_name(version: &str) -> String {
    let nanos = crate::clock::now_nanos();
    format!(".tmp-{version}-{nanos}")
}

fn playbook_yaml_for_version(
    playbook: &Playbook,
    version: &str,
) -> Result<String, VersioningError> {
    let mut playbook = playbook.clone();
    playbook.version = version.to_string();
    yaml_serde::to_string(&playbook).map_err(|e| VersioningError::Schema(e.to_string()))
}

#[derive(Clone, Copy)]
enum VersionBump {
    Minor,
    Patch,
    /// The version was pinned by an explicit override (see
    /// `create_version_with_override`): a collision on rename is a hard
    /// conflict, never silently bumped to the next free number.
    Fixed,
}

/// Where the scripts of a version being committed come from.
#[derive(Clone, Copy)]
enum VersionScripts<'a> {
    /// Carry the `scripts/` of this base version along (an ordinary save);
    /// `None` starts with an empty `scripts/`.
    CarryFrom(Option<&'a str>),
    /// Files an import brought (bundle format 2), already checked by the
    /// bundle's shared path rule.
    Imported(&'a [TreeFile]),
}

/// Builds the version in a staging directory (YAML and scripts together) and
/// commits it by one atomic rename, so a version is complete from its first
/// visible moment.
fn commit_version_dir(
    playbook_dir: &Path,
    initial_version: &str,
    playbook: &Playbook,
    scripts: VersionScripts<'_>,
    bump: VersionBump,
) -> Result<PathBuf, VersioningError> {
    let mut version = initial_version.to_string();

    for _ in 0..MAX_RENAME_ATTEMPTS {
        let validated_yaml = playbook_yaml_for_version(playbook, &version)?;
        let tmp = playbook_dir.join(temp_dir_name(&version));
        if tmp.exists() {
            remove_dir_all(&tmp)?;
        }
        // A failed staging step leaves no half-built directory behind.
        if let Err(e) = stage_version(playbook_dir, &tmp, &validated_yaml, scripts) {
            let _ = remove_dir_all(&tmp);
            return Err(e);
        }

        let target = playbook_dir.join(&version);
        match fs::rename(&tmp, &target) {
            Ok(()) => return Ok(target),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty
                ) =>
            {
                remove_dir_all(&tmp)?;
                version = match bump {
                    VersionBump::Minor => bump_minor(&version)?,
                    VersionBump::Patch => bump_patch(&version)?,
                    VersionBump::Fixed => {
                        let id = playbook_dir
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy();
                        return Err(VersioningError::Conflict(format!(
                            "version `{version}` already exists for playbook `{id}`"
                        )));
                    }
                };
                continue;
            }
            Err(e) => {
                let _ = remove_dir_all(&tmp);
                return Err(e.into());
            }
        }
    }

    Err(VersioningError::Conflict(format!(
        "failed to allocate version after {MAX_RENAME_ATTEMPTS} attempts"
    )))
}

/// Removes the directory a failed first commit created, but only while it is
/// empty: [`commit_version_dir`] already removed its own staging, so anything
/// still there belongs to a concurrent writer (a version it committed) and
/// must survive.
fn discard_new_playbook_dir(playbook_dir: &Path) {
    match fs::remove_dir(playbook_dir) {
        Ok(()) => {}
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
            ) => {}
        Err(e) => eprintln!(
            "apb: could not remove the empty playbook directory {}: {e}",
            playbook_dir.display()
        ),
    }
}

/// Writes the YAML and the scripts of a version into its staging directory
/// `tmp`.
fn stage_version(
    playbook_dir: &Path,
    tmp: &Path,
    validated_yaml: &str,
    scripts: VersionScripts<'_>,
) -> Result<(), VersioningError> {
    fs::create_dir_all(tmp.join("scripts"))?;
    atomic_write(&tmp.join("playbook.yaml"), validated_yaml.as_bytes())?;
    match scripts {
        VersionScripts::Imported(files) => write_imported_scripts(&tmp.join("scripts"), files)?,
        VersionScripts::CarryFrom(Some(base)) => {
            let scripts_src = playbook_dir.join(base).join("scripts");
            if scripts_src.is_dir() {
                crate::fsutil::copy_tree(&scripts_src, &tmp.join("scripts"))?;
            }
        }
        VersionScripts::CarryFrom(None) => {}
    }
    Ok(())
}

/// Writes validated bundle scripts into a staging `scripts/` directory.
/// Every path was already checked by the shared bundle script path check
/// (safe, relative segments only), so joining it cannot escape the
/// directory. Each file goes through [`atomic_write`]; the executable bit is
/// restored on unix and ignored elsewhere.
fn write_imported_scripts(scripts_dir: &Path, scripts: &[TreeFile]) -> Result<(), VersioningError> {
    for script in scripts {
        let dst = scripts_dir.join(&script.rel);
        crate::fsutil::atomic_write_under(scripts_dir, &dst, &script.bytes)?;
        set_executable_bit(&dst, script.executable)?;
    }
    Ok(())
}

fn set_executable_bit(path: &Path, executable: bool) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(if executable { 0o755 } else { 0o644 });
        fs::set_permissions(path, perms)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, executable);
        Ok(())
    }
}

fn copy_parent_layout(
    playbook_dir: &Path,
    base_version: &str,
    new_version: &str,
) -> Result<(), VersioningError> {
    let src = playbook_dir
        .join("layouts")
        .join(format!("{base_version}.yaml"));
    if !src.is_file() {
        return Ok(());
    }
    let content = fs::read_to_string(&src)?;
    let dst = playbook_dir
        .join("layouts")
        .join(format!("{new_version}.yaml"));
    atomic_write(&dst, content.as_bytes())?;
    Ok(())
}

fn remove_dir_all(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const VALID: &str = include_str!("../../tests/fixtures/valid.yaml");

    #[test]
    fn commit_version_dir_rewrites_yaml_on_collision_bump() {
        let dir = tempfile::tempdir().unwrap();
        let playbook_dir = dir.path().join("apb");
        fs::create_dir_all(playbook_dir.join("1.0.0/scripts")).unwrap();
        fs::create_dir_all(playbook_dir.join("1.1.0")).unwrap();
        fs::write(playbook_dir.join("1.1.0/occupant"), "taken").unwrap();

        let mut playbook = Playbook::from_yaml(VALID).unwrap();
        playbook.id = "implement-task".into();
        playbook.version = "1.1.0".into();

        let path = commit_version_dir(
            &playbook_dir,
            "1.1.0",
            &playbook,
            VersionScripts::CarryFrom(Some("1.0.0")),
            VersionBump::Minor,
        )
        .unwrap();
        assert_eq!(path.file_name().unwrap().to_string_lossy(), "1.2.0");

        let yaml = fs::read_to_string(path.join("playbook.yaml")).unwrap();
        assert!(yaml.contains("version: 1.2.0"));
        assert!(!yaml.contains("version: 1.1.0"));
    }

    #[test]
    fn failed_first_commit_keeps_a_concurrent_writers_version() {
        let dir = tempfile::tempdir().unwrap();
        let playbook_dir = dir.path().join("apb");
        fs::create_dir_all(playbook_dir.join("1.0.0")).unwrap();
        fs::write(playbook_dir.join("1.0.0/playbook.yaml"), "committed").unwrap();

        discard_new_playbook_dir(&playbook_dir);
        assert_eq!(
            fs::read_to_string(playbook_dir.join("1.0.0/playbook.yaml")).unwrap(),
            "committed"
        );

        fs::remove_dir_all(playbook_dir.join("1.0.0")).unwrap();
        discard_new_playbook_dir(&playbook_dir);
        assert!(!playbook_dir.exists());
    }

    #[test]
    fn frozen_playbook_refuses_new_versions_and_patches() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        crate::registry::init_project(root).unwrap();

        // A first version exists (this playbook is now an existing one).
        create_version(root, "implement-task", VALID, None, true).unwrap();
        let pb_dir = playbooks_dir(root).join("implement-task");
        assert!(pb_dir.join("1.0.0").is_dir());

        // Freeze it.
        fs::write(pb_dir.join(crate::registry::FROZEN_MARKER), "1").unwrap();

        // A new minor version is refused.
        let e1 = create_version(root, "implement-task", VALID, None, true).unwrap_err();
        assert!(matches!(e1, VersioningError::Frozen(_)), "got {e1:?}");

        // A supervisor patch is refused too.
        let e2 = create_patch_version(root, "implement-task", "1.0.0", VALID, "run1", "workaround")
            .unwrap_err();
        assert!(matches!(e2, VersioningError::Frozen(_)), "got {e2:?}");

        // Promotion (repoints current) is refused too.
        let e3 = promote_version(root, "implement-task", "1.0.0").unwrap_err();
        assert!(matches!(e3, VersioningError::Frozen(_)), "got {e3:?}");

        // Unfreezing lets a new version through again.
        fs::remove_file(pb_dir.join(crate::registry::FROZEN_MARKER)).unwrap();
        create_version(root, "implement-task", VALID, None, true).unwrap();
    }
}
