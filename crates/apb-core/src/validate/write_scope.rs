//! The declared write scope: V170 to V172 (spec node-contracts D3).
//!
//! V170 (error) refuses a malformed or misplaced `writes` or
//! `max_changed_files`. V171 (warning) names two writers that can run side
//! by side and neither declares a scope. V172 (warning) names the same
//! pair shape where at least one of the two declares `writes` or
//! `max_changed_files`: they run one after another. A `max_changed_files`
//! alone makes a node scoped for the batch rule too, so such a pair never
//! runs side by side and gets V172, not V171.

use super::*;

pub(crate) fn check_write_scope(playbook: &Playbook, r: &mut ValidationReport) {
    let defaults_parallel = playbook.defaults.max_parallel != Some(1);
    for n in &playbook.nodes {
        let id: Option<&str> = Some(&n.id);
        // V170: the declaration itself.
        if n.writes.is_some() || n.max_changed_files.is_some() {
            let is_writer_kind =
                matches!(n.kind, NodeKind::AgentTask { .. } | NodeKind::Script { .. });
            if !is_writer_kind {
                r.error(
                    "V170",
                    id,
                    format!(
                        "`writes` applies to agent_task and script nodes only; node `{}` is a {} node",
                        n.id,
                        n.kind.type_str()
                    ),
                );
            }
            if n.read_only {
                r.error(
                    "V170",
                    id,
                    format!("node `{}` declares a write scope and `read_only`, which no attempt can satisfy", n.id),
                );
            }
            if n.list().is_some() {
                r.error(
                    "V170",
                    id,
                    "`writes` is not allowed on a list node: its items share one working tree and run at once".to_string(),
                );
            }
            if n.cache.is_some() {
                r.error(
                    "V170",
                    id,
                    format!("node `{}` declares a write scope and `cache`: a cache hit settles the node with no scope check in this run", n.id),
                );
            }
            if n.max_changed_files == Some(0) {
                r.error(
                    "V170",
                    id,
                    format!(
                        "node `{}` declares max_changed_files: 0, which no attempt can satisfy",
                        n.id
                    ),
                );
            }
            match &n.writes {
                Some(WriteScope::Globs(globs)) => {
                    if globs.is_empty() {
                        r.error(
                            "V170",
                            id,
                            format!("node `{}` declares an empty `writes` list", n.id),
                        );
                    } else {
                        if globs.len() > crate::write_scope::MAX_SCOPE_ENTRIES {
                            r.error(
                                "V170",
                                id,
                                format!(
                                    "node `{}` declares {} write scope entries, more than the {} a scope may name",
                                    n.id,
                                    globs.len(),
                                    crate::write_scope::MAX_SCOPE_ENTRIES
                                ),
                            );
                        }
                        for g in globs {
                            if let Some(problem) = crate::write_scope::entry_problem(g) {
                                r.error(
                                    "V170",
                                    id,
                                    format!("node `{}` write scope entry `{g}` is refused: {problem}", n.id),
                                );
                            }
                        }
                    }
                }
                Some(WriteScope::Template(text)) => {
                    match crate::write_scope::template_ref(text) {
                        None => r.error(
                            "V170",
                            id,
                            format!(
                                "node `{}` `writes` template is not exactly one `{{{{...}}}}` reference: {text}",
                                n.id
                            ),
                        ),
                        Some(inner) => {
                            let ok = match inner.strip_prefix("nodes.") {
                                Some(rest) => match rest.split_once(".output.") {
                                    Some((node_id, _)) => playbook.node(node_id).is_some(),
                                    None => false,
                                },
                                None => inner.strip_prefix("params.").is_some_and(|name| {
                                    playbook.params.iter().any(|p| p.name == name)
                                }),
                            };
                            if !ok {
                                r.error(
                                    "V170",
                                    id,
                                    format!(
                                        "node `{}` `writes` template reads `{inner}`, which is not `nodes.<id>.output.<field>` of an existing node or `params.<name>` of a declared param",
                                        n.id
                                    ),
                                );
                            }
                        }
                    }
                }
                None => {}
            }
        }
    }
    // V171 and V172: sibling writers from one source.
    if !defaults_parallel {
        return;
    }
    let is_writer = |n: &Node| {
        let interactive = matches!(
            &n.kind,
            NodeKind::AgentTask {
                interactive: true,
                ..
            }
        );
        matches!(n.kind, NodeKind::AgentTask { .. } | NodeKind::Script { .. })
            && !n.read_only
            && !interactive
            && n.list().is_none()
            && n.kind.protect_globs().is_empty()
    };
    let scoped = |n: &Node| n.writes.is_some() || n.max_changed_files.is_some();
    for source in &playbook.nodes {
        let targets: Vec<&Node> = playbook
            .edges
            .iter()
            .filter(|e| e.from == source.id && !e.fallback && e.condition.is_none())
            .filter_map(|e| playbook.node(&e.to))
            .filter(|n| is_writer(n))
            .collect();
        for (i, a) in targets.iter().enumerate() {
            for b in &targets[i + 1..] {
                if !scoped(a) && !scoped(b) {
                    r.warn(
                        "V171",
                        Some(source.id.as_str()),
                        format!(
                            "nodes `{}` and `{}` can write side by side and neither declares `writes`: apb cannot tell which one changed the tree",
                            a.id, b.id
                        ),
                    );
                } else {
                    r.warn(
                        "V172",
                        Some(source.id.as_str()),
                        format!(
                            "nodes `{}` and `{}` are sibling writers and at least one declares a write scope: they run one after another",
                            a.id, b.id
                        ),
                    );
                }
            }
        }
    }
}
