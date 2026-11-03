//! Rules for a node's `results:` declaration: V152 to V154.
//!
//! A declaration names what a snapshot of the node's executions keeps (see
//! [`crate::result_store`]). The rules keep it to work nodes, to paths the
//! capture can walk one component at a time below its scope root, and to a
//! bounded count. What a glob matches at run time (links, secret-shaped
//! names, apb's own storage) is the capture's to refuse; the validator
//! refuses only what a declaration names outright.

use std::collections::BTreeSet;

use super::*;
use crate::result_store::{MAX_DECLARATIONS, ResultScope, is_reserved_first};

/// The nodes whose executions take a snapshot: every node that declares
/// `results:` (an empty list included). The one place later references to
/// a node's results (approvals, evidence) extend.
pub fn snapshotted_nodes(playbook: &Playbook) -> BTreeSet<String> {
    playbook
        .nodes
        .iter()
        .filter(|n| n.results.is_some())
        .map(|n| n.id.clone())
        .collect()
}

/// Whether a node of this kind settles work a snapshot can hold: an
/// `agent_task` or `script` (a list node is one of them) or a `finish` that
/// composes an answer.
fn kind_takes_results(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::AgentTask { .. }
            | NodeKind::Script { .. }
            | NodeKind::Finish {
                prompt: Some(_),
                ..
            }
    )
}

/// V152 (error): `results:` on a node that settles no work of its own.
/// V153 (error): a declared path the capture cannot honour (see
/// [`path_problem`]). V154 (error): more than [`MAX_DECLARATIONS`] entries.
pub(crate) fn check_results(playbook: &Playbook, r: &mut ValidationReport) {
    for n in &playbook.nodes {
        let Some(decls) = &n.results else { continue };
        let id = Some(n.id.as_str());
        if !kind_takes_results(&n.kind) {
            r.error(
                "V152",
                id,
                format!(
                    "`results` applies to agent_task, script and finish nodes with a prompt; node `{}` is a {} node{}, which settles no result to keep",
                    n.id,
                    n.kind.type_str(),
                    if matches!(n.kind, NodeKind::Finish { .. }) {
                        " without a prompt"
                    } else {
                        ""
                    }
                ),
            );
        }
        if decls.len() > MAX_DECLARATIONS {
            r.error(
                "V154",
                id,
                format!(
                    "node `{}` declares {} `results` entries; at most {MAX_DECLARATIONS} are allowed",
                    n.id,
                    decls.len()
                ),
            );
        }
        for d in decls {
            if let Some(why) = path_problem(&d.path, d.scope) {
                r.error(
                    "V153",
                    id,
                    format!(
                        "node `{}`: `results` path `{}` ({} scope) {why}",
                        n.id,
                        d.path,
                        d.scope.as_str()
                    ),
                );
            }
        }
    }
}

/// Why a declared path breaks the grammar, or `None`. A path is relative,
/// `/`-separated, without empty, `.` or `..` segments; `*` and `?` are its
/// only wildcards and sit in the last segment; `**` and the other glob
/// syntaxes (`[`, `]`, `{`, `}`, `\`) are refused, so a declaration means
/// the same to every reader. A workspace path stays out of `.git` and
/// `.apb`; a run path names none of apb's own storage.
pub(crate) fn path_problem(path: &str, scope: ResultScope) -> Option<String> {
    if path.is_empty() {
        return Some("is empty".into());
    }
    if path.starts_with('/') {
        return Some("is absolute; it must be relative to its scope root".into());
    }
    if path.contains('\0') {
        return Some("contains a NUL byte".into());
    }
    if path.contains("**") {
        return Some("uses `**`; a glob covers one directory level".into());
    }
    if let Some(c) = path
        .chars()
        .find(|c| matches!(c, '[' | ']' | '{' | '}' | '\\'))
    {
        return Some(format!(
            "contains `{c}`; only `*` and `?` are wildcards and `/` separates segments"
        ));
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Some("has an empty segment".into());
    }
    if segments.iter().any(|s| *s == "." || *s == "..") {
        return Some("has a `.` or `..` segment".into());
    }
    let last = segments.len() - 1;
    if segments[..last].iter().any(|s| s.contains(['*', '?'])) {
        return Some("has a wildcard outside its last segment".into());
    }
    let first = segments[0];
    if !is_reserved_first(scope, first) {
        return None;
    }
    Some(match scope {
        ResultScope::Workspace => {
            format!("reaches into `{first}`, which holds repository or apb data, not a result")
        }
        ResultScope::Run => format!("names `{first}`, apb's own storage in the run directory"),
    })
}
