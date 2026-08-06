//! Template rules: which `{{ ... }}` namespaces exist, and whether every
//! reference in a playbook resolves to something the run will actually have.

use super::graph::must_have_finished;
use super::*;
use crate::graphutil::{adjacency, reachable_from};

pub(crate) fn check_templates(playbook: &Playbook, r: &mut ValidationReport) {
    let params: HashSet<&str> = playbook.params.iter().map(|p| p.name.as_str()).collect();
    let nodes: HashSet<&str> = playbook.nodes.iter().map(|n| n.id.as_str()).collect();
    let hooks: HashSet<&str> = playbook
        .nodes
        .iter()
        .filter_map(|n| match &n.kind {
            NodeKind::Wait {
                wait_for: crate::schema::WaitFor::Webhook { key },
                ..
            } => Some(key.as_str()),
            _ => None,
        })
        .collect();

    let check_text = |owner: &str, text: &str, r: &mut ValidationReport| {
        for cap in template_refs(text) {
            let parts: Vec<&str> = cap.split('.').collect();
            let ok = match parts.as_slice() {
                ["params", p] => params.contains(p),
                [
                    "nodes",
                    nid,
                    "output" | "report" | "review_note" | "rejected_output",
                ] => nodes.contains(nid),
                ["run", "instruction" | "context"] => true,
                ["run", "hooks", key] => hooks.contains(key),
                _ => false,
            };
            if !ok {
                r.error(
                    "V13",
                    Some(owner),
                    format!("template `{{{{{cap}}}}}` cannot be resolved{V13_KNOWN_NAMESPACES}"),
                );
            }
        }
    };

    for (owner, text) in template_texts(playbook) {
        check_text(owner, text, r);
    }
}

/// The template-bearing texts of a playbook as `(owner node id, text)` pairs.
/// The single place that knows which node kinds carry a template, so V13 and
/// V38 always scan exactly the same texts.
pub(crate) fn template_texts(playbook: &Playbook) -> Vec<(&str, &str)> {
    playbook
        .nodes
        .iter()
        .filter_map(|n| {
            let text = match &n.kind {
                NodeKind::AgentTask { prompt, .. } | NodeKind::Prompt { prompt } => prompt.as_str(),
                NodeKind::Playbook {
                    instruction: Some(instruction),
                    ..
                } => instruction.as_str(),
                NodeKind::Finish {
                    prompt: Some(prompt),
                    ..
                } => prompt.as_str(),
                _ => return None,
            };
            Some((n.id.as_str(), text))
        })
        .collect()
}

/// V38 (warning): a template reads `nodes.<id>.output` or `nodes.<id>.report`
/// where the graph does not order `<id>` before the reading node. At run time
/// such a read renders as an empty string rather than failing the node (spec
/// 2026-08-05, 1.5), so the mistake is otherwise invisible until an agent gets a
/// prompt with a hole in it.
///
/// Guaranteed-before comes from [`must_have_finished`], so a linear chain, a read
/// at or behind a wait-for-all barrier (explicit `join: all` or the implicit
/// barrier of an acyclic fan-in), and an either-or conditional merge reading both
/// of its branches are all silent. What is left is the genuinely racy read: a
/// parallel sibling branch with nothing synchronizing the two, or a first-arrival
/// merge (`join: any`) reading a branch that may still be running.
///
/// Three shapes are excluded on purpose, to keep the rule quiet where the
/// author's intent is clear:
///
/// * a loop-carried read, where source and reader sit in one cycle (the reader
///   sees the previous iteration's value, which is the idiom the first-arrival
///   semantics of a cycle merge point exist for);
/// * a self-read (a special case of the above test);
/// * a read by the node `defaults.on_failure` routes to, which is entered from
///   anywhere in the graph, so no drawn edge describes what preceded it.
pub(crate) fn check_cross_branch_reads(playbook: &Playbook, r: &mut ValidationReport) {
    let must = must_have_finished(playbook);
    let adj = adjacency(playbook);
    let failure_handler = match &playbook.defaults.on_failure {
        FailurePolicy::Node(target) => Some(target.as_str()),
        _ => None,
    };
    for (owner, text) in template_texts(playbook) {
        if Some(owner) == failure_handler {
            continue;
        }
        for cap in template_refs(text) {
            let parts: Vec<&str> = cap.split('.').collect();
            let (source, field) = match parts.as_slice() {
                ["nodes", source, field @ ("output" | "report")] => (*source, *field),
                // Any other namespace is V13's business, not this rule's.
                _ => continue,
            };
            if playbook.node(source).is_none() {
                // Unknown node: already a V13 error, nothing to add here.
                continue;
            }
            // One cycle around both (a self-read included): loop-carried, not racy.
            if reachable_from(&adj, source).contains(owner)
                && reachable_from(&adj, owner).contains(source)
            {
                continue;
            }
            if must.get(owner).is_some_and(|set| set.contains(source)) {
                continue;
            }
            r.warn(
                "V38",
                Some(owner),
                format!(
                    "template reads `nodes.{source}.{field}` but nothing orders `{source}` before `{owner}`, so the value may render empty; add `join: all` on the edges into `{owner}` (or route it behind a node that joins both branches)"
                ),
            );
        }
    }
}

