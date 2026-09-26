//! Playbook definition tools: create, update, delete, read, list, validate,
//! and the compact catalog the host agent matches a request against.

use std::path::Path;

use super::{ToolError, open};
use apb_core::profile::QualifiedProfileRef;
use apb_core::registry::{LoadedPlaybook, is_safe_segment};
use apb_core::schema::NodeKind;
use apb_core::validate::{Severity, ValidationContext, validate};
use apb_core::versioning::{delete_playbook, list_trash, restore_from_trash, save_definition};
use serde_json::{Value, json};

/// Creates a new playbook or a new minor version of an existing one, through
/// the one save path (`save_definition`), which also approves the saved digest.
pub fn playbook_create(root: &Path, id: &str, yaml: &str) -> Result<Value, ToolError> {
    let saved = save_definition(root, id, yaml, None, true)?;
    Ok(saved.answer(id))
}

/// Updates an existing playbook (a new minor version, or none when the
/// definition did not change). If the id does not exist - NotFound.
pub fn playbook_update(root: &Path, id: &str, yaml: &str) -> Result<Value, ToolError> {
    if !is_safe_segment(id) {
        return Err(ToolError::NotFound(id.to_string()));
    }
    let dir = root.join(".apb/playbooks").join(id);
    if !dir.is_dir() {
        return Err(ToolError::NotFound(id.to_string()));
    }
    let saved = save_definition(root, id, yaml, None, true)?;
    Ok(saved.answer(id))
}

/// Soft-deletes a playbook into trash.
pub fn playbook_delete(root: &Path, id: &str) -> Result<Value, ToolError> {
    let trashed = delete_playbook(root, id, apb_core::clock::now_ms())?;
    Ok(json!({ "trashed": trashed.to_string_lossy() }))
}

/// The project's deleted playbooks, newest deletion first (the same listing
/// as the dashboard's Trash view and `apb trash list`).
pub fn playbook_trash_list(root: &Path) -> Result<Value, ToolError> {
    let entries = list_trash(root)?;
    serde_json::to_value(entries).map_err(|e| ToolError::Engine(e.to_string()))
}

/// Restores a trash entry (or a playbook id's latest deletion) through the one
/// core path; a playbook that exists again under the id is a `Conflict`.
pub fn playbook_trash_restore(root: &Path, name: &str) -> Result<Value, ToolError> {
    let restored = restore_from_trash(root, name)?;
    serde_json::to_value(restored).map_err(|e| ToolError::Engine(e.to_string()))
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

/// Tier 2 (spec 2026-08-15): the guided-interview guide. Pulled only when
/// the user wants a playbook built from a process they describe.
pub fn playbook_interview() -> Result<Value, ToolError> {
    Ok(json!({ "guide": include_str!("../../../../docs/HOWTO-interview.md") }))
}

/// Detail level for [`playbook_get`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailMode {
    /// Compact interface view: top-level identity, params (name/type/label),
    /// the default profile, node entries (id/type/title/declared profile
    /// ref), edge structure, and the supervisor interface. Excludes every
    /// node prompt body. The MCP default.
    Summary,
    /// Full authoring payload: yaml + full playbook + layout, byte-identical
    /// to the pre-summary behavior.
    Full,
}

impl DetailMode {
    /// Parse the MCP `detail` argument. `None` or `"summary"` -> [`Self::Summary`],
    /// `"full"` -> [`Self::Full`]. Any other value falls back to [`Self::Summary`]:
    /// a read-only get must not break discovery on a typo.
    pub fn from_arg(s: Option<&str>) -> Self {
        match s {
            Some("full") => DetailMode::Full,
            _ => DetailMode::Summary,
        }
    }
}

pub fn playbook_get(
    root: &Path,
    id: &str,
    version: Option<&str>,
    detail: DetailMode,
) -> Result<Value, ToolError> {
    let reg = open(root)?;
    let loaded = reg.load(id, version)?;
    match detail {
        DetailMode::Full => Ok(json!({
            "id": id,
            "version": loaded.version,
            "yaml": loaded.yaml,
            "playbook": loaded.playbook,
            "layout": loaded.layout,
        })),
        DetailMode::Summary => Ok(playbook_summary(id, &loaded)),
    }
}

