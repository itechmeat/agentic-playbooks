use std::collections::{HashMap, HashSet, VecDeque};

use crate::profile::{ProfileScope, QualifiedProfileRef};
use crate::profile_store::PlaybookOrigin;
use crate::schema::{EdgeCondition, FunctionsAllow, Isolation, NodeKind, Playbook};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug)]
pub struct Issue {
    pub code: &'static str,
    pub severity: Severity,
    pub message: String,
    pub node: Option<String>,
}

#[derive(Debug, Default)]
pub struct ValidationReport {
    pub issues: Vec<Issue>,
}

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        !self.issues.iter().any(|i| i.severity == Severity::Error)
    }
    fn error(&mut self, code: &'static str, node: Option<&str>, msg: String) {
        self.issues.push(Issue {
            code,
            severity: Severity::Error,
            message: msg,
            node: node.map(String::from),
        });
    }
    fn warn(&mut self, code: &'static str, node: Option<&str>, msg: String) {
        self.issues.push(Issue {
            code,
            severity: Severity::Warning,
            message: msg,
            node: node.map(String::from),
        });
    }
}

#[derive(Debug, Default)]
pub struct ValidationContext {
    /// Names of the available project profiles (for a structural existence
    /// check). Full scope-aware resolution happens at run start.
    pub profiles: Vec<String>,
    /// Origin of the playbook being checked: a global playbook cannot
    /// reference a profile with `scope: project` (V14).
    pub playbook_origin: PlaybookOrigin,
}

pub fn validate(playbook: &Playbook, ctx: &ValidationContext) -> ValidationReport {
    let mut r = ValidationReport::default();
    check_unique_ids(playbook, &mut r); // V01, V02
    check_expected_duration(playbook, &mut r); // V19, V20
    check_finish(playbook, &mut r); // V21
    check_playbook_ref(playbook, &mut r); // V22
    check_connectors(playbook, &mut r); // V23, V24, V25, V26
    check_start_finish(playbook, &mut r); // V03, V04, V05
    check_edges_exist(playbook, &mut r); // V06
    if r.is_valid() {
        check_reachability(playbook, &mut r); // V07, V08
        check_conditions(playbook, &mut r); // V09, V10
        check_cycles(playbook, &mut r); // V11
        check_scripts(playbook, &mut r); // V12
        check_templates(playbook, &mut r); // V13
        check_refs(playbook, ctx, &mut r); // V14, V15
        check_isolation(playbook, &mut r); // V16
        check_trigger(playbook, &mut r); // V17
    }
    r
}

/// V17: structured trigger fields (spec 8.5) are machine-facing and compact.
/// Limits: at most 5 lines per field, each line <= 120 characters. Otherwise
/// the field starts carrying free-form text, which is unsafe to display and
/// match against.
const TRIGGER_MAX_ITEMS: usize = 5;
const TRIGGER_MAX_LEN: usize = 120;

fn check_trigger(playbook: &Playbook, r: &mut ValidationReport) {
    let Some(t) = &playbook.trigger else { return };
    for (field, items) in [
        ("when", &t.when),
        ("avoid_when", &t.avoid_when),
        ("examples", &t.examples),
    ] {
        if items.len() > TRIGGER_MAX_ITEMS {
            r.error(
                "V17",
                None,
                format!(
                    "trigger.{field} has {} items, max {TRIGGER_MAX_ITEMS}",
                    items.len()
                ),
            );
        }
        for (i, s) in items.iter().enumerate() {
            if s.chars().count() > TRIGGER_MAX_LEN {
                r.error(
                    "V17",
                    None,
                    format!(
                        "trigger.{field}[{i}] is {} chars, max {TRIGGER_MAX_LEN}",
                        s.chars().count()
                    ),
                );
            }
        }
    }
}

/// V16: isolation is declared. The engine materializes skills as copies into
/// an isolated per-node workdir (skills_mode: materialized), but does not yet
/// enforce full sandboxing (project tree, process) (spec 8.3). A warning so the
/// enforcement boundary is stated honestly rather than implied.
fn check_isolation(playbook: &Playbook, r: &mut ValidationReport) {
    for n in &playbook.nodes {
        if let NodeKind::AgentTask {
            isolation: Some(iso),
            ..
        } = &n.kind
            && !matches!(iso, Isolation::None)
        {
            let name = match iso {
                Isolation::Full => "full",
                Isolation::BestEffort => "best_effort",
                Isolation::None => "none",
            };
            r.warn(
                "V16",
                Some(&n.id),
                format!("isolation `{name}` materializes skill copies into an isolated node workdir, but full sandbox isolation (project tree, process) is not yet enforced; see spec 8.3"),
            );
        }
    }
}

