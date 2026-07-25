//! Small read-only lookups that belong to no single resource: the project
//! list and the connector inventory.

use std::path::Path;

use super::ToolError;
use serde_json::{Value, json};

/// The registry of the user's workspaces (spec 6): current, global, and other projects.
pub fn projects_list() -> Result<Value, ToolError> {
    let entries = apb_core::projects::list_active();
    serde_json::to_value(entries).map_err(|e| ToolError::Engine(e.to_string()))
}

/// Read-only listing of installed connectors for an authoring agent (spec 12):
/// each installed connector with its version, storefront summary, connector
/// trust state, the function names it exposes (with description and the
/// read_only / deprecated marks), and the configured account names - enough to
/// write a node `connectors` binding. Never returns account field values or env
/// values (a secret-marked field holds an `{{env.VAR}}` reference, which we
/// still do not surface here: names only).
pub fn connectors_list(root: &Path) -> Result<Value, ToolError> {
    let trust = apb_core::trust::TrustStore::load();
    let approved_ids = trust.approved_record_ids(apb_core::trust::Kind::Connector);
    let mut out = Vec::new();
    for summary in apb_core::connector::store::list() {
        let Ok(loaded) = apb_core::connector::store::load(&summary.name) else {
            // A connector that vanished or stopped parsing between listing and
            // load is simply skipped, matching `store::list`'s own tolerance.
            continue;
        };
        let functions: Vec<Value> = loaded
            .doc
            .functions
            .iter()
            .map(|f| {
                json!({
                    "name": f.name,
                    "description": f.description,
                    "read_only": f.read_only,
                    "deprecated": f.deprecated,
                })
            })
            .collect();
        // Account NAMES only (never fields/env). Best-effort: a broken account
        // config yields an empty account list, not a failed listing.
        let accounts: Vec<String> = apb_core::connector::config::load_merged(root, &summary.name)
            .map(|accts| accts.into_iter().map(|a| a.name).collect())
            .unwrap_or_default();
        let trust_state = if trust.is_approved(&loaded.digest) {
            "approved"
        } else if approved_ids.iter().any(|id| id == &summary.name) {
            "changed"
        } else {
            "unapproved"
        };
        out.push(json!({
            "name": summary.name,
            "version": summary.version,
            "summary": summary.meta.summary,
            "trust": trust_state,
            "functions": functions,
            "accounts": accounts,
        }));
    }
    Ok(json!({ "connectors": out }))
}
