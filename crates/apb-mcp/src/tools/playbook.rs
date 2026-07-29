//! Playbook definition tools: create, update, delete, read, list, validate,
//! and the compact catalog the host agent matches a request against.

use std::path::Path;

use super::{ToolError, open};
use apb_core::registry::is_safe_segment;
use apb_core::validate::{Severity, ValidationContext, validate};
use apb_core::versioning::{create_version, delete_playbook};
use serde_json::{Value, json};

/// Creates a new playbook or a new minor version of an existing one.
pub fn playbook_create(root: &Path, id: &str, yaml: &str) -> Result<Value, ToolError> {
    let version = create_version(root, id, yaml, None, true)?;
    Ok(json!({ "id": id, "version": version }))
}

/// Updates an existing playbook (a new minor version). If the id does not exist - NotFound.
pub fn playbook_update(root: &Path, id: &str, yaml: &str) -> Result<Value, ToolError> {
    if !is_safe_segment(id) {
        return Err(ToolError::NotFound(id.to_string()));
    }
    let dir = root.join(".apb/playbooks").join(id);
    if !dir.is_dir() {
        return Err(ToolError::NotFound(id.to_string()));
    }
    let version = create_version(root, id, yaml, None, true)?;
    Ok(json!({ "id": id, "version": version }))
}

/// Approves the digest of a version just created locally (spec 3.1): creation
/// through the tool/CLI is a local user action, hence trusted. Best-effort:
/// a failure is not critical (the playbook will simply stay untrusted until trial/acknowledge).
/// Project scope (`root/.apb`); global creation is approved on its own path.
pub fn approve_local(root: &Path, id: &str, version: &str) {
    let yaml_path = root
        .join(".apb/playbooks")
        .join(id)
        .join(version)
        .join("playbook.yaml");
    if let Ok(yaml) = std::fs::read_to_string(&yaml_path) {
        let digest = apb_core::scope::digest_str(&yaml);
        let mut trust = apb_core::trust::TrustStore::load();
        let _ = trust.approve(&digest, id, apb_core::trust::OriginKind::LocallyApproved);
    }
}

/// Soft-deletes a playbook into trash.
pub fn playbook_delete(root: &Path, id: &str) -> Result<Value, ToolError> {
    let trashed = delete_playbook(root, id, apb_core::clock::now_ms())?;
    Ok(json!({ "trashed": trashed.to_string_lossy() }))
}

pub fn playbook_list(root: &Path) -> Result<Value, ToolError> {
    let reg = open(root)?;
    let list = reg.list()?;
    serde_json::to_value(list).map_err(|e| ToolError::Engine(e.to_string()))
}

/// The compact structural playbook catalog (tier 1, spec 4). Project scope
/// plus global, with trust-aware shadowing and effective effects. Does not break
/// `playbook_list` - this is a separate surface.
pub fn playbook_catalog(
    root: &Path,
    workspace_id: Option<&str>,
    revision: Option<&str>,
    limit: Option<usize>,
) -> Result<Value, ToolError> {
    let view = apb_core::dismiss::active(root);
    Ok(crate::catalog::build(
        root,
        workspace_id,
        revision,
        limit,
        view.records,
        view.diagnostics,
    ))
}

/// Tier 2 (spec 4): playbook authoring details. Pulled only when
/// creating/reworking one; it does not enter a normal session.
pub fn playbook_howto() -> Result<Value, ToolError> {
    Ok(json!({ "howto": include_str!("../../../../docs/HOWTO-authoring.md") }))
}

pub fn playbook_get(root: &Path, id: &str, version: Option<&str>) -> Result<Value, ToolError> {
    let reg = open(root)?;
    let loaded = reg.load(id, version)?;
    Ok(json!({
        "id": id,
        "version": loaded.version,
        "yaml": loaded.yaml,
        "playbook": loaded.playbook,
        "layout": loaded.layout,
    }))
}

pub fn playbook_validate(root: &Path, id: &str) -> Result<Value, ToolError> {
    let reg = open(root)?;
    let loaded = reg.load(id, None)?;
    let ctx = ValidationContext {
        profiles: reg.profiles(),
        ..Default::default()
    };
    let report = validate(&loaded.playbook, &ctx);
    let issues: Vec<Value> = report.issues.iter().map(|i| json!({
        "code": i.code,
        "severity": match i.severity { Severity::Error => "error", Severity::Warning => "warning" },
        "message": i.message,
        "node": i.node,
    })).collect();
    Ok(json!({ "valid": report.is_valid(), "issues": issues }))
}