/// V19 (warning): an agent_task or script node without `expected_duration`
/// (nudges authors; never blocks). V20 (error): an `expected_duration` value
/// that cannot be parsed to seconds.
fn check_expected_duration(playbook: &Playbook, r: &mut ValidationReport) {
    for n in &playbook.nodes {
        match &n.expected_duration {
            Some(ed) if ed.parsed().is_none() => {
                r.error(
                    "V20",
                    Some(&n.id),
                    format!(
                        "node `{}` has an unparsable expected_duration; use seconds like `90` or a single unit like `30s`, `5m`, `2h`",
                        n.id
                    ),
                );
            }
            None if n.kind.needs_duration_estimate() => {
                r.warn(
                    "V19",
                    Some(&n.id),
                    format!(
                        "node `{}` has no expected_duration; progress will use the {}s default",
                        n.id,
                        crate::duration::DEFAULT_TASK_SECONDS
                    ),
                );
            }
            _ => {}
        }
    }
}

/// V21 (error): a finish node that binds a `profile` but has no `prompt`. A
/// profile without a prompt can never execute (a finish without a prompt is
/// instant and free), so it is an authoring mistake.
fn check_finish(playbook: &Playbook, r: &mut ValidationReport) {
    for n in &playbook.nodes {
        if let NodeKind::Finish {
            prompt: None,
            profile: Some(_),
            ..
        } = &n.kind
        {
            r.error(
                "V21",
                Some(&n.id),
                format!(
                    "finish node `{}` binds a profile but has no prompt; a profile without a prompt can never execute",
                    n.id
                ),
            );
        }
    }
}

/// V22 (error): a playbook node whose reference id is empty or not a safe path
/// segment. Resolvability of the reference is a gate/adopt concern (the offline
/// validator cannot see other playbooks).
fn check_playbook_ref(playbook: &Playbook, r: &mut ValidationReport) {
    for n in &playbook.nodes {
        if let NodeKind::Playbook { playbook: pref, .. } = &n.kind
            && (pref.id.is_empty() || !crate::registry::is_safe_segment(&pref.id))
        {
            r.error(
                "V22",
                Some(&n.id),
                format!(
                    "playbook node `{}` has an empty or invalid playbook reference",
                    n.id
                ),
            );
        }
    }
}

/// V23 (error): a connector binding name, an `accounts` entry, or a
/// `functions` list entry fails its identifier format check. Binding names
/// and account entries are connector/account folder names - hyphen slugs
/// (`crate::profile::validate_profile_name`); `functions` list entries are
/// the connector's snake_case function names
/// (`crate::connector::validate_snake_name`). V24 (error): a node binds the
/// same connector name more than once. V25 (error): an `accounts` or
/// `functions` list entry that is empty or repeated within one binding. V26
/// (error): `max_calls` is 0 (a binding that can never be called).
fn check_connectors(playbook: &Playbook, r: &mut ValidationReport) {
    for n in &playbook.nodes {
        let mut seen_connectors = HashSet::new();
        for b in n.kind.connector_bindings() {
            if !seen_connectors.insert(b.name.as_str()) {
                r.error(
                    "V24",
                    Some(&n.id),
                    format!(
                        "node `{}` binds connector `{}` more than once",
                        n.id, b.name
                    ),
                );
            }
            if let Err(msg) = crate::profile::validate_profile_name(&b.name) {
                r.error(
                    "V23",
                    Some(&n.id),
                    format!(
                        "node `{}` connector `{}` has an invalid name: {msg}",
                        n.id, b.name
                    ),
                );
            }
            if let Some(accounts) = &b.accounts {
                check_connector_list(&n.id, &b.name, "accounts", accounts, r, |item| {
                    crate::profile::validate_profile_name(item)
                });
            }
            if let FunctionsAllow::List(names) = &b.functions {
                check_connector_list(&n.id, &b.name, "functions", names, r, |item| {
                    crate::connector::validate_snake_name(item)
                });
            }
            if b.max_calls == Some(0) {
                r.error(
                    "V26",
                    Some(&n.id),
                    format!("node `{}` connector `{}` has max_calls 0", n.id, b.name),
                );
            }
        }
    }
}

