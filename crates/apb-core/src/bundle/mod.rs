//! A playbook version as one portable file (`apb export` / `apb import`):
//! the YAML, the editor layout and, since format 2, the version's
//! scripts ([`scripts`]).

mod preview;
mod scripts;

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::content::sha256_hex;
use crate::registry::{Registry, RegistryError};
use crate::versioning::{
    ImportProvenance, ImportedScripts, Saved, ScriptEntry, VersioningError, canonical_definition,
    playbooks_dir, sanitize_file_name, save_imported, save_layout,
};

pub use crate::versioning::BundleOrigin;
pub use preview::{ImportPreview, ScriptsDiff, preview};
pub use scripts::{BundleScript, ScriptsError};

/// Errors for exporting/importing a playbook as a single file.
#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("registry: {0}")]
    Registry(#[from] RegistryError),
    #[error("versioning: {0}")]
    Versioning(#[from] VersioningError),
    #[error("bundle format: {0}")]
    Format(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("yaml: {0}")]
    Yaml(String),
    #[error("bundle scripts: {0}")]
    Scripts(#[from] ScriptsError),
    /// The digest of the exported version could not be computed.
    #[error("digest of the exported version: {0}")]
    Digest(#[from] crate::content::ContentError),
}

/// A single portable playbook file: the playbook.yaml text plus the editor
/// layout (ui.xyflow) and, since format 2, the regular files under the
/// version's `scripts/` directory. The YAML is stored as a string, in the
/// form a save writes (see [`export_bundle`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybookBundle {
    /// Bundle format version (not the playbook version).
    pub apb_bundle: u32,
    pub id: String,
    pub version: String,
    pub playbook: String,
    /// Editor layout (layouts/<version>.yaml) as JSON; None - no layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<Value>,
    /// The regular files under the version's `scripts/` directory, each with
    /// a path relative to `scripts/`. Format 2 always writes the field; a
    /// format-1 bundle (or a version without scripts) reads as an empty list.
    #[serde(default)]
    pub scripts: Vec<BundleScript>,
    /// What the exporter recorded about the version it exported. An older
    /// apb ignores the field; a bundle without it imports the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<BundleOrigin>,
}

/// The file an import reads: its raw bytes (what the bundle digest covers)
/// and the name it was given, if any.
#[derive(Debug, Clone, Copy)]
pub struct ImportSource<'a> {
    pub raw: &'a [u8],
    pub file_name: Option<&'a str>,
}

pub const BUNDLE_SCHEMA: u32 = 2;
/// The oldest bundle format [`import_bundle`] still accepts: a format-1
/// bundle predates the `scripts` field, so it may not carry any, and its
/// import keeps the current version's scripts as an ordinary save does.
pub const OLDEST_SUPPORTED_SCHEMA: u32 = 1;

impl PlaybookBundle {
    pub fn to_json(&self) -> Result<String, BundleError> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn from_json(s: &str) -> Result<Self, BundleError> {
        Ok(serde_json::from_str(s)?)
    }

    /// The digest an import is accepted by: over the raw file when the
    /// bundle was read from one, else over its canonical JSON.
    pub fn digest(&self, raw: Option<&[u8]>) -> Result<String, BundleError> {
        Ok(match raw {
            Some(raw) => sha256_hex(raw),
            None => sha256_hex(self.to_json()?.as_bytes()),
        })
    }
}

/// Assembles a bundle from the registry: playbook.yaml of the specified (or
/// current) version plus its layout and its scripts. The scripts walk covers
/// the same tree the scope digest
/// ([`crate::scope::definition_digest`]) covers, with exactly
/// [`TreeLimits::default()`]; over any limit the export fails rather than
/// writing a silently partial bundle.
///
/// The YAML goes into the bundle in the form a save writes (see
/// [`crate::versioning::save_definition`]), and the recorded origin digest
/// is computed over that form: an import of the bundle under the same id
/// and version digests exactly like it. A version written by apb is
/// already in that form; a hand-edited one loses its comments and
/// formatting here, as it would on any save.
pub fn export_bundle(
    root: &Path,
    id: &str,
    version: Option<&str>,
) -> Result<PlaybookBundle, BundleError> {
    let reg = Registry::open(root)?;
    let loaded = reg.load(id, version)?;
    let version_dir = playbooks_dir(root).join(id).join(&loaded.version);
    let scripts = scripts::export_scripts(&version_dir)?;
    let yaml = canonical_definition(&loaded.yaml, id, &loaded.version)?;
    let digest = crate::scope::definition_digest(&yaml, &version_dir)?;
    Ok(PlaybookBundle {
        apb_bundle: BUNDLE_SCHEMA,
        id: id.to_string(),
        version: loaded.version,
        playbook: yaml,
        layout: loaded.layout,
        scripts,
        origin: Some(BundleOrigin {
            digest,
            scope: "project".to_string(),
            apb_version: env!("CARGO_PKG_VERSION").to_string(),
            exported_at_ms: crate::clock::now_ms_u64(),
        }),
    })
}

/// Imports a bundle into a project: creates a NEW playbook version under its
/// id - the YAML and its scripts as ONE version - validates it, and, if
/// present, saves the layout under the assigned version (never over the
/// current one on an unchanged import). Returns what was
/// saved: the assigned version, or the current one when the bundle equals it
/// ([`Saved::unchanged`], nothing written).
///
/// A format-2 bundle is authoritative about scripts: the imported version
/// holds exactly the bundle's `scripts`, none when the list is empty, and
/// never the current version's. A format-1 bundle has no scripts (one that
/// carries any is rejected), so its version keeps the current version's
/// scripts, as before format 2.
///
/// The incoming scripts are untrusted input and are fully validated (the
/// shared path rule, base64, size limits) before anything is written. An
/// imported version is never auto-approved, with or without scripts: it stays
/// untrusted until the normal approval.
///
/// The bundle's `version` field is honored when it is free (not already used
/// by this playbook): the imported version is created with that exact
/// number. When it is already taken, the import fails with a
/// `VersioningError::Conflict` naming the collision rather than silently
/// assigning a different version. When the bundle carries no version (an
/// empty string), the project's normal auto-assign scheme applies, same as
/// before this field was honored.
///
/// The version written records where it came from ([`ImportProvenance`]):
/// the digest of `source.raw` (or of the bundle's JSON without a source),
/// the sanitized base name of the file, the declared id and version, the
/// exporter's [`BundleOrigin`], the scripts by fingerprint and the final
/// trust digest. A version imported with scripts runs only after an approval
/// that names that digest (see [`crate::versioning::approval`]).
pub fn import_bundle(
    root: &Path,
    bundle: &PlaybookBundle,
    make_current: bool,
    source: Option<ImportSource<'_>>,
) -> Result<Saved, BundleError> {
    let checked = check_bundle(bundle)?;
    let scripts = checked.scripts();
    let provenance = ImportProvenance {
        bundle_digest: bundle.digest(source.map(|s| s.raw))?,
        file_name: source
            .and_then(|s| s.file_name)
            .and_then(sanitize_file_name),
        source_id: bundle.id.clone(),
        source_version: bundle.version.clone(),
        source: bundle.origin.clone(),
        imported_at_ms: crate::clock::now_ms_u64(),
        scripts: match scripts {
            ImportedScripts::Bundle(files) => files.iter().map(ScriptEntry::of).collect(),
            ImportedScripts::CarryCurrent => Vec::new(),
        },
        digest: String::new(),
    };
    let saved = save_imported(
        root,
        &bundle.id,
        &bundle.playbook,
        checked.version_override,
        make_current,
        scripts,
        &provenance,
    )?;
    // An unchanged import wrote nothing, and the current version's layout
    // stays as the person last arranged it.
    if let Some(layout) = bundle.layout.as_ref().filter(|_| !saved.unchanged) {
        let layout_yaml =
            yaml_serde::to_string(layout).map_err(|e| BundleError::Yaml(e.to_string()))?;
        save_layout(root, &bundle.id, &saved.version, &layout_yaml)?;
    }
    Ok(saved)
}

/// A bundle that passed the structural checks, with its scripts decoded.
struct CheckedBundle<'a> {
    version_override: Option<&'a str>,
    files: Vec<crate::content::TreeFile>,
    authoritative_scripts: bool,
}

