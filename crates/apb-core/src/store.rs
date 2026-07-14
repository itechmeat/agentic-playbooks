//! Playbook resolver: turns a `PlaybookRef` into a concrete definition plus
//! an execution location (spec 3). The key separation: where the definition
//! LIVES (`definition_parent`) and where it EXECUTES (`execution_root`) are
//! different axes. A global playbook lives in `<config_dir>/playbooks/`, but
//! executes in the caller's project root.

use std::path::{Path, PathBuf};

use crate::registry::{Registry, RegistryError};
use crate::scope::{Origin, PlaybookRef, digest_str};

/// Result of resolution: everything the engine needs to run a playbook
/// without knowing about scopes.
#[derive(Debug, Clone)]
pub struct ResolvedPlaybook {
    /// Directory containing `playbooks/` with the definition (a project's
    /// `.apb` or the global config dir).
    pub definition_parent: PathBuf,
    /// Project root in which the run executes and history is written.
    pub execution_root: PathBuf,
    pub id: String,
    pub version: String,
    /// Content fingerprint of the definition (`sha256:...`); trust binding.
    pub digest: String,
    /// Origin label for run provenance: `"global"` or `"project"`.
    pub origin_label: &'static str,
}

/// Directory of the global store: `<config_dir>`, inside which `playbooks/`
/// lives. `None` if the config directory does not resolve (no-config
/// environment).
pub fn global_playbooks_parent() -> Option<PathBuf> {
    crate::config::config_dir()
}

/// Resolves a `PlaybookRef` into a definition + an execution location.
/// `execution_root` is always `project_root` (a global playbook executes in
/// the current project). For `Origin::Project { workspace_id: Some(_) }` the
/// path of another workspace is supplied by the caller (apb-mcp via the
/// registry); here `Project` means `project_root`.
pub fn resolve(project_root: &Path, wref: &PlaybookRef) -> Result<ResolvedPlaybook, RegistryError> {
    let (definition_parent, origin_label) = match &wref.origin {
        Origin::Global => {
            let parent = global_playbooks_parent()
                .ok_or_else(|| RegistryError::NotFound("global config dir".into()))?;
            (parent, "global")
        }
        Origin::Project { .. } => (project_root.join(".apb"), "project"),
    };
    let reg = Registry::open_dir(&definition_parent)?;
    let loaded = reg.load(&wref.id, wref.version.as_deref())?;
    Ok(ResolvedPlaybook {
        definition_parent,
        execution_root: project_root.to_path_buf(),
        id: wref.id.clone(),
        version: loaded.version.clone(),
        digest: digest_str(&loaded.yaml),
        origin_label,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::{Origin, PlaybookRef};

    const MINI: &str = "schema: 1\nid: mini\nname: Mini\nversion: 1.0.0\nnodes:\n  - id: s\n    type: start\nedges: []\n";

    fn seed(parent: &Path, id: &str) {
        let vdir = parent.join("playbooks").join(id).join("1.0.0");
        std::fs::create_dir_all(&vdir).unwrap();
        let yaml = MINI.replace("id: mini", &format!("id: {id}"));
        std::fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
        std::fs::write(parent.join("playbooks").join(id).join("current"), "1.0.0").unwrap();
    }

    #[test]
    fn global_resolves_with_project_execution_root() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        // The lock serializes env mutation against other env tests in the crate.
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        seed(cfg.path(), "mini");

        let wref = PlaybookRef {
            origin: Origin::Global,
            id: "mini".into(),
            version: None,
        };
        let r = resolve(proj.path(), &wref).unwrap();
        assert_eq!(r.origin_label, "global");
        assert_eq!(r.execution_root, proj.path());
        assert_eq!(r.definition_parent, cfg.path());
        assert!(r.digest.starts_with("sha256:"));
        unsafe {
            std::env::remove_var("APB_CONFIG_DIR");
        }
    }

    #[test]
    fn project_resolves_to_dot_apb() {
        let proj = tempfile::tempdir().unwrap();
        seed(&proj.path().join(".apb"), "mini");
        let wref = PlaybookRef {
            origin: Origin::Project { workspace_id: None },
            id: "mini".into(),
            version: None,
        };
        let r = resolve(proj.path(), &wref).unwrap();
        assert_eq!(r.origin_label, "project");
        assert_eq!(r.definition_parent, proj.path().join(".apb"));
    }
}
