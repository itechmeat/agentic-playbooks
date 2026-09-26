//! Rules for the node fields added by issue #67: the warm session handoff
//! (`continue_session`, V44/V45), declared output fields (V46) and the node
//! `workdir` template (V47).

use super::graph::must_have_finished;
use super::templates::template_refs;
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
    let isolated = |kind: &NodeKind| {
        matches!(
            kind,
            NodeKind::AgentTask {
                isolation: Some(Isolation::Full | Isolation::BestEffort),
                ..
            }
        )
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
        if isolated(&source.kind) || isolated(&node.kind) {
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

/// V46 (warning): a template (prompt or workdir) or an `output_field` edge
/// reads `nodes.<id>.output.<field>` of a node that declares `outputs.fields`
/// without that field. Nodes that declare nothing are not checked, so
/// existing playbooks see no new warnings.
pub(crate) fn check_declared_fields(playbook: &Playbook, r: &mut ValidationReport) {
    let declared = |id: &str| -> Option<&Vec<String>> {
        playbook
            .node(id)
            .and_then(|n| n.outputs.as_ref())
            .map(|o| &o.fields)
            .filter(|f| !f.is_empty())
    };
    let report = |owner: Option<&str>, source: &str, field: &str, r: &mut ValidationReport| {
        if let Some(fields) = declared(source)
            && !fields.iter().any(|f| f == field)
        {
            r.warn(
                "V46",
                owner,
                format!(
                    "reads field `{field}` of `{source}`, which declares only outputs.fields [{}]",
                    fields.join(", ")
                ),
            );
        }
    };
    let mut texts = super::templates::template_texts(playbook);
    texts.extend(workdir_texts(playbook));
    for (owner, text) in texts {
        for cap in template_refs(text) {
            if let ["nodes", source, "output" | "report", field] =
                cap.split('.').collect::<Vec<&str>>().as_slice()
            {
                report(Some(owner), source, field, r);
            }
        }
    }
    for edge in &playbook.edges {
        if let Some(EdgeCondition::OutputField { node, field, .. }) = &edge.condition {
            report(Some(&edge.from), node, field, r);
        }
    }
}

/// The `workdir` templates of a playbook as `(owner node id, text)` pairs.
pub(crate) fn workdir_texts(playbook: &Playbook) -> Vec<(&str, &str)> {
    playbook
        .nodes
        .iter()
        .filter_map(|n| n.kind.workdir_template().map(|w| (n.id.as_str(), w)))
        .collect()
}

/// V47 (error): a `workdir` template may read only what can name a path -
/// `params.*`, `run.instruction`, a node's output (or one field of it) and a
/// review decision - of a node that exists; and a node cannot combine
/// `workdir` with `isolation` (an isolated node runs in its own directory).
pub(crate) fn check_workdir(playbook: &Playbook, r: &mut ValidationReport) {
    let params: HashSet<&str> = playbook.params.iter().map(|p| p.name.as_str()).collect();
    for (owner, text) in workdir_texts(playbook) {
        for cap in template_refs(text) {
            let ok = match cap.split('.').collect::<Vec<&str>>().as_slice() {
                ["params", p] => params.contains(p),
                ["run", "instruction"] => true,
                ["nodes", id, "output" | "report" | "review_decision"] => {
                    playbook.node(id).is_some()
                }
                ["nodes", id, "output" | "report", field] => {
                    playbook.node(id).is_some() && !field.trim().is_empty()
                }
                _ => false,
            };
            if !ok {
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
        if let NodeKind::AgentTask {
            isolation: Some(Isolation::Full | Isolation::BestEffort),
            ..
        } = &node.kind
            && node.kind.workdir_template().is_some()
        {
            r.error(
                "V47",
                Some(&node.id),
                "workdir cannot be combined with isolation: an isolated node runs in its own directory".to_string(),
            );
        }
    }
}
