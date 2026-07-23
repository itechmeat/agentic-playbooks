use std::collections::{HashMap, HashSet, VecDeque};

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
///
/// Immutability is scoped to the executed prefix the migration will NOT re-run
/// (issue #45 finding 6). A migration continues linearly from `continue_from`,
/// so every node forward-reachable from it in the patched graph is re-executed
/// on the migrated version and MAY change - the reviewer's case was a patch that
/// hardened already-executed `qa`/`fix_qa` nodes lying after the continue point,
/// which the old check wrongly rejected. Only a node in the PRESERVED prefix
/// (executed, but not forward-reachable from `continue_from`) whose prior result
/// the migrated run relies on without re-running must stay byte-identical.
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

    // The set of nodes the migration re-executes: forward-reachable from
    // `continue_from` in the patched graph (structural reachability, following
    // every edge regardless of its condition - a conditional edge could route
    // through the node on the migrated run). `continue_from` itself is included.
    // Any executed node OUTSIDE this set stays in the preserved prefix.
    let rerun = reachable_from(patched, continue_from);

    for id in executed_node_ids {
        let Some(base_node) = base.node(id) else {
            continue;
        };
        let patched_node = patched_nodes[id.as_str()];
        // A node the migration will re-run may change freely; a node in the
        // preserved prefix may not.
        if !rerun.contains(id.as_str()) && nodes_differ(base_node, patched_node) {
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

/// Nodes forward-reachable from `start` over the playbook's edges, `start`
/// included. Edges are followed unconditionally: this is the set of nodes whose
/// re-execution the migration could imply, so it must not under-count (a
/// conditional edge that only fires for some state still keeps the target
/// mutable). Mirrors the drive loop, which clears the frontier and re-drives
/// forward from `continue_from` on the patched playbook.
fn reachable_from(playbook: &Playbook, start: &str) -> HashSet<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    seen.insert(start.to_string());
    queue.push_back(start.to_string());
    while let Some(node) = queue.pop_front() {
        for edge in &playbook.edges {
            if edge.from == node && seen.insert(edge.to.clone()) {
                queue.push_back(edge.to.clone());
            }
        }
    }
    seen
}

fn nodes_differ(base: &Node, patched: &Node) -> bool {
    match (serde_json::to_value(base), serde_json::to_value(patched)) {
        (Ok(base), Ok(patched)) => base != patched,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Playbook;

    /// A linear playbook `start -> a -> b -> c -> done` whose middle nodes carry
    /// a prompt that a patch can change.
    fn linear(a_prompt: &str, b_prompt: &str, c_prompt: &str) -> Playbook {
        let yaml = format!(
            r#"schema: 2
id: mig
name: mig
version: 1.0.0
defaults: {{ profile: p }}
nodes:
  - {{ id: start, type: start }}
  - {{ id: a, type: prompt, prompt: "{a_prompt}" }}
  - {{ id: b, type: prompt, prompt: "{b_prompt}" }}
  - {{ id: c, type: prompt, prompt: "{c_prompt}" }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: a }}
  - {{ from: a, to: b }}
  - {{ from: b, to: c }}
  - {{ from: c, to: done }}
"#
        );
        Playbook::from_yaml(&yaml).unwrap()
    }

    #[test]
    fn change_to_node_after_continue_point_is_accepted() {
        // Executed a, b, c; continue from b. b and c are re-run on the migrated
        // version, so hardening their prompts is allowed (issue #45 finding 6).
        let base = linear("a", "b", "c");
        let patched = linear("a", "b hardened", "c hardened");
        let executed = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert!(validate_migration(&base, &patched, &executed, "b").is_ok());
    }

    #[test]
    fn change_to_preserved_prefix_node_is_rejected() {
        // Continue from c; a lies in the preserved prefix (not re-run), so
        // changing it is still rejected with the existing error.
        let base = linear("a", "b", "c");
        let patched = linear("a changed", "b", "c");
        let executed = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let err = validate_migration(&base, &patched, &executed, "c").unwrap_err();
        assert!(matches!(err, MigrationError::ExecutedNodeChanged(id) if id == "a"));
    }

    #[test]
    fn continue_from_node_itself_may_change() {
        let base = linear("a", "b", "c");
        let patched = linear("a", "b hardened", "c");
        let executed = vec!["a".to_string(), "b".to_string()];
        assert!(validate_migration(&base, &patched, &executed, "b").is_ok());
    }

    #[test]
    fn removed_executed_node_is_rejected() {
        let base = linear("a", "b", "c");
        let mut patched = linear("a", "b", "c");
        patched.nodes.retain(|n| n.id != "b");
        patched.edges.retain(|e| e.from != "b" && e.to != "b");
        let executed = vec!["a".to_string(), "b".to_string()];
        let err = validate_migration(&base, &patched, &executed, "a").unwrap_err();
        assert!(matches!(err, MigrationError::ExecutedNodeRemoved(id) if id == "b"));
    }

    #[test]
    fn preserved_prefix_change_off_the_forward_path_is_rejected() {
        // A diamond: start -> a -> {b, c} -> d. Continue from c; b is executed
        // but not forward-reachable from c, so it stays immutable.
        let yaml = |b_prompt: &str| {
            format!(
                r#"schema: 2
id: mig
name: mig
version: 1.0.0
defaults: {{ profile: p }}
nodes:
  - {{ id: start, type: start }}
  - {{ id: a, type: prompt, prompt: "a" }}
  - {{ id: b, type: prompt, prompt: "{b_prompt}" }}
  - {{ id: c, type: prompt, prompt: "c" }}
  - {{ id: d, type: finish, outcome: success }}
edges:
  - {{ from: start, to: a }}
  - {{ from: a, to: b }}
  - {{ from: a, to: c }}
  - {{ from: b, to: d }}
  - {{ from: c, to: d }}
"#
            )
        };
        let base = Playbook::from_yaml(&yaml("b")).unwrap();
        let patched = Playbook::from_yaml(&yaml("b changed")).unwrap();
        let executed = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let err = validate_migration(&base, &patched, &executed, "c").unwrap_err();
        assert!(matches!(err, MigrationError::ExecutedNodeChanged(id) if id == "b"));
    }
}
