//! Who vouched for what a playbook runs: its own digest, an import waiting
//! for its approval by digest, its profile bundles, connectors and
//! accounts, and every sub-playbook the run would start. Whether an
//! acknowledge at start covers an item is said per item: it does for
//! playbook and profile trust, never for connectors, accounts and imported
//! scripts.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::profile::QualifiedProfileRef;
use apb_core::schema::Playbook;
use apb_core::trust::TrustStore;
use serde_json::Value;

use super::report::TrustItem;
use crate::gate::trust_scan::ConnectorScan;
use crate::run_config::ChildExpectation;

fn item(
    kind: &str,
    subject: &str,
    digest: Option<&str>,
    approved: bool,
    covers: bool,
) -> TrustItem {
    TrustItem {
        kind: kind.to_string(),
        subject: subject.to_string(),
        digest: digest.map(str::to_string),
        approved,
        acknowledge_covers: covers,
    }
}

/// The playbook's own digest, and its import approval when it waits for one.
pub(super) fn playbook_items(
    playbook_dir: &Path,
    id: &str,
    version: &str,
    digest: &str,
    store: &TrustStore,
) -> Vec<TrustItem> {
    let mut out = vec![item(
        "playbook",
        id,
        Some(digest),
        store.is_approved(digest),
        true,
    )];
    if apb_core::versioning::approval::import_pending(playbook_dir, version, digest, store) {
        out.push(item("imported_scripts", id, Some(digest), false, false));
    }
    out
}

/// The profile bundles `<scope>/<name> -> digest` as trust items.
pub(super) fn bundle_items(
    bundles: &BTreeMap<String, String>,
    store: &TrustStore,
) -> Vec<TrustItem> {
    bundles
        .iter()
        .map(|(key, digest)| {
            item(
                "profile_bundle",
                key,
                Some(digest),
                store.is_approved(digest),
                true,
            )
        })
        .collect()
}

/// Drops repeated items (a profile bundle the root and a sub-playbook
/// share), keeping the first.
pub(super) fn dedup(items: &mut Vec<TrustItem>) {
    let mut seen = std::collections::HashSet::new();
    items.retain(|i| seen.insert((i.kind.clone(), i.subject.clone(), i.digest.clone())));
}

/// The connectors and accounts of a scan.
pub(super) fn connector_items(scan: &ConnectorScan) -> Vec<TrustItem> {
    let mut out: Vec<TrustItem> = scan
        .connectors
        .iter()
        .map(|(name, digest)| {
            let approved = !scan.untrusted_connectors.contains(name);
            item("connector", name, Some(digest), approved, false)
        })
        .collect();
    out.extend(scan.accounts.iter().map(|(id, digest)| {
        let approved = !scan.unapproved_accounts.contains(id);
        item("connector_account", id, Some(digest), approved, false)
    }));
    // One row per granted MCP tool nobody approved for its account
    // (`c/a/t`); only a person approves it at a terminal.
    out.extend(
        scan.unpinned_mcp_tools
            .iter()
            .map(|entry| item("mcp_tools", entry, None, false, false)),
    );
    // A pin store that does not read approves no tool: one row naming it.
    if let Some(store) = &scan.mcp_pin_store_unreadable {
        out.push(item(
            "mcp_tools",
            &format!("pin store {store} does not read"),
            None,
            false,
            false,
        ));
    }
    out
}

/// Every sub-playbook pin of the tree, with its profile bundles, depth
/// first; `prefix` is the node path of the parent.
pub(super) fn child_items(
    children: &BTreeMap<String, ChildExpectation>,
    prefix: &str,
    store: &TrustStore,
    out: &mut Vec<TrustItem>,
) {
    for (node, child) in children {
        let path = if prefix.is_empty() {
            node.clone()
        } else {
            format!("{prefix}/{node}")
        };
        let digest = &child.playbook_digest;
        out.push(item(
            "sub_playbook",
            &format!("{path} ({})", child.id),
            Some(digest),
            store.is_approved(digest),
            true,
        ));
        out.extend(bundle_items(&child.profile_bundles, store));
        child_items(&child.children, &path, store, out);
    }
}

/// The trust items a refusal of the sub-playbook walk names, when it is a
/// trust refusal (a child's connector, account or imported scripts); `None`
/// for any other refusal.
pub(super) fn refusal_items(refusal: &Value) -> Option<Vec<TrustItem>> {
    let names = |field: &str| -> Vec<String> {
        refusal[field]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    match refusal["policy"].as_str()? {
        "untrusted_connector_requires_approve" => Some(
            names("connectors")
                .iter()
                .map(|n| item("connector", n, None, false, false))
                .collect(),
        ),
        "unapproved_connector_account" => Some(
            names("accounts")
                .iter()
                .map(|n| item("connector_account", n, None, false, false))
                .collect(),
        ),
        // A pin store that does not read lists no tool: the same row
        // naming the store as the playbook's own (`connector_items`).
        "mcp_tools_unpinned" => match refusal["store_unreadable"].as_str() {
            Some(store) => Some(vec![item(
                "mcp_tools",
                &format!("pin store {store} does not read"),
                None,
                false,
                false,
            )]),
            None => Some(
                names("tools")
                    .iter()
                    .map(|n| item("mcp_tools", n, None, false, false))
                    .collect(),
            ),
        },
        "imported_scripts_require_approval" => Some(vec![item(
            "imported_scripts",
            refusal["id"].as_str().unwrap_or_default(),
            refusal["digest"].as_str(),
            false,
            false,
        )]),
        _ => None,
    }
}

/// The profile references a playbook binds for a fit (supervisor
/// included: a fit looks at every way the playbook can run).
pub(super) fn refs_of(playbook: &Playbook) -> Vec<QualifiedProfileRef> {
    crate::gate::collect_profile_refs(playbook, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A child's refusal over a pin store that does not read names the
    /// store, so fit never reports the run trusted.
    #[test]
    fn an_unreadable_pin_store_of_a_child_is_a_row() {
        let rows = refusal_items(&json!({
            "policy": "mcp_tools_unpinned",
            "tools": [],
            "store_unreadable": "/cfg/mcp-tools.json",
        }))
        .unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].kind, "mcp_tools");
        assert_eq!(
            rows[0].subject,
            "pin store /cfg/mcp-tools.json does not read"
        );
        assert!(!rows[0].approved);
        // An ordinary pin refusal keeps one row per tool.
        let rows = refusal_items(&json!({
            "policy": "mcp_tools_unpinned",
            "tools": ["tracker/work: search"],
        }))
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].subject, "tracker/work: search");
    }
}
