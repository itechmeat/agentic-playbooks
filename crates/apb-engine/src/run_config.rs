use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use apb_core::fsutil::atomic_write;
use apb_core::profile::ProfileScope;
use serde::{Deserialize, Serialize};

use crate::error::EngineError;

/// Anti-TOCTOU pin of one sub-playbook child, verified in the parent's policy
/// gate and handed to the engine verbatim (spec C). The engine starts the child
/// against this pinned version and rejects any digest/bundle drift. `children`
/// recursively pins the child's own sub-playbook nodes, keyed by the child's
/// node id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildExpectation {
    pub id: String,
    /// The concrete origin the gate resolved this child to (review I2). Always
    /// `Project` or `Global` - never `Auto`, which is enforced by construction:
    /// pins are built from an already-resolved `Origin` in `collect_children`,
    /// and `Origin` has no auto variant. `ProfileScope`'s snake_case serde
    /// serializes to the same lowercase `"project"`/`"global"` strings the
    /// field held as a `String`, so on-disk run configs are unchanged and old
    /// ones still deserialize.
    pub scope: ProfileScope,
    pub version: String,
    pub playbook_digest: String,
    #[serde(default)]
    pub profile_bundles: BTreeMap<String, String>,
    #[serde(default)]
    pub children: BTreeMap<String, ChildExpectation>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RunConfig {
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    #[serde(default)]
    pub instruction: Option<String>,
    /// The run expects an external background supervisor agent (the engine
    /// spawns it itself and watches its heartbeat) - see
    /// `RunOptions::supervisor_expected` and Phase 4c Task 4.
    #[serde(default)]
    pub supervisor_expected: bool,
    #[serde(default)]
    pub max_patches_per_run: Option<u32>,
    /// Threshold for the assembled context size in bytes: once exceeded, old
    /// sections are compacted by a cheap model into context_compact.md
    /// (spec 8.5). None or 0 - compaction disabled.
    #[serde(default)]
    pub context_max_bytes: Option<usize>,
    /// Model used for context compaction (a cheap one). None -> "haiku".
    #[serde(default)]
    pub context_compact_model: Option<String>,
    /// Run-level overrides (spec 11): different models/executors without
    /// creating a new version. None/empty - the playbook matches its version exactly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides: Option<apb_core::overrides::RunOverrides>,
    /// The parent run id, when this run is a Part C sub-playbook child.
    #[serde(default)]
    pub parent_run: Option<String>,
    /// Sub-playbook nesting depth (spec C). A top-level run is 0; each child is
    /// parent depth + 1. Enforced against `MAX_SUBPLAYBOOK_DEPTH`.
    #[serde(default)]
    pub depth: usize,
    /// Verified sub-playbook pins from the policy gate, keyed by this run's
    /// playbook-node id (spec C). `None` on the CLI path (no gate) -> children
    /// resolve live without a drift check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_children: Option<BTreeMap<String, ChildExpectation>>,
}

pub fn write_run_config(run_dir: &Path, cfg: &RunConfig) -> Result<(), EngineError> {
    let yaml = serde_yaml_ng::to_string(cfg).map_err(|e| EngineError::Yaml(e.to_string()))?;
    atomic_write(&run_dir.join("run.yaml"), yaml.as_bytes())?;
    Ok(())
}

pub fn read_run_config(run_dir: &Path) -> Result<RunConfig, EngineError> {
    let path = run_dir.join("run.yaml");
    if !path.is_file() {
        return Ok(RunConfig::default());
    }
    let raw = std::fs::read_to_string(&path)?;
    serde_yaml_ng::from_str(&raw).map_err(|e| EngineError::Yaml(e.to_string()))
}

pub fn snapshot_playbook(run_dir: &Path, yaml: &str) -> Result<(), EngineError> {
    atomic_write(&run_dir.join("playbook.yaml"), yaml.as_bytes())?;
    Ok(())
}

/// The version snapshot folder within the run (also holds scripts copied at start).
pub fn snapshot_dir(run_dir: &Path) -> PathBuf {
    run_dir.to_path_buf()
}

/// Recursively copies `version_dir/scripts` to `run_dir/scripts`.
/// Without this, script nodes cannot find their scripts: `run_script`
/// resolves the path relative to `run_dir`, while the sources live in the
/// playbook version folder. If the version has no `scripts` folder, we
/// simply do nothing.
pub fn copy_scripts(version_dir: &Path, run_dir: &Path) -> Result<(), EngineError> {
    let src = version_dir.join("scripts");
    if !src.is_dir() {
        return Ok(());
    }
    copy_dir_recursive(&src, &run_dir.join("scripts"))
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), EngineError> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The typed `scope` field (review I2) must serialize to the SAME lowercase
    /// strings the field used as a `String`, and an on-disk config written with
    /// those strings must still deserialize. Proves the format is unchanged.
    #[test]
    fn child_expectation_scope_on_disk_strings_are_stable() {
        let child = ChildExpectation {
            id: "child".into(),
            scope: ProfileScope::Project,
            version: "1.0.0".into(),
            playbook_digest: "sha256:abc".into(),
            profile_bundles: BTreeMap::new(),
            children: BTreeMap::new(),
        };
        let yaml = serde_yaml_ng::to_string(&child).unwrap();
        assert!(
            yaml.contains("scope: project"),
            "project scope must serialize to the bare `project` string, got:\n{yaml}"
        );

        let global = ChildExpectation {
            scope: ProfileScope::Global,
            ..child.clone()
        };
        assert!(
            serde_yaml_ng::to_string(&global)
                .unwrap()
                .contains("scope: global")
        );

        // Old configs (pre-typing) stored the same strings; they must still load.
        let legacy = "id: child\nscope: global\nversion: 1.0.0\nplaybook_digest: sha256:abc\n";
        let parsed: ChildExpectation = serde_yaml_ng::from_str(legacy).unwrap();
        assert_eq!(parsed.scope, ProfileScope::Global);

        // Full roundtrip preserves the value.
        let round: ChildExpectation = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(round, child);
    }
}
