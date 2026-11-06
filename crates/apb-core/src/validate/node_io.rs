//! Rules for the fields added by issue #67: the warm session handoff
//! (`continue_session`, V44/V45), declared output fields (V46), the node
//! `workdir` template (V47), the run working tree (`worktree`, V48) and a
//! declared cache key (`cache.key`, V49).

use super::graph::must_have_finished;
use super::templates::{
    cache_key_texts, template_refs, template_texts, workdir_texts, writes_texts,
};
use super::*;

/// V44 (error): `continue_session` must name another `agent_task` of the
/// playbook. V45 (warning): a handoff the engine can already tell will start
/// cold - nothing orders the source before the node, the two nodes bind
/// different profiles, either one runs isolated, or their `workdir`s differ.
/// A cold handoff still runs (as a fresh agent), which is why V45 only warns.
pub(crate) fn check_session_handoff(playbook: &Playbook, r: &mut ValidationReport) {
    let must = must_have_finished(playbook);
    let profile_of = |kind: &NodeKind| -> Option<QualifiedProfileRef> {
        match kind {
            NodeKind::AgentTask { profile, .. } => profile
                .clone()
                .or_else(|| playbook.defaults.profile.clone()),
            _ => None,
        }
    };
    for node in &playbook.nodes {
        let NodeKind::AgentTask {
            continue_session: Some(source_id),
            ..
        } = &node.kind
        else {
            continue;
        };
        let source = match playbook.node(source_id) {
            Some(s) if s.id != node.id && matches!(s.kind, NodeKind::AgentTask { .. }) => s,
            _ => {
                r.error(
                    "V44",
                    Some(&node.id),
                    format!(
                        "continue_session `{source_id}` must name another agent_task node of this playbook"
                    ),
                );
                continue;
            }
        };
        let mut cold: Vec<&str> = Vec::new();
        if !must
            .get(node.id.as_str())
            .is_some_and(|s| s.contains(source_id.as_str()))
        {
            cold.push("nothing orders the source before this node");
        }
        if profile_of(&source.kind) != profile_of(&node.kind) {
            cold.push("the two nodes bind different profiles");
        }
        if source.kind.is_isolated() || node.kind.is_isolated() {
            cold.push("an isolated node runs in its own directory");
        }
        if source.kind.workdir_template() != node.kind.workdir_template() {
            cold.push("the two nodes run in different workdirs");
        }
        // Two continuations of one session that may run at the same time
        // would both write into it.
        let racing = playbook.nodes.iter().any(|other| {
            other.id != node.id
                && matches!(&other.kind, NodeKind::AgentTask { continue_session: Some(s), .. } if s == source_id)
                && !must.get(node.id.as_str()).is_some_and(|s| s.contains(other.id.as_str()))
                && !must.get(other.id.as_str()).is_some_and(|s| s.contains(node.id.as_str()))
        });
        if racing {
            cold.push("another node continues the same session and may run at the same time");
        }
        if !cold.is_empty() {
            r.warn(
                "V45",
                Some(&node.id),
                format!(
                    "continue_session `{source_id}` will start a fresh agent instead of continuing its session: {}",
                    cold.join("; ")
                ),
            );
        }
    }
}