/// Checks one `accounts`/`functions` list of a connector binding: every
/// entry must be non-empty, unique within the list (V25), and pass its
/// identifier format check (V23). `field` names the offending list in the
/// message (`accounts` or `functions`).
fn check_connector_list(
    node_id: &str,
    connector: &str,
    field: &str,
    items: &[String],
    r: &mut ValidationReport,
    validate: impl Fn(&str) -> Result<(), String>,
) {
    let mut seen = HashSet::new();
    for item in items {
        if item.is_empty() {
            r.error(
                "V25",
                Some(node_id),
                format!("node `{node_id}` connector `{connector}` has an empty {field} entry"),
            );
            continue;
        }
        if !seen.insert(item.as_str()) {
            r.error(
                "V25",
                Some(node_id),
                format!(
                    "node `{node_id}` connector `{connector}` has duplicate {field} entry `{item}`"
                ),
            );
        }
        if let Err(msg) = validate(item) {
            r.error(
                "V23",
                Some(node_id),
                format!(
                    "node `{node_id}` connector `{connector}` {field} entry `{item}` is invalid: {msg}"
                ),
            );
        }
    }
}

fn adjacency(playbook: &Playbook) -> HashMap<&str, Vec<&str>> {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for n in &playbook.nodes {
        adj.entry(n.id.as_str()).or_default();
    }
    for e in &playbook.edges {
        adj.entry(e.from.as_str()).or_default().push(e.to.as_str());
    }
    adj
}

fn check_unique_ids(playbook: &Playbook, r: &mut ValidationReport) {
    let mut seen = HashSet::new();
    for n in &playbook.nodes {
        if !seen.insert(n.id.as_str()) {
            r.error("V01", Some(&n.id), format!("duplicate node id `{}`", n.id));
        }
    }
    let mut pseen = HashSet::new();
    for p in &playbook.params {
        if !pseen.insert(p.name.as_str()) {
            r.error("V02", None, format!("duplicate param name `{}`", p.name));
        }
    }
}

fn check_start_finish(playbook: &Playbook, r: &mut ValidationReport) {
    let starts: Vec<_> = playbook
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Start))
        .collect();
    if starts.len() != 1 {
        r.error(
            "V03",
            None,
            format!("expected exactly one start node, found {}", starts.len()),
        );
    }
    for e in &playbook.edges {
        if let Some(to) = playbook.node(&e.to)
            && matches!(to.kind, NodeKind::Start)
        {
            r.error(
                "V04",
                Some(&e.to),
                "start node must not have incoming edges".into(),
            );
        }
        if let Some(from) = playbook.node(&e.from)
            && matches!(from.kind, NodeKind::Finish { .. })
        {
            r.error(
                "V05",
                Some(&e.from),
                "finish node must not have outgoing edges".into(),
            );
        }
    }
}

fn check_edges_exist(playbook: &Playbook, r: &mut ValidationReport) {
    for e in &playbook.edges {
        for id in [&e.from, &e.to] {
            if playbook.node(id).is_none() {
                r.error(
                    "V06",
                    Some(id),
                    format!("edge references unknown node `{id}`"),
                );
            }
        }
    }
}

fn check_reachability(playbook: &Playbook, r: &mut ValidationReport) {
    let Some(start) = playbook
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Start))
    else {
        return;
    };
    let adj = adjacency(playbook);
    let mut seen = HashSet::new();
    let mut q = VecDeque::from([start.id.as_str()]);
    while let Some(id) = q.pop_front() {
        if seen.insert(id) {
            for next in adj.get(id).into_iter().flatten() {
                q.push_back(next);
            }
        }
    }
    for n in &playbook.nodes {
        if !seen.contains(n.id.as_str()) {
            r.error(
                "V07",
                Some(&n.id),
                format!("node `{}` is unreachable from start", n.id),
            );
        }
    }
    // V08: from every reachable node some finish node must be reachable (otherwise warning)
    let finishes: HashSet<&str> = playbook
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Finish { .. }))
        .map(|n| n.id.as_str())
        .collect();
    for n in &playbook.nodes {
        if !seen.contains(n.id.as_str()) {
            continue;
        }
        let mut vis = HashSet::new();
        let mut q = VecDeque::from([n.id.as_str()]);
        let mut ok = false;
        while let Some(id) = q.pop_front() {
            if finishes.contains(id) {
                ok = true;
                break;
            }
            if vis.insert(id) {
                for next in adj.get(id).into_iter().flatten() {
                    q.push_back(next);
                }
            }
        }
        if !ok {
            r.warn(
                "V08",
                Some(&n.id),
                format!("no path from `{}` to any finish node", n.id),
            );
        }
    }
}

