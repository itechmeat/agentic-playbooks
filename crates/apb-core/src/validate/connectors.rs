//! Connector rules: the grants a playbook declares, and the function
//! allowlists inside them.

use super::*;

/// V23 (error): a connector binding name, an `accounts` entry, or a
/// `functions` list entry fails its identifier format check. Binding names
/// and account entries are connector/account folder names - hyphen slugs
/// (`crate::profile::validate_profile_name`); `functions` list entries are
/// the connector's snake_case function names
/// (`crate::connector::validate_snake_name`). V24 (error): a node binds the
/// same connector name more than once. V25 (error): an `accounts` or
/// `functions` list entry that is empty or repeated within one binding. V26
/// (error): `max_calls` is 0 (a binding that can never be called).
pub(crate) fn check_connectors(playbook: &Playbook, r: &mut ValidationReport) {
    for n in &playbook.nodes {
        let mut seen_connectors = HashSet::new();
        for b in n.kind.connector_bindings() {
            if !seen_connectors.insert(b.name.as_str()) {
                r.error(
                    "V24",
                    Some(&n.id),
                    format!(
                        "node `{}` binds connector `{}` more than once",
                        n.id, b.name
                    ),
                );
            }
            if let Err(msg) = crate::profile::validate_profile_name(&b.name) {
                r.error(
                    "V23",
                    Some(&n.id),
                    format!(
                        "node `{}` connector `{}` has an invalid name: {msg}",
                        n.id, b.name
                    ),
                );
            }
            if let Some(accounts) = &b.accounts {
                check_connector_list(&n.id, &b.name, "accounts", accounts, r, |item| {
                    crate::profile::validate_profile_name(item)
                });
            }
            if let FunctionsAllow::List(names) = &b.functions {
                check_connector_list(&n.id, &b.name, "functions", names, r, |item| {
                    crate::connector::validate_snake_name(item)
                });
            }
            if b.max_calls == Some(0) {
                r.error(
                    "V26",
                    Some(&n.id),
                    format!("node `{}` connector `{}` has max_calls 0", n.id, b.name),
                );
            }
        }
    }
}

/// Checks one `accounts`/`functions` list of a connector binding: every
/// entry must be non-empty, unique within the list (V25), and pass its
/// identifier format check (V23). `field` names the offending list in the
/// message (`accounts` or `functions`).
pub(crate) fn check_connector_list(
    node_id: &str,
    connector: &str,
    field: &str,
    items: &[String],
    r: &mut ValidationReport,
    validate: impl Fn(&str) -> Result<(), String>,
) {
    let mut seen = HashSet::new();
    for item in items {
        if item.is_empty() {
            r.error(
                "V25",
                Some(node_id),
                format!("node `{node_id}` connector `{connector}` has an empty {field} entry"),
            );
            continue;
        }
        if !seen.insert(item.as_str()) {
            r.error(
                "V25",
                Some(node_id),
                format!(
                    "node `{node_id}` connector `{connector}` has duplicate {field} entry `{item}`"
                ),
            );
        }
        if let Err(msg) = validate(item) {
            r.error(
                "V23",
                Some(node_id),
                format!(
                    "node `{node_id}` connector `{connector}` {field} entry `{item}` is invalid: {msg}"
                ),
            );
        }
    }
}
