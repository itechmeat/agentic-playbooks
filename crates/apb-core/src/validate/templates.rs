//! Template rules: which `{{ ... }}` namespaces exist, and whether every
//! reference in a playbook resolves to something the run will actually have.

use super::*;

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
                ["nodes", nid, "output" | "report" | "review_note"] => nodes.contains(nid),
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

    for n in &playbook.nodes {
        match &n.kind {
            NodeKind::AgentTask { prompt, .. } | NodeKind::Prompt { prompt } => {
                check_text(&n.id, prompt, r)
            }
            NodeKind::Playbook {
                instruction: Some(instruction),
                ..
            } => check_text(&n.id, instruction, r),
            NodeKind::Finish {
                prompt: Some(prompt),
                ..
            } => check_text(&n.id, prompt, r),
            _ => {}
        }
    }
}

/// V13 message suffix: names the resolvable template namespaces so an author
/// hitting an unresolved template sees the full set of valid forms, not just
/// the one they got wrong.
pub(crate) const V13_KNOWN_NAMESPACES: &str = "; known namespaces: params.*, nodes.<id>.output, \
    nodes.<id>.report, nodes.<id>.review_note, run.instruction, run.context, run.hooks.*";

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