fn reachable_from<'a>(adj: &HashMap<&'a str, Vec<&'a str>>, from: &'a str) -> HashSet<&'a str> {
    let mut seen = HashSet::new();
    let mut q = VecDeque::from([from]);
    while let Some(id) = q.pop_front() {
        if seen.insert(id) {
            for next in adj.get(id).into_iter().flatten() {
                q.push_back(next);
            }
        }
    }
    seen
}

fn check_conditions(playbook: &Playbook, r: &mut ValidationReport) {
    let adj = adjacency(playbook);
    for n in &playbook.nodes {
        if !matches!(n.kind, NodeKind::Condition { .. }) {
            continue;
        }
        let out: Vec<_> = playbook.edges.iter().filter(|e| e.from == n.id).collect();
        let has_fallback = out.iter().any(|e| e.fallback);
        // V09: node_status branches must cover success and failure (or declare a fallback)
        let mut covered = HashSet::new();
        for e in &out {
            if let Some(EdgeCondition::NodeStatus { equals, .. }) = &e.condition {
                covered.insert(*equals);
            }
        }
        let uses_node_status = out
            .iter()
            .any(|e| matches!(e.condition, Some(EdgeCondition::NodeStatus { .. })));
        if uses_node_status && covered.len() < 2 && !has_fallback {
            r.error(
                "V09",
                Some(&n.id),
                "condition edges must cover both success and failure or declare a fallback edge"
                    .into(),
            );
        }
        // V10: a condition may only reference nodes from which this condition node is reachable
        for e in &out {
            let referenced = match &e.condition {
                Some(EdgeCondition::NodeStatus { node, .. }) => Some(node),
                Some(EdgeCondition::OutputMatch { node, .. }) => Some(node),
                _ => None,
            };
            if let Some(dep) = referenced {
                let ok = playbook.node(dep).is_some()
                    && reachable_from(&adj, dep.as_str()).contains(n.id.as_str());
                if !ok {
                    r.error(
                        "V10",
                        Some(&n.id),
                        format!(
                            "condition references node `{dep}` that cannot execute before `{}`",
                            n.id
                        ),
                    );
                }
            }
        }
    }
}

fn check_cycles(playbook: &Playbook, r: &mut ValidationReport) {
    // Every cycle must pass through a condition node with max_loops.
    // It's enough to check the SCCs: a component with a cycle must contain such a node.
    let ids: Vec<&str> = playbook.nodes.iter().map(|n| n.id.as_str()).collect();
    let adj = adjacency(playbook);
    // iterative Tarjan
    let index_of: HashMap<&str, usize> = ids.iter().enumerate().map(|(i, s)| (*s, i)).collect();
    let n = ids.len();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut counter = 0usize;
    let mut sccs: Vec<Vec<usize>> = Vec::new();

    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        let mut call: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(&(v, ei)) = call.last() {
            if ei == 0 {
                index[v] = counter;
                low[v] = counter;
                counter += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            let neigh: Vec<usize> = adj
                .get(ids[v])
                .into_iter()
                .flatten()
                .filter_map(|t| index_of.get(t).copied())
                .collect();
            if ei < neigh.len() {
                call.last_mut().expect("frame exists").1 += 1;
                let w = neigh[ei];
                if index[w] == usize::MAX {
                    call.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                if low[v] == index[v] {
                    let mut comp = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        comp.push(w);
                        if w == v {
                            break;
                        }
                    }
                    sccs.push(comp);
                }
                call.pop();
                if let Some(&(parent, _)) = call.last() {
                    low[parent] = low[parent].min(low[v]);
                }
            }
        }
    }

    let self_loop: HashSet<&str> = playbook
        .edges
        .iter()
        .filter(|e| e.from == e.to)
        .map(|e| e.from.as_str())
        .collect();
    for comp in sccs {
        let cyclic = comp.len() > 1 || self_loop.contains(ids[comp[0]]);
        if !cyclic {
            continue;
        }
        let has_guard = comp.iter().any(|&i| {
            matches!(
                playbook.node(ids[i]).map(|n| &n.kind),
                Some(NodeKind::Condition { max_loops: Some(_) })
            )
        });
        if !has_guard {
            let members: Vec<&str> = comp.iter().map(|&i| ids[i]).collect();
            r.error(
                "V11",
                Some(members[0]),
                format!(
                    "cycle [{}] must pass through a condition node with max_loops",
                    members.join(", ")
                ),
            );
        }
    }
}

