//! Rules for a node's `evidence:` block: V159.
//!
//! The block names the nodes whose stored snapshots an `agent_task`'s
//! attempts read through the engine's evidence reader (see the engine's
//! `evidence` module). Every `from` node takes a snapshot
//! ([`super::snapshotted_nodes`] names it), so the validator only keeps the
//! block to an `agent_task` that a source can reach, with slug kinds, and
//! off `isolation: full`, where the reader cannot reach the run directory.

use super::*;
use crate::isolation::Level;

/// V159 (error): `evidence` on a kind other than `agent_task`; a `from`
/// node that is not declared or whose work cannot reach the consumer
/// before one of its attempts (forward, or around a loop for the consumer
/// itself); an empty `from`; a `kinds` entry that is not a slug; `evidence`
/// with `isolation: full`.
pub(crate) fn check_evidence(playbook: &Playbook, r: &mut ValidationReport) {
    let adj = crate::graphutil::adjacency(playbook);
    for n in &playbook.nodes {
        let Some(decl) = &n.evidence else { continue };
        let id = Some(n.id.as_str());
        if !matches!(n.kind, NodeKind::AgentTask { .. }) {
            r.error(
                "V159",
                id,
                format!(
                    "`evidence` applies to agent_task nodes only; node `{}` is a {} node",
                    n.id,
                    n.kind.type_str()
                ),
            );
            continue;
        }
        if decl.from.is_empty() {
            r.error(
                "V159",
                id,
                "`evidence.from` names no node: name the nodes whose results the attempts read"
                    .into(),
            );
        }
        for from in &decl.from {
            if playbook.node(from).is_none() {
                r.error(
                    "V159",
                    id,
                    format!("`evidence.from` names `{from}`, which is not a node of this playbook"),
                );
                continue;
            }
            let reaches =
                adj.get(from.as_str()).into_iter().flatten().any(|next| {
                    crate::graphutil::reachable_from(&adj, next).contains(n.id.as_str())
                });
            if !reaches {
                r.error(
                    "V159",
                    id,
                    format!(
                        "`evidence.from` names `{from}`, which never runs before `{}`: no edge path leads from it to the node",
                        n.id
                    ),
                );
            }
        }
        for kind in decl.kinds.iter().flatten() {
            if !crate::records::is_kind(kind) {
                r.error(
                    "V159",
                    id,
                    format!("`evidence.kinds` entry `{kind}` is not a slug ([a-z0-9][a-z0-9_-]*)"),
                );
            }
        }
        if n.kind.isolation().is_some_and(|i| i.level == Level::Full) {
            r.error(
                "V159",
                id,
                "`evidence` is not allowed with `isolation: full`: the evidence reader reads the run directory, which a fully isolated attempt does not see".into(),
            );
        }
    }
}
