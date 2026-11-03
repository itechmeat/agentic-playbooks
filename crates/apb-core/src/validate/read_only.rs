//! Rules for a read-only node (`read_only: true`): V137 to V139. V140, a
//! publish step `from` a read-only node, lives with the publish rules.
//!
//! The field sits on the node, not inside a kind, so the validator is where
//! a kind that cannot honour it is refused instead of running unchecked. A
//! node-level `read_only` is not `isolation.read_only` (extra paths bound
//! read-only) nor a connector binding's `functions: read_only`.

use super::*;

/// V137 (error): `read_only` on a kind other than `agent_task` or `script`.
/// V138 (error): `read_only` on a list node: its items share one working
/// tree and run at once, so a change could not be blamed on one item, and
/// an item's retry would run again after a write. V139 (warning): `protect`
/// on a read-only node, which already fails on any change.
pub(crate) fn check_read_only(playbook: &Playbook, r: &mut ValidationReport) {
    for n in playbook.nodes.iter().filter(|n| n.read_only) {
        let id = Some(n.id.as_str());
        if !matches!(n.kind, NodeKind::AgentTask { .. } | NodeKind::Script { .. }) {
            r.error(
                "V137",
                id,
                format!(
                    "`read_only` applies to agent_task and script nodes only; node `{}` is a {} node, which would run without the check",
                    n.id,
                    n.kind.type_str()
                ),
            );
            continue;
        }
        if n.list().is_some() {
            r.error(
                "V138",
                id,
                "`read_only` is not allowed on a list node: its items share one working tree and run at once, so a change could not be blamed on one item".into(),
            );
        }
        if !n.kind.protect_globs().is_empty() {
            r.warn(
                "V139",
                id,
                "`protect` is redundant on a read-only node: any change to the tree already fails the node".into(),
            );
        }
    }
}