fn check_scripts(playbook: &Playbook, r: &mut ValidationReport) {
    let escapes =
        |script: &str| script.starts_with('/') || script.split('/').any(|seg| seg == "..");
    for n in &playbook.nodes {
        match &n.kind {
            NodeKind::Script { script, .. } if escapes(script) => {
                r.error(
                    "V12",
                    Some(&n.id),
                    format!("script path `{script}` must stay inside the version directory"),
                );
            }
            NodeKind::AgentTask {
                success_check: Some(script),
                ..
            } if escapes(script) || !script.starts_with("scripts/") => {
                r.error("V12", Some(&n.id),
                    format!("success_check path `{script}` must live under `scripts/` inside the version directory"));
            }
            _ => {}
        }
    }
}

fn check_templates(playbook: &Playbook, r: &mut ValidationReport) {
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
                    format!("template `{{{{{cap}}}}}` cannot be resolved"),
                );
            }
        }
    };

    for n in &playbook.nodes {
        match &n.kind {
            NodeKind::AgentTask { prompt, .. } | NodeKind::Prompt { prompt } => {
                check_text(&n.id, prompt, r)
            }
            _ => {}
        }
    }
}

fn template_refs(text: &str) -> Vec<String> {
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

fn check_refs(playbook: &Playbook, ctx: &ValidationContext, r: &mut ValidationReport) {
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

#[cfg(test)]
mod connector_tests {
    use super::*;
    use crate::schema::Playbook;

    fn ctx() -> ValidationContext {
        ValidationContext::default()
    }

    fn codes(yaml: &str) -> Vec<&'static str> {
        let playbook = Playbook::from_yaml(yaml).unwrap();
        validate(&playbook, &ctx())
            .issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .map(|i| i.code)
            .collect()
    }

    const GOOD: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: start, type: start }
  - id: a
    type: agent_task
    prompt: hi
    profile: x
    connectors:
      - { name: telegram, accounts: [team-bot], functions: [send_message], max_calls: 50 }
      - jira
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: done }
"#;

    #[test]
    fn valid_connector_bindings_have_no_errors() {
        let c = codes(GOOD);
        assert!(c.is_empty(), "expected no errors, got {c:?}");
    }

    #[test]
    fn v23_invalid_connector_name_is_rejected() {
        let bad = GOOD.replace("name: telegram", "name: Telegram");
        assert!(codes(&bad).contains(&"V23"));
    }

    #[test]
    fn v23_invalid_account_entry_is_rejected() {
        let bad = GOOD.replace("accounts: [team-bot]", "accounts: [Team-Bot]");
        assert!(codes(&bad).contains(&"V23"));
    }

    #[test]
    fn v23_invalid_function_entry_is_rejected() {
        let bad = GOOD.replace("functions: [send_message]", "functions: [send-message]");
        assert!(codes(&bad).contains(&"V23"));
    }

    #[test]
    fn v24_duplicate_connector_name_is_rejected() {
        let bad = GOOD.replace("      - jira", "      - jira\n      - jira");
        assert!(codes(&bad).contains(&"V24"));
    }

    #[test]
    fn v25_duplicate_account_entry_is_rejected() {
        let bad = GOOD.replace("accounts: [team-bot]", "accounts: [team-bot, team-bot]");
        assert!(codes(&bad).contains(&"V25"));
    }

    #[test]
    fn v25_empty_function_entry_is_rejected() {
        let bad = GOOD.replace("functions: [send_message]", r#"functions: [""]"#);
        assert!(codes(&bad).contains(&"V25"));
    }

    #[test]
    fn v26_max_calls_zero_is_rejected() {
        let bad = GOOD.replace("max_calls: 50", "max_calls: 0");
        assert!(codes(&bad).contains(&"V26"));
    }
}
