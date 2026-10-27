//! What an import would do, computed before anything is written: whether
//! the playbook is new or unchanged, the definition diff against the
//! current version, which scripts it adds, removes or changes, the
//! validation errors that would refuse it, and whether its scripts will
//! need an approval by digest before the version can run.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Serialize;

use super::{BundleError, BundleOrigin, ImportedScripts, PlaybookBundle, check_bundle};
use crate::content::{TreeFile, TreeLimits, read_tree_files};
use crate::registry::{is_safe_segment, list_versions};
use crate::schema::Playbook;
use crate::validate::{Issue, Severity};
use crate::versioning::{
    VersionDiff, VersioningError, diff_definitions, inherited_import, playbooks_dir,
    read_current_pointer, unchanged_import, validation_errors,
};

/// How the bundle's scripts compare with the current version's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ScriptsDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
    pub unchanged: usize,
}

/// The preview of importing one bundle into a project.
#[derive(Debug, Clone, Serialize)]
pub struct ImportPreview {
    pub id: String,
    /// The version the bundle declares (empty: assigned on import).
    pub version: String,
    /// No playbook with this id exists in the project yet.
    pub new_playbook: bool,
    /// The bundle equals the current version and names no other version:
    /// importing writes nothing.
    pub unchanged: bool,
    /// The declared version already exists, so the import would be refused.
    pub version_taken: bool,
    /// What an acceptance of this exact file must name.
    pub bundle_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<BundleOrigin>,
    /// The definition diff against the current version; `None` for a new
    /// playbook, or when either YAML does not parse.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<VersionDiff>,
    pub scripts: ScriptsDiff,
    /// Validation errors that would refuse the import.
    pub errors: Vec<Issue>,
    /// The import writes a version with scripts, which runs only after an
    /// approval naming its digest.
    pub needs_script_approval: bool,
}

/// Previews importing `bundle` (read from `raw`) into the project at
/// `root`. Structural problems (format, id, scripts) are errors, as on
/// import; validation problems are listed in [`ImportPreview::errors`].
pub fn preview(
    root: &Path,
    bundle: &PlaybookBundle,
    raw: &[u8],
) -> Result<ImportPreview, BundleError> {
    let checked = check_bundle(bundle)?;
    let id = bundle.id.as_str();
    if !is_safe_segment(id) {
        return Err(BundleError::Format(
            "bundle id is not a safe name".to_string(),
        ));
    }
    let playbook_dir = playbooks_dir(root).join(id);
    let current = read_current_pointer(&playbook_dir);
    let unchanged = unchanged_import(
        root,
        id,
        &bundle.playbook,
        checked.version_override,
        checked.scripts(),
    )
    .is_some();
    let version_taken = !unchanged
        && checked.version_override.is_some_and(|v| {
            list_versions(&playbook_dir).is_ok_and(|have| have.iter().any(|h| h == v))
        });
    // A YAML that does not parse (the bundle's or the stored one) has no
    // structural diff; the bundle's own problem is listed in `errors`.
    let definition = match current.as_deref() {
        Some(current) => {
            let current_yaml = fs::read_to_string(playbook_dir.join(current).join("playbook.yaml"))
                .map_err(VersioningError::from)?;
            diff_definitions(&current_yaml, &bundle.playbook).ok()
        }
        None => None,
    };
    let current_scripts = match current.as_deref() {
        Some(current) => scripts_of(&playbook_dir.join(current))?,
        None => Vec::new(),
    };
    let scripts = match checked.scripts() {
        ImportedScripts::Bundle(files) => diff_scripts(&current_scripts, files),
        ImportedScripts::CarryCurrent => diff_scripts(&current_scripts, &current_scripts),
    };
    let errors = match Playbook::from_yaml(&bundle.playbook) {
        Ok(mut playbook) => {
            playbook.id = id.to_string();
            validation_errors(root, &playbook)?
        }
        Err(e) => vec![Issue {
            code: "schema",
            severity: Severity::Error,
            message: e.to_string(),
            node: None,
        }],
    };
    Ok(ImportPreview {
        id: id.to_string(),
        version: bundle.version.clone(),
        new_playbook: current.is_none(),
        unchanged,
        version_taken,
        bundle_digest: bundle.digest(Some(raw))?,
        origin: bundle.origin.clone(),
        definition,
        needs_script_approval: !unchanged
            && match checked.scripts() {
                ImportedScripts::Bundle(files) => !files.is_empty(),
                // A format-1 bundle keeps the current scripts, and with
                // them an import approval they still wait for.
                ImportedScripts::CarryCurrent => {
                    inherited_import(&playbook_dir, current.as_deref()).is_some()
                }
            },
        scripts,
        errors,
    })
}

/// The files under `version_dir/scripts`, none when it has no scripts.
fn scripts_of(version_dir: &Path) -> Result<Vec<TreeFile>, BundleError> {
    let dir = version_dir.join("scripts");
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    read_tree_files(&dir, &TreeLimits::default())
        .map(|(files, _)| files)
        .map_err(|e| BundleError::Scripts(super::ScriptsError(e.to_string())))
}

fn diff_scripts(from: &[TreeFile], to: &[TreeFile]) -> ScriptsDiff {
    fn index(files: &[TreeFile]) -> BTreeMap<&str, (&[u8], bool)> {
        files
            .iter()
            .map(|f| (f.rel.as_str(), (f.bytes.as_slice(), f.executable)))
            .collect()
    }
    let (old, new) = (index(from), index(to));
    let mut diff = ScriptsDiff::default();
    for (path, content) in &new {
        match old.get(path) {
            None => diff.added.push(path.to_string()),
            Some(before) if before != content => diff.changed.push(path.to_string()),
            Some(_) => diff.unchanged += 1,
        }
    }
    diff.removed = old
        .keys()
        .filter(|p| !new.contains_key(*p))
        .map(|p| p.to_string())
        .collect();
    diff
}