/// Builds the compact summary of a playbook: its interface without any node
/// prompt body. The summary carries enough to match, route, and reason about
/// a playbook (identity, params, node graph, declared profile bindings,
/// supervisor interface) while staying small enough for an MCP host to inject.
fn playbook_summary(id: &str, loaded: &LoadedPlaybook) -> Value {
    let pb = &loaded.playbook;

    let params: Vec<Value> = pb
        .params
        .iter()
        .map(|p| {
            let mut o = json!({ "name": p.name, "type": p.kind });
            if let Some(label) = &p.label {
                o["label"] = json!(label);
            }
            o
        })
        .collect();

    let mut defaults = json!({});
    if let Some(profile) = &pb.defaults.profile {
        defaults["profile"] = ref_value(profile);
    }
    if let Some(retries) = pb.defaults.max_retries {
        defaults["max_retries"] = json!(retries);
    }
    if let Some(timeout) = pb.defaults.timeout_seconds {
        defaults["timeout_seconds"] = json!(timeout);
    }

    let nodes: Vec<Value> = pb
        .nodes
        .iter()
        .map(|n| {
            let mut o = json!({
                "id": n.id,
                "type": n.kind.type_str(),
            });
            if let Some(title) = &n.title {
                o["title"] = json!(title);
            }
            if let Some(profile) = declared_profile(&n.kind) {
                o["profile"] = ref_value(profile);
            }
            o
        })
        .collect();

    let edges: Vec<Value> = pb
        .edges
        .iter()
        .map(|e| {
            let mut o = json!({
                "from": e.from,
                "to": e.to,
                "has_condition": e.condition.is_some(),
                "fallback": e.fallback,
            });
            if let Some(join) = &e.join {
                o["join"] = json!(join);
            }
            if let Some(max) = e.max_traversals {
                o["max_traversals"] = json!(max);
            }
            o
        })
        .collect();

    let supervisor = match &pb.supervisor {
        Some(s) => {
            let mut o = json!({ "has_policy": s.policy.is_some() });
            if let Some(profile) = &s.profile {
                o["profile"] = ref_value(profile);
            }
            o
        }
        None => json!(null),
    };

    let mut summary = json!({
        "detail": "summary",
        "id": id,
        "name": pb.name,
        "description": pb.description,
        "version": loaded.version,
        "schema": pb.schema,
        "params": params,
        "defaults": defaults,
        "nodes": nodes,
        "edges": edges,
        "supervisor": supervisor,
    });
    // The goal is the contract of the run (spec 2026-08-15): an adopting
    // agent must see it here, not only in `detail: "full"` or the run
    // manifest. Additive, present only when the playbook declares one.
    if let Some(goal) = &pb.goal {
        summary["goal"] = serde_json::to_value(goal).unwrap_or(Value::Null);
    }
    summary
}

/// The profile a node declares on itself (not resolved against `defaults`):
/// `agent_task.profile`, or `finish.profile` when a prompt makes it effective.
/// `None` for every other kind, so the summary never leaks a dead binding.
fn declared_profile(kind: &NodeKind) -> Option<&QualifiedProfileRef> {
    match kind {
        NodeKind::AgentTask { profile, .. } => profile.as_ref(),
        NodeKind::Finish {
            profile,
            prompt: Some(_),
            ..
        } => profile.as_ref(),
        _ => None,
    }
}

/// Serializes a [`QualifiedProfileRef`] as `{ name, scope }`, falling back to
/// null only if serialization itself fails (it cannot for this type).
fn ref_value(r: &QualifiedProfileRef) -> Value {
    serde_json::to_value(r).unwrap_or(Value::Null)
}

pub fn playbook_validate(root: &Path, id: &str) -> Result<Value, ToolError> {
    let reg = open(root)?;
    let loaded = reg.load(id, None)?;
    let ctx =
        ValidationContext::for_registry(&reg, apb_core::profile_store::PlaybookOrigin::Project);
    let report = validate(&loaded.playbook, &ctx);
    let issues: Vec<Value> = report.issues.iter().map(|i| json!({
        "code": i.code,
        "severity": match i.severity { Severity::Error => "error", Severity::Warning => "warning" },
        "message": i.message,
        "node": i.node,
    })).collect();
    Ok(json!({ "valid": report.is_valid(), "issues": issues }))
}