/// V46 (warning): a template (prompt, workdir, `writes` or cache key) or an
/// `output_field` or `no_new_items` edge reads `nodes.<id>.output.<field>`
/// (or `.report.<field>`, or `.seen.<field>`) of a node that declares
/// `outputs.fields` without that field. Nodes that declare nothing are not checked, so
/// existing playbooks see no new warnings.
pub(crate) fn check_declared_fields(
    playbook: &Playbook,
    ctx: &ValidationContext,
    r: &mut ValidationReport,
) {
    let fields = declared_fields(playbook, ctx);
    let declared = |id: &str| -> Option<&Vec<String>> { fields.get(id) };
    let report = |owner: Option<&str>,
                  source: &str,
                  field: &str,
                  edge: Option<&crate::schema::Edge>,
                  r: &mut ValidationReport| {
        if let Some(fields) = declared(source)
            && !fields.iter().any(|f| f == field)
        {
            // Raised from an `output_field` edge: that edge; from a
            // template: no edge.
            let edges: Vec<&crate::schema::Edge> = edge.into_iter().collect();
            r.warn_edges(
                playbook,
                "V46",
                owner,
                &edges,
                format!(
                    "reads field `{field}` of `{source}`, which declares only outputs.fields [{}]",
                    fields.join(", ")
                ),
            );
        }
    };
    let mut texts: Vec<(Option<&str>, &str)> = template_texts(playbook)
        .into_iter()
        .chain(workdir_texts(playbook))
        .chain(writes_texts(playbook))
        .chain(cache_key_texts(playbook))
        .map(|(owner, text)| (Some(owner), text))
        .collect();
    // The run working tree belongs to no node.
    texts.extend(playbook.worktree_template().map(|w| (None, w)));
    for (owner, text) in texts {
        for cap in template_refs(text) {
            if let ["nodes", source, "output" | "report" | "seen", field] =
                cap.split('.').collect::<Vec<&str>>().as_slice()
            {
                report(owner, source, field, None, r);
            }
        }
    }
    for edge in &playbook.edges {
        if let Some(EdgeCondition::OutputField { node, field, .. }) = &edge.condition {
            report(Some(&edge.from), node, field, Some(edge), r);
        }
        // The seen template reads a declared field of the loop node, and the
        // condition's field is the same reading: the same V46 warning.
        if let Some(EdgeCondition::NoNewItems { node, field, .. }) = &edge.condition {
            report(Some(&edge.from), node, field, Some(edge), r);
        }
    }
}

/// The fields each node publishes, for nodes whose fields are known: its
/// `outputs.fields`, a list node's counts and items, a judge node's answers
/// and, for a `type: playbook` node with `output: outputs` whose child
/// resolved, the child's declared output names. A node absent from the map
/// is not checked. Shared by V46 and the run outputs rules (V133).
pub(super) fn declared_fields<'a>(
    playbook: &'a Playbook,
    ctx: &ValidationContext,
) -> HashMap<&'a str, Vec<String>> {
    let mut out: HashMap<&str, Vec<String>> = HashMap::new();
    for n in &playbook.nodes {
        let fields = if n.list().is_some() {
            Some(crate::list::LIST_OUTPUT_FIELDS.map(String::from).to_vec())
        } else if let Some(f) = n
            .outputs
            .as_ref()
            .map(|o| crate::schema::field_names(&o.fields))
            .filter(|f| !f.is_empty())
        {
            Some(f.clone())
        } else if let Some(f) = super::judge::judge_output_fields(n) {
            Some(f)
        } else if matches!(
            n.kind,
            NodeKind::Playbook {
                output: crate::run_outputs::ChildOutput::Outputs,
                ..
            }
        ) {
            ctx.child_outputs.get(&n.id).cloned()
        } else {
            None
        };
        if let Some(f) = fields {
            out.insert(n.id.as_str(), f);
        }
    }
    out
}

/// The `run.*` value a `workdir` (V47) or `cache.key` (V49) may read.
pub(super) const PATH_RUN_VALUES: [&str; 1] = ["instruction"];

/// Whether a template reference names a plain value a path, a key or a
/// branch can be built from: `params.<declared>`, one of `run_values`
/// (`run.<name>`), a node's output (or one field of it), report or review
/// decision, of a node that exists. `workdir` (V47) and `cache.key` (V49)
/// pass [`PATH_RUN_VALUES`]; a publish `branch` (V117) passes `id`.
pub(super) fn value_ref_ok(playbook: &Playbook, cap: &str, run_values: &[&str]) -> bool {
    match cap.split('.').collect::<Vec<&str>>().as_slice() {
        ["params", p] => playbook.params.iter().any(|d| d.name == *p),
        ["run", v] => run_values.contains(v),
        ["nodes", id, "output" | "report" | "review_decision"] => playbook.node(id).is_some(),
        ["nodes", id, "output" | "report", field] => {
            playbook.node(id).is_some() && !field.trim().is_empty()
        }
        _ => false,
    }
}

