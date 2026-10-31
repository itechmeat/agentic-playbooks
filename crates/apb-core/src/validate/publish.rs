//! Rules for a `publish` node and `publish_only` (see [`crate::publish`]):
//! V116-V120.

use super::node_io::value_ref_ok;
use super::templates::template_refs;
use super::*;
use crate::isolation::Level;
use crate::publish::{MAX_TIMEOUT_SECONDS, refname};

/// The `run.*` value a publish `branch` may read.
const BRANCH_RUN_VALUES: [&str; 1] = ["id"];

pub(crate) fn check_publish(playbook: &Playbook, r: &mut ValidationReport) {
    let mut publishes = false;
    for n in &playbook.nodes {
        let NodeKind::Publish {
            from,
            remote,
            branch,
            timeout_seconds,
            ..
        } = &n.kind
        else {
            continue;
        };
        publishes = true;
        check_from(playbook, &n.id, from, r);
        check_target(playbook, n, remote, branch.as_deref(), *timeout_seconds, r);
        check_fields(n, r);
    }
    check_isolation(playbook, publishes, r);
}

/// V116 (error): `from` names an `agent_task` or `script` node, not a list
/// node, from which the publish node is reachable.
fn check_from(playbook: &Playbook, id: &str, from: &str, r: &mut ValidationReport) {
    let Some(source) = playbook.node(from) else {
        r.error(
            "V116",
            Some(id),
            format!("`from` names `{from}`, which is not a node of this playbook"),
        );
        return;
    };
    if !matches!(
        source.kind,
        NodeKind::AgentTask { .. } | NodeKind::Script { .. }
    ) {
        r.error(
            "V116",
            Some(id),
            format!("`from` names `{from}`, a {} node; a publish node publishes what an agent_task or script node committed", source.kind.type_str()),
        );
    } else if source.list().is_some() {
        r.error(
            "V116",
            Some(id),
            format!("`from` names `{from}`, a list node; its items commit separately, so there is no one commit to publish"),
        );
    }
    if !crate::graphutil::reachable(playbook, &[from]).contains(id) {
        r.error(
            "V116",
            Some(id),
            format!("the publish node is not reachable from `{from}`, so it can never publish what `{from}` committed"),
        );
    }
}

/// V117 (error): the remote's grammar, what the branch template reads, a
/// literal branch's ref-name rules, and the timeout's range.
fn check_target(
    playbook: &Playbook,
    n: &crate::schema::Node,
    remote: &str,
    branch: Option<&str>,
    timeout: Option<u64>,
    r: &mut ValidationReport,
) {
    let id = Some(n.id.as_str());
    if !refname::remote_ok(remote) {
        r.error(
            "V117",
            id,
            format!("`remote: {remote}` is not a remote name ([A-Za-z0-9._-], not led by `-`)"),
        );
    }
    if let Some(branch) = branch {
        let refs = template_refs(branch);
        for cap in &refs {
            if !branch_ref_ok(playbook, cap) {
                r.error(
                    "V117",
                    id,
                    format!(
                        "branch template `{{{{{cap}}}}}` is not allowed; a branch may read params.*, run.id, nodes.<id>.output and nodes.<id>.output.<field> of an existing node"
                    ),
                );
            }
        }
        // The literal parts must make a valid name whatever the values are.
        if !refname::branch_ok(&literal_shape(branch)) {
            r.error(
                "V117",
                id,
                format!("`branch: {branch}` is not a valid branch name"),
            );
        }
    }
    if let Some(t) = timeout
        && !(1..=MAX_TIMEOUT_SECONDS).contains(&t)
    {
        r.error(
            "V117",
            id,
            format!("`timeout_seconds: {t}` must be between 1 and {MAX_TIMEOUT_SECONDS}"),
        );
    }
}

/// Whether a branch template may read `cap`: what a path may read, minus a
/// node's report and review decision (prose, never a ref name), with
/// `run.id` for `run.instruction`.
fn branch_ref_ok(playbook: &Playbook, cap: &str) -> bool {
    match cap.split('.').collect::<Vec<&str>>().as_slice() {
        ["nodes", _, "report" | "review_decision", ..] => false,
        _ => value_ref_ok(playbook, cap, &BRANCH_RUN_VALUES),
    }
}

