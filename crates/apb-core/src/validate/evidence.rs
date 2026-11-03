//! The rule for a node's `evidence:` block: V159.
//!
//! An `agent_task` with `evidence` reads the latest result snapshots of its
//! `from` nodes through a reading session the engine opens per attempt
//! (see `docs/RESULTS.md`), and the claims in its status file are checked
//! against what that session returned. The rule keeps the declaration to
//! what such a session can serve.

use super::results::kind_takes_results;
use super::*;
use crate::isolation::Level;
use crate::records::is_kind;

/// V159 (error): `evidence` on a node other than `agent_task`; a `from`
/// list that is empty, names an unknown node, a node that settles no
/// result a snapshot can hold, or a node that does not reach the consumer;
/// a `kinds` entry that is not a slug; or `evidence` together with an
/// isolation level the sandbox enforces (`full` or `best_effort`), where
/// the reader cannot reach the run directory.
pub(crate) fn check_evidence(playbook: &Playbook, r: &mut ValidationReport) {
    for n in &playbook.nodes {
        let Some(ev) = &n.evidence else { continue };
        let id = Some(n.id.as_str());
        let mut problem = |why: String| r.error("V159", id, format!("node `{}`: {why}", n.id));
        if !matches!(n.kind, NodeKind::AgentTask { .. }) {
            problem(format!(
                "`evidence` applies to agent_task nodes; this is a {} node, which reads nothing through the engine",
                n.kind.type_str()
            ));
            continue;
        }
        if ev.from.is_empty() {
            problem("`evidence.from` names no node to read".into());
        }
        for from in &ev.from {
            match playbook.node(from) {
                None => problem(format!("`evidence.from` names `{from}`, which is no node")),
                Some(f) if !kind_takes_results(&f.kind) => problem(format!(
                    "`evidence.from` names `{from}`, a {} node, which settles no result a snapshot can hold",
                    f.kind.type_str()
                )),
                Some(_) if !decisions::downstream_nodes(playbook, from).contains(&n.id) => {
                    problem(format!(
                        "`evidence.from` names `{from}`, which does not reach this node, so none of its results can exist when it runs"
                    ))
                }
                Some(_) => {}
            }
        }
        for k in ev.kinds.iter().flatten() {
            if !is_kind(k) {
                problem(format!(
                    "`evidence.kinds` entry `{k}` is not a slug ([a-z0-9][a-z0-9_-]*)"
                ));
            }
        }
        if n.kind.isolation().is_some_and(|s| s.level != Level::None) {
            problem(
                "`evidence` cannot be combined with an enforced isolation level (`full`, `best_effort`): the reader needs the run directory, which the sandbox withholds".into(),
            );
        }
    }
}
