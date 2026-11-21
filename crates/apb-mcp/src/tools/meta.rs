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
///
/// An MCP connector adds its `mcp` block (`transport`, `protocol`) and, per
/// mcp function, its `mcp_tool` and the `approved_accounts` whose pin record
/// covers that tool for this definition and account (`ServerPin::compare`,
/// the program not read): no digest, no server started. A pin store that
/// does not read adds a top-level `pins_error` (fixed words, never the
/// reader's diagnostic) and reads as no pins.
pub fn connectors_list(root: &Path) -> Result<Value, ToolError> {
    let trust = apb_core::trust::TrustStore::load();
    let (pins, pins_error) = match apb_core::connector::mcp::pins::PinStore::load() {
        Ok(store) => (store, None),
        Err(_) => (
            Default::default(),
            Some(apb_core::connector::mcp::pins::PIN_STORE_UNREADABLE),
        ),
    };
    let mut out = Vec::new();
    for summary in apb_core::connector::store::list() {
        let Ok(loaded) = apb_core::connector::store::load(&summary.name) else {
            // A connector that vanished or stopped parsing between listing and
            // load is simply skipped, matching `store::list`'s own tolerance.
            continue;
        };
        // Account NAMES (never field or env values). Best-effort: a broken
        // account config yields an empty account list, not a failed listing.
        // Plus, per account, each secret read from a command with its command
        // line: that is what approving the account authorizes apb to run, so
        // an agent asking the user to approve must be able to show it.
        let merged =
            apb_core::connector::config::load_merged(root, &summary.name).unwrap_or_default();
        let functions: Vec<Value> = loaded
            .doc
            .functions
            .iter()
            .map(|f| {
                let mut function = json!({
                    "name": f.name,
                    "description": f.description,
                    "read_only": f.read_only,
                    "deprecated": f.deprecated,
                });
                if let Some(m) = &f.mcp {
                    function["mcp_tool"] = json!(m.tool);
                    function["approved_accounts"] = json!(approved_accounts(
                        &pins,
                        &summary.name,
                        &loaded,
                        &merged,
                        &m.tool
                    ));
                }
                function
            })
            .collect();
        let accounts: Vec<String> = merged.iter().map(|a| a.name.clone()).collect();
        let account_commands: serde_json::Map<String, Value> = merged
            .iter()
            .filter_map(|a| {
                let cmd = apb_core::connector::config::cmd_refs(&loaded.doc, a);
                (!cmd.is_empty()).then(|| (a.name.clone(), json!(cmd)))
            })
            .collect();
        let trust_state = trust
            .status(
                &loaded.digest,
                &summary.name,
                apb_core::trust::Kind::Connector,
            )
            .as_str();
        let mut connector = json!({
            "name": summary.name,
            "version": summary.version,
            "summary": summary.meta.summary,
            "trust": trust_state,
            "update_available": apb_core::connector::install::embedded_update(&summary.name),
            "functions": functions,
            "accounts": accounts,
            "account_commands": account_commands,
        });
        if let Some(spec) = &loaded.doc.mcp {
            connector["mcp"] = json!({
                "transport": spec.transport.as_str(),
                "protocol": spec.protocol,
            });
        }
        out.push(connector);
    }
    let mut answer = json!({ "connectors": out });
    if let Some(e) = pins_error {
        answer["pins_error"] = json!(e);
    }
    Ok(answer)
}

/// The names of the accounts whose pin record passes `ServerPin::compare`
/// for `tool` against this definition and the account as configured, the
/// program not read and no tool digest compared.
fn approved_accounts(
    pins: &apb_core::connector::mcp::pins::PinStore,
    name: &str,
    loaded: &apb_core::connector::store::LoadedConnector,
    accounts: &[apb_core::connector::config::Account],
    tool: &str,
) -> Vec<String> {
    use apb_core::connector::mcp::pins::{LiveBinding, ProgramFact};
    accounts
        .iter()
        .filter(|a| {
            pins.server(name, &a.name).is_some_and(|record| {
                record
                    .compare(&LiveBinding {
                        connector_digest: &loaded.digest,
                        account_digest: &apb_core::connector::config::account_digest(a),
                        program: ProgramFact::NotRead,
                        tool,
                        tool_digest: None,
                    })
                    .is_ok()
            })
        })
        .map(|a| a.name.clone())
        .collect()
}

fn parse_kind(kind: Option<&str>) -> Result<Option<apb_core::trust::Kind>, ToolError> {
    match kind {
        None => Ok(None),
        Some(k) => apb_core::trust::Kind::parse(k).map(Some).ok_or_else(|| {
            ToolError::Engine(format!(
                "unknown kind `{k}`; use playbook, profile_bundle, connector or connector_account"
            ))
        }),
    }
}

/// Every approval in the trust store (optionally of one kind): digest, id,
/// kind, origin and approval time. The same listing as `apb trust list` and
/// the dashboard's Trust view.
pub fn trust_list(kind: Option<&str>) -> Result<Value, ToolError> {
    let kind = parse_kind(kind)?;
    let entries: Vec<apb_core::trust::TrustEntry> = apb_core::trust::TrustStore::load()
        .entries()
        .into_iter()
        .filter(|e| kind.is_none_or(|k| k == e.kind))
        .collect();
    Ok(json!({ "approvals": entries }))
}

/// Revokes the approvals `target` names (a digest, or every approval under an
/// id, optionally of one kind) through the one core path, and returns what
/// was removed. Revoking only lowers trust.
pub fn trust_revoke(target: &str, kind: Option<&str>) -> Result<Value, ToolError> {
    let selector = apb_core::trust::TrustSelector::parse(target, parse_kind(kind)?);
    let removed = apb_core::trust::TrustStore::load()
        .revoke(&selector)
        .map_err(|e| ToolError::Engine(e.to_string()))?;
    Ok(json!({ "revoked": removed }))
}