/// V47 (error): a `workdir` template may read only what can name a path -
/// `params.*`, `run.instruction`, a node's output (or one field of it) and a
/// review decision - of a node that exists; and a node cannot combine
/// `workdir` with `isolation` (an isolated node runs in its own directory).
pub(crate) fn check_workdir(playbook: &Playbook, r: &mut ValidationReport) {
    for (owner, text) in workdir_texts(playbook) {
        for cap in template_refs(text) {
            if !value_ref_ok(playbook, &cap, &PATH_RUN_VALUES) {
                r.error(
                    "V47",
                    Some(owner),
                    format!(
                        "workdir template `{{{{{cap}}}}}` is not allowed; a workdir may read params.*, run.instruction, nodes.<id>.output, nodes.<id>.output.<field> and nodes.<id>.review_decision of an existing node"
                    ),
                );
            }
        }
    }
    for node in &playbook.nodes {
        if node.kind.is_isolated() && node.kind.workdir_template().is_some() {
            r.error(
                "V47",
                Some(&node.id),
                "workdir cannot be combined with isolation: an isolated node runs in its own directory".to_string(),
            );
        }
    }
}

/// V48 (error): the run `worktree` template may read only `params.*` and the
/// output (or one field of it) of ONE `agent_task` or `script` node: the tree
/// is resolved either at run start (params only) or when that node succeeds,
/// and a template over two nodes would have no single moment to resolve at.
pub(crate) fn check_worktree(playbook: &Playbook, r: &mut ValidationReport) {
    let Some(text) = playbook.worktree_template() else {
        return;
    };
    let mut sources: Vec<String> = Vec::new();
    for cap in template_refs(text) {
        let ok = match cap.split('.').collect::<Vec<&str>>().as_slice() {
            ["params", p] => playbook.params.iter().any(|d| d.name == *p),
            ["nodes", id, "output"] => producing_node(playbook, id),
            ["nodes", id, "output", field] => {
                producing_node(playbook, id) && !field.trim().is_empty()
            }
            _ => false,
        };
        if !ok {
            r.error(
                "V48",
                None,
                format!(
                    "worktree template `{{{{{cap}}}}}` is not allowed; a worktree may read params.* and nodes.<id>.output or nodes.<id>.output.<field> of an agent_task or script node"
                ),
            );
            continue;
        }
        if let ["nodes", id, ..] = cap.split('.').collect::<Vec<&str>>().as_slice()
            && !sources.iter().any(|s| s == id)
        {
            sources.push((*id).to_string());
        }
    }
    if sources.len() > 1 {
        r.error(
            "V48",
            None,
            format!(
                "worktree reads the outputs of several nodes ({}); it must be resolvable when one node succeeds",
                sources.join(", ")
            ),
        );
    }
}

fn producing_node(playbook: &Playbook, id: &str) -> bool {
    playbook
        .node(id)
        .is_some_and(|n| matches!(n.kind, NodeKind::AgentTask { .. } | NodeKind::Script { .. }))
}

/// V49: a declared `cache.key`. Error: the template reads something that is
/// not a plain value (the same set a `workdir` may read). Warnings: the key
/// has no effect (cache mode off, or a node with its own `workdir`, which
/// never caches), or it reads nothing at all and has no `ttl`, so the first
/// stored result replays until someone refreshes the cache.
pub(crate) fn check_cache_key(playbook: &Playbook, r: &mut ValidationReport) {
    for node in &playbook.nodes {
        let Some(key) = node.cache_key_template() else {
            continue;
        };
        let refs = template_refs(key);
        for cap in &refs {
            if !value_ref_ok(playbook, cap, &PATH_RUN_VALUES) {
                r.error(
                    "V49",
                    Some(&node.id),
                    format!(
                        "cache key template `{{{{{cap}}}}}` is not allowed; a key may read params.*, run.instruction, nodes.<id>.output, nodes.<id>.output.<field> and nodes.<id>.review_decision of an existing node"
                    ),
                );
            }
        }
        if node.cache_mode() != CacheMode::Auto {
            r.warn(
                "V49",
                Some(&node.id),
                "cache.key has no effect: the cache mode is off (set mode: auto)".to_string(),
            );
        } else if node.kind.workdir_template().is_some() {
            r.warn(
                "V49",
                Some(&node.id),
                "cache.key has no effect: a node with its own workdir is never cached".to_string(),
            );
        } else if refs.is_empty() && node.cache_ttl_seconds().is_none() {
            r.warn(
                "V49",
                Some(&node.id),
                "cache.key reads nothing, so the first stored result replays until the cache is refreshed; read what the result depends on or set a ttl".to_string(),
            );
        }
    }
}
