//! A playbook version as one portable file (`apb export` / `apb import`):
//! the raw YAML, the editor layout and, since format 2, the version's
//! scripts ([`scripts`]).

mod scripts;

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::registry::{Registry, RegistryError};
use crate::versioning::{
    ImportedScripts, Saved, VersioningError, playbooks_dir, save_imported, save_layout,
};

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
}

/// A single portable playbook file: the raw playbook.yaml text plus the
/// editor layout (ui.xyflow) and, since format 2, the regular files under the
/// version's `scripts/` directory. The raw YAML is stored as a string so that
/// export-import does not lose formatting or unknown fields.
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
}

/// Assembles a bundle from the registry: playbook.yaml of the specified (or
/// current) version plus its layout and its scripts. The scripts walk covers
/// the same tree the scope digest
/// ([`crate::scope::definition_digest`]) covers, with exactly
/// [`TreeLimits::default()`]; over any limit the export fails rather than
/// writing a silently partial bundle, so an imported version digests exactly
/// like the source one.
pub fn export_bundle(
    root: &Path,
    id: &str,
    version: Option<&str>,
) -> Result<PlaybookBundle, BundleError> {
    let reg = Registry::open(root)?;
    let loaded = reg.load(id, version)?;
    let version_dir = playbooks_dir(root).join(id).join(&loaded.version);
    let scripts = scripts::export_scripts(&version_dir)?;
    Ok(PlaybookBundle {
        apb_bundle: BUNDLE_SCHEMA,
        id: id.to_string(),
        version: loaded.version,
        playbook: loaded.yaml,
        layout: loaded.layout,
        scripts,
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
pub fn import_bundle(
    root: &Path,
    bundle: &PlaybookBundle,
    make_current: bool,
) -> Result<Saved, BundleError> {
    if !(OLDEST_SUPPORTED_SCHEMA..=BUNDLE_SCHEMA).contains(&bundle.apb_bundle) {
        return Err(BundleError::Format(format!(
            "unsupported bundle schema {} (supported: {OLDEST_SUPPORTED_SCHEMA} to {BUNDLE_SCHEMA})",
            bundle.apb_bundle
        )));
    }
    if bundle.id.trim().is_empty() {
        return Err(BundleError::Format("bundle has empty id".to_string()));
    }
    let version_override = if bundle.version.trim().is_empty() {
        None
    } else {
        Some(bundle.version.as_str())
    };
    if bundle.apb_bundle < 2 && !bundle.scripts.is_empty() {
        return Err(BundleError::Format(
            "a format-1 bundle cannot carry scripts".to_string(),
        ));
    }
    // Everything below can still fail; the scripts are decoded and checked
    // BEFORE the versioning write, so a rejected bundle writes nothing.
    let files = scripts::import_scripts(&bundle.scripts)?;
    let scripts = if bundle.apb_bundle >= 2 {
        ImportedScripts::Bundle(&files)
    } else {
        ImportedScripts::CarryCurrent
    };
    let saved = save_imported(
        root,
        &bundle.id,
        &bundle.playbook,
        version_override,
        make_current,
        scripts,
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