/// `text` with every `{{ ... }}` replaced by a plain word.
fn literal_shape(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("}}") {
            Some(end) => {
                out.push('x');
                rest = &rest[start + 2 + end + 2..];
            }
            None => {
                rest = &rest[start..];
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// V118 (error): fields a publish node does not take. `auto_decide` is V72.
fn check_fields(n: &crate::schema::Node, r: &mut ValidationReport) {
    let set: [(&str, bool); 9] = [
        ("list", n.list.is_some()),
        ("fork", n.fork.is_some()),
        ("cache", n.cache.is_some()),
        ("success_check", n.success_check.is_some()),
        ("route", n.route.is_some()),
        ("completion_check", n.completion_check.is_some()),
        ("option_descriptions", !n.option_descriptions.is_empty()),
        ("auto_decide_ok", n.auto_decide_ok),
        ("inputs/outputs", n.inputs.is_some() || n.outputs.is_some()),
    ];
    for (field, present) in set {
        if present {
            r.error(
                "V118",
                Some(&n.id),
                format!("a publish node does not take `{field}`: its consent is a person's, and it pushes exactly the plan"),
            );
        }
    }
}

/// A node that would run an agent outside `isolation: full`, and what the
/// author can change about it.
pub struct OpenAgent<'a> {
    pub node: &'a str,
    pub fix: &'static str,
}

/// Every node of `playbook` that runs an agent without `isolation: full`:
/// what `publish_only` refuses (V120 at validation, and a child run held to
/// its parent's `publish_only` at its start). A `finish` prompt and a judge
/// that emulates run their agent without any sandbox, so their fix is to
/// drop that, not to add an isolation they cannot take.
pub fn publish_only_violations(playbook: &Playbook) -> Vec<OpenAgent<'_>> {
    playbook
        .nodes
        .iter()
        .filter(|n| n.kind.runs_agent())
        .filter_map(|n| {
            let fix = match &n.kind {
                NodeKind::AgentTask { .. } => {
                    if n.kind.isolation().is_some_and(|s| s.level == Level::Full) {
                        return None;
                    }
                    "set `isolation: full`"
                }
                NodeKind::Finish { .. } => {
                    "a finish prompt runs its agent without a sandbox; drop the prompt"
                }
                NodeKind::Judge { .. } => {
                    "an emulating judge runs its agent without a sandbox; choose another `on_unavailable`"
                }
                _ => "it runs an agent without a sandbox",
            };
            Some(OpenAgent { node: &n.id, fix })
        })
        .collect()
}

/// V119 (warning) and V120: who else can push. An agent without `isolation:
/// full` has the operator's credentials, so it can push by itself; under
/// `publish_only` that is an error. A script node, a baseline, goal or
/// project-check script and a sub-playbook (whose own nodes are not visible
/// here) run outside the sandbox or unchecked: always a warning. Under
/// `publish_only` a sub-playbook's run is held to the same rule when it
/// starts.
fn check_isolation(playbook: &Playbook, publishes: bool, r: &mut ValidationReport) {
    if playbook.publish_only && !publishes {
        r.warn(
            "V120",
            None,
            "`publish_only` is set but the playbook has no publish node".to_string(),
        );
    }
    if !publishes {
        return;
    }
    for open in publish_only_violations(playbook) {
        let text = format!(
            "the playbook publishes, and node `{}` runs an agent without `isolation: full`, so it can push by itself with the operator's credentials ({})",
            open.node, open.fix
        );
        if playbook.publish_only {
            r.error("V120", Some(open.node), format!("`publish_only`: {text}"));
        } else {
            r.warn("V119", Some(open.node), text);
        }
    }
    let scripts = outside_scripts(playbook);
    if !scripts.is_empty() {
        r.warn(
            "V119",
            None,
            format!(
                "the playbook publishes, and {} run outside any sandbox with the operator's credentials, so they can push by themselves",
                scripts.join(", ")
            ),
        );
    }
    let children: Vec<&str> = playbook
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Playbook { .. }))
        .map(|n| n.id.as_str())
        .collect();
    if !children.is_empty() {
        let held = if playbook.publish_only {
            "under `publish_only` each child run refuses to start unless every one of its agents is `isolation: full`"
        } else {
            "set `publish_only` to hold each child run to `isolation: full`"
        };
        r.warn(
            "V119",
            None,
            format!(
                "the playbook publishes, and node(s) {} run a sub-playbook whose agents and scripts are not checked here; {held}",
                names(&children)
            ),
        );
    }
}

/// The scripts that run outside the sandbox: script nodes, baseline script
/// and project-check sources, and goal criteria checked by a script.
fn outside_scripts(playbook: &Playbook) -> Vec<String> {
    let mut out = Vec::new();
    for n in &playbook.nodes {
        if matches!(n.kind, NodeKind::Script { .. }) {
            out.push(format!("script node `{}`", n.id));
        }
    }
    for c in playbook.baseline_checks() {
        if c.script.is_some() || c.project_checks.is_some() {
            out.push(format!("baseline check `{}`", c.id));
        }
    }
    for (i, c) in playbook
        .goal
        .iter()
        .flat_map(|g| g.criteria.iter())
        .enumerate()
    {
        if matches!(c.check, crate::schema::GoalCheck::Script { .. }) {
            out.push(format!("goal criterion {} (a script)", i + 1));
        }
    }
    out
}

fn names(ids: &[&str]) -> String {
    ids.iter()
        .map(|i| format!("`{i}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::literal_shape;

    #[test]
    fn the_literal_shape_replaces_every_template() {
        assert_eq!(literal_shape("apb/{{run.id}}"), "apb/x");
        assert_eq!(literal_shape("{{params.a}}-{{params.b}}"), "x-x");
        assert_eq!(literal_shape("a{{b"), "a{{b");
    }
}
