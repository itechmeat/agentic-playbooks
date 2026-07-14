use std::collections::HashMap;

use crate::schema::{Node, Playbook};

#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error("executed node changed: {0}")]
    ExecutedNodeChanged(String),
    #[error("executed node removed: {0}")]
    ExecutedNodeRemoved(String),
    #[error("continue-from node missing: {0}")]
    ContinueFromMissing(String),
}

/// Checks the 10.3 migration rules for a run's already-executed nodes.
pub fn validate_migration(
    base: &Playbook,
    patched: &Playbook,
    executed_node_ids: &[String],
    continue_from: &str,
) -> Result<(), MigrationError> {
    let patched_nodes: HashMap<&str, &Node> = patched
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();

    for id in executed_node_ids {
        if !patched_nodes.contains_key(id.as_str()) {
            return Err(MigrationError::ExecutedNodeRemoved(id.clone()));
        }
    }

    for id in executed_node_ids {
        let Some(base_node) = base.node(id) else {
            continue;
        };
        let patched_node = patched_nodes[id.as_str()];
        if id != continue_from && nodes_differ(base_node, patched_node) {
            return Err(MigrationError::ExecutedNodeChanged(id.clone()));
        }
    }

    if !patched_nodes.contains_key(continue_from) {
        return Err(MigrationError::ContinueFromMissing(
            continue_from.to_string(),
        ));
    }

    Ok(())
}

fn nodes_differ(base: &Node, patched: &Node) -> bool {
    match (serde_json::to_value(base), serde_json::to_value(patched)) {
        (Ok(base), Ok(patched)) => base != patched,
        _ => true,
    }
}