impl CheckedBundle<'_> {
    fn scripts(&self) -> ImportedScripts<'_> {
        if self.authoritative_scripts {
            ImportedScripts::Bundle(&self.files)
        } else {
            ImportedScripts::CarryCurrent
        }
    }
}

/// The structural checks every import and preview makes before anything
/// is written: a supported format, an id, no scripts in a format-1 bundle,
/// and every script decoded and checked (the shared path rule, base64, size
/// limits).
fn check_bundle(bundle: &PlaybookBundle) -> Result<CheckedBundle<'_>, BundleError> {
    if !(OLDEST_SUPPORTED_SCHEMA..=BUNDLE_SCHEMA).contains(&bundle.apb_bundle) {
        return Err(BundleError::Format(format!(
            "unsupported bundle schema {} (supported: {OLDEST_SUPPORTED_SCHEMA} to {BUNDLE_SCHEMA})",
            bundle.apb_bundle
        )));
    }
    if bundle.id.trim().is_empty() {
        return Err(BundleError::Format("bundle has empty id".to_string()));
    }
    if bundle.apb_bundle < 2 && !bundle.scripts.is_empty() {
        return Err(BundleError::Format(
            "a format-1 bundle cannot carry scripts".to_string(),
        ));
    }
    let version_override = if bundle.version.trim().is_empty() {
        None
    } else {
        Some(bundle.version.as_str())
    };
    Ok(CheckedBundle {
        version_override,
        files: scripts::import_scripts(&bundle.scripts)?,
        authoritative_scripts: bundle.apb_bundle >= 2,
    })
}
