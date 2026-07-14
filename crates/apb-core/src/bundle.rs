use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::registry::{Registry, RegistryError};
use crate::versioning::{VersioningError, create_version, save_layout};

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
}

/// A single portable playbook file: the raw playbook.yaml text plus the
/// editor layout (ui.xyflow). The raw YAML is stored as a string so that
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
}

pub const BUNDLE_SCHEMA: u32 = 1;

impl PlaybookBundle {
    pub fn to_json(&self) -> Result<String, BundleError> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn from_json(s: &str) -> Result<Self, BundleError> {
        Ok(serde_json::from_str(s)?)
    }
}

/// Assembles a bundle from the registry: playbook.yaml of the specified (or
/// current) version plus its layout.
pub fn export_bundle(
    root: &Path,
    id: &str,
    version: Option<&str>,
) -> Result<PlaybookBundle, BundleError> {
    let reg = Registry::open(root)?;
    let loaded = reg.load(id, version)?;
    Ok(PlaybookBundle {
        apb_bundle: BUNDLE_SCHEMA,
        id: id.to_string(),
        version: loaded.version,
        playbook: loaded.yaml,
        layout: loaded.layout,
    })
}

/// Imports a bundle into a project: creates a NEW playbook version under its
/// id following the project's versioning scheme (does not force the version
/// from the bundle, to avoid collisions), validates it, and, if present, saves
/// the layout under the assigned version. Returns the assigned version.
pub fn import_bundle(
    root: &Path,
    bundle: &PlaybookBundle,
    make_current: bool,
) -> Result<String, BundleError> {
    if bundle.apb_bundle != BUNDLE_SCHEMA {
        return Err(BundleError::Format(format!(
            "unsupported bundle schema {} (expected {BUNDLE_SCHEMA})",
            bundle.apb_bundle
        )));
    }
    if bundle.id.trim().is_empty() {
        return Err(BundleError::Format("bundle has empty id".to_string()));
    }
    let assigned = create_version(root, &bundle.id, &bundle.playbook, None, make_current)?;
    if let Some(layout) = &bundle.layout {
        let layout_yaml =
            serde_yaml_ng::to_string(layout).map_err(|e| BundleError::Yaml(e.to_string()))?;
        save_layout(root, &bundle.id, &assigned, &layout_yaml)?;
    }
    Ok(assigned)
}