/// V13 message suffix: names the resolvable template namespaces so an author
/// hitting an unresolved template sees the full set of valid forms, not just
/// the one they got wrong.
pub(crate) const V13_KNOWN_NAMESPACES: &str = "; known namespaces: params.*, nodes.<id>.output, \
    nodes.<id>.report, nodes.<id>.review_note, nodes.<id>.rejected_output, run.instruction, \
    run.context, run.hooks.*";

pub(crate) fn template_refs(text: &str) -> Vec<String> {
    // no regex dependency: manual scan for {{ ... }}
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if &bytes[i..i + 2] == b"{{"
            && let Some(end) = text[i + 2..].find("}}")
        {
            out.push(text[i + 2..i + 2 + end].trim().to_string());
            i += 2 + end + 2;
            continue;
        }
        i += 1;
    }
    out
}

pub(crate) fn check_refs(playbook: &Playbook, ctx: &ValidationContext, r: &mut ValidationReport) {
    // Checking a profile reference (schema 2): scope:project in a global
    // playbook is a schema error; otherwise the name must be among the available profiles.
    let check_profile = |owner: &str, p: &QualifiedProfileRef, r: &mut ValidationReport| {
        if ctx.playbook_origin == PlaybookOrigin::Global && p.scope == ProfileScope::Project {
            r.error(
                "V14",
                Some(owner),
                format!(
                    "global playbook cannot reference project profile `{}`",
                    p.name
                ),
            );
            return;
        }
        // `ctx.profiles` lists only PROJECT profiles, so existence can only be
        // checked against it for an explicit `scope: project`. For
        // `global`/`auto` (which may resolve to global), existence is checked
        // by the scope-aware resolver at run start - otherwise a valid
        // reference to a global profile would falsely trip V14.
        if p.scope == ProfileScope::Project && !ctx.profiles.iter().any(|x| x == &p.name) {
            r.error(
                "V14",
                Some(owner),
                format!("profile `{}` not found", p.name),
            );
        }
    };
    if let Some(p) = &playbook.defaults.profile {
        check_profile("defaults", p, r);
    }
    if let Some(s) = &playbook.supervisor
        && let Some(p) = &s.profile
    {
        check_profile("supervisor", p, r);
    }
    let has_default = playbook.defaults.profile.is_some();
    for n in &playbook.nodes {
        // Nodes that run an agent (agent_task and finish-with-prompt) need an
        // executor binding and get identical scope checks. A finish WITHOUT a
        // prompt never runs an agent and needs no binding (a profile on such a
        // node is a V21 authoring error, handled in check_finish).
        if !n.kind.runs_agent() {
            continue;
        }
        let node_profile = match &n.kind {
            NodeKind::AgentTask { profile, .. } | NodeKind::Finish { profile, .. } => {
                profile.as_ref()
            }
            _ => None,
        };
        if let Some(p) = node_profile {
            check_profile(&n.id, p, r);
        }
        // V18: a node that runs an agent must have an executor binding - a
        // profile on the node or `defaults.profile`.
        if node_profile.is_none() && !has_default {
            r.error(
                "V18",
                Some(&n.id),
                format!(
                    "node `{}` runs an agent but has no profile and playbook has no defaults.profile",
                    n.id
                ),
            );
        }
    }
}
