//! List mode rules (V85-V89, V93-V95): where a `list:` block may sit, where
//! its items come from, its item check, send-once on its steps, a judge
//! check's answers, and the goal criterion that counts a list.

use std::collections::BTreeSet;

use super::graph::must_have_finished;
use super::templates::{template_refs, template_texts, workdir_texts};
use super::*;
use crate::list::{
    CheckSource, HARD_MAX_ITEMS, ItemCheck, ListSpec, MAX_BATCH_RETRIES, VERDICT_OPTIONS,
    items::items_from_values,
};
use crate::schema::Node;

/// Every list rule.
pub(crate) fn check_lists(playbook: &Playbook, ctx: &ValidationContext, r: &mut ValidationReport) {
    check_item_refs(playbook, r);
    let must = must_have_finished(playbook);
    let cyclic = in_cycles(playbook);
    for node in &playbook.nodes {
        if crate::list::check_site_node(&node.id).is_some() {
            r.error(
                "V85",
                Some(&node.id),
                format!(
                    "node ids ending in `{}` are reserved for a list's item check",
                    crate::list::LIST_CHECK_SUFFIX
                ),
            );
        }
        let Some(spec) = node.list() else {
            check_continuation_of_list(playbook, node, r);
            continue;
        };
        check_placement(playbook, node, spec, r);
        check_source(playbook, node, spec, &must, r);
        if let Some(check) = &spec.check {
            check_item_check(playbook, node, spec, check, ctx, r);
        } else {
            r.warn(
                "V93",
                Some(&node.id),
                "list has no check: every item's result is self-reported".to_string(),
            );
        }
        check_sends(node, ctx, r);
        if spec.rerun == crate::list::RerunPolicy::NotPassed && !cyclic.contains(node.id.as_str()) {
            r.warn(
                "V93",
                Some(&node.id),
                "rerun: not_passed has no effect on a node that is in no cycle".to_string(),
            );
        }
        let cap = playbook
            .defaults
            .max_parallel
            .map(|n| n.max(1))
            .unwrap_or(crate::schema::DEFAULT_MAX_PARALLEL);
        if spec.batch.parallel as usize > cap {
            r.warn(
                "V94",
                Some(&node.id),
                format!(
                    "batch.parallel {} is larger than max_parallel {cap}; items run at most {cap} at a time",
                    spec.batch.parallel
                ),
            );
        }
    }
    check_goal_lists(playbook, r);
}

/// V85: where `list` may sit and its numbers.
fn check_placement(playbook: &Playbook, node: &Node, spec: &ListSpec, r: &mut ValidationReport) {
    let mut bad: Vec<String> = Vec::new();
    match &node.kind {
        NodeKind::AgentTask {
            interactive,
            continue_session,
            protect,
            ..
        } => {
            if *interactive {
                bad.push("it is interactive".into());
            }
            if continue_session.is_some() {
                bad.push("it continues another node's session".into());
            }
            if !protect.is_empty() && spec.batch.parallel > 1 {
                bad.push("it protects paths while items run in parallel".into());
            }
        }
        NodeKind::Script { .. } => {}
        _ => bad.push(format!(
            "it is a `{}` node; list runs only on agent_task and script",
            node.kind.type_str()
        )),
    }
    if node.cache.is_some() {
        bad.push("it declares cache".into());
    }
    if node.success_check.is_some() {
        bad.push("it declares success_check (use list.check)".into());
    }
    if playbook.nodes.iter().any(|n| {
        matches!(&n.kind, NodeKind::AgentTask { continue_session: Some(s), .. } if *s == node.id)
    }) {
        bad.push("another node continues its session".into());
    }
    let b = &spec.batch;
    let i = &spec.items;
    if b.size == 0 {
        bad.push("batch.size is 0".into());
    }
    if b.parallel == 0 {
        bad.push("batch.parallel is 0".into());
    }
    if b.retries > MAX_BATCH_RETRIES {
        bad.push(format!(
            "batch.retries {} is more than {MAX_BATCH_RETRIES}",
            b.retries
        ));
    }
    if i.max_items == 0 || i.max_items > HARD_MAX_ITEMS {
        bad.push(format!(
            "items.max_items {} is outside 1..={HARD_MAX_ITEMS}",
            i.max_items
        ));
    }
    if !is_identifier(&i.key) {
        bad.push(format!("items.key `{}` is not an identifier", i.key));
    }
    for why in bad {
        r.error("V85", Some(&node.id), format!("list: {why}"));
    }
}

/// V85 on the other side: a node continuing a list node's session.
fn check_continuation_of_list(playbook: &Playbook, node: &Node, r: &mut ValidationReport) {
    if let NodeKind::AgentTask {
        continue_session: Some(source),
        ..
    } = &node.kind
        && playbook.node(source).is_some_and(|s| s.list().is_some())
    {
        r.error(
            "V85",
            Some(&node.id),
            format!("continue_session `{source}` names a list node, whose items run in sessions of their own"),
        );
    }
}

/// V86: the item source.
fn check_source(
    playbook: &Playbook,
    node: &Node,
    spec: &ListSpec,
    must: &HashMap<&str, HashSet<&str>>,
    r: &mut ValidationReport,
) {
    let src = &spec.items;
    let mut err = |msg: String| r.error("V86", Some(&node.id), format!("list.items: {msg}"));
    if let Some(f) = &src.filter {
        if f.field.is_some() != f.equals.is_some() {
            err("where.field and where.equals go together".into());
        }
        if matches!(
            f.equals,
            Some(serde_json::Value::Object(_) | serde_json::Value::Array(_))
        ) {
            err("where.equals must be a string, number, bool or null".into());
        }
        if let Some(state) = &f.state
            && crate::list::verdict::ItemState::parse(state).is_none()
        {
            err(format!(
                "where.state `{state}` is not pending, running, passed or needs_look"
            ));
        }
    }
    match (&src.from, src.inline.is_empty()) {
        (Some(_), false) | (None, true) => {
            err("set exactly one of `from` and `inline`".into());
            return;
        }
        (None, false) => {
            if src.filter.as_ref().is_some_and(|f| f.state.is_some()) {
                err("where.state reads a list node's states; inline items have none".into());
            }
            if let Err(e) = items_from_values(&src.inline, &src.key, src.max_items) {
                err(format!("inline items are not usable: {e}"));
            }
            return;
        }
        (Some(_), true) => {}
    }
    let from = src.from.as_ref().expect("checked above");
    let Some(source) = playbook.node(&from.node) else {
        err(format!("from.node `{}` does not exist", from.node));
        return;
    };
    if !must
        .get(node.id.as_str())
        .is_some_and(|s| s.contains(from.node.as_str()))
    {
        err(format!(
            "from.node `{}` is not guaranteed to finish before this node",
            from.node
        ));
    }
    let from_list = source.list().is_some();
    if from_list {
        if from.field != "items" {
            err(format!(
                "a list node `{}` offers its items as field `items`, not `{}`",
                from.node, from.field
            ));
        }
    } else {
        // A judge node publishes its questions' fields without declaring them.
        let declared = source
            .outputs
            .as_ref()
            .map(|o| o.fields.clone())
            .filter(|f| !f.is_empty())
            .or_else(|| super::judge::judge_output_fields(source))
            .is_some_and(|f| f.contains(&from.field));
        if !declared {
            err(format!(
                "`{}` does not declare `{}` in outputs.fields",
                from.node, from.field
            ));
        }
        if src.filter.as_ref().is_some_and(|f| f.state.is_some()) {
            err("where.state reads a list node's states; the source is not a list node".into());
        }
    }
}

/// V86: `item.*` and `item_output.*` are not template namespaces of a
/// node's prompt or workdir: the engine hands the item as a section of its
/// own after the shared prompt.
fn check_item_refs(playbook: &Playbook, r: &mut ValidationReport) {
    for (owner, text) in template_texts(playbook)
        .into_iter()
        .chain(workdir_texts(playbook))
    {
        for cap in template_refs(text) {
            if crate::list::is_item_ref(&cap) {
                r.error(
                    "V86",
                    Some(owner),
                    format!(
                        "template `{{{{{cap}}}}}` reads the item; a list step gets its item as a section after the shared prompt, so the prompt stays the same for every item"
                    ),
                );
            }
        }
    }
}

/// V87 and V89: the item check.
fn check_item_check(
    playbook: &Playbook,
    node: &Node,
    spec: &ListSpec,
    check: &ItemCheck,
    ctx: &ValidationContext,
    r: &mut ValidationReport,
) {
    let Some(source) = check.source() else {
        r.error(
            "V87",
            Some(&node.id),
            format!(
                "list.check names {} sources; name exactly one of output, script, connector, judge",
                check.source_count()
            ),
        );
        return;
    };
    let mut err = |msg: String| r.error("V87", Some(&node.id), format!("list.check: {msg}"));
    let templates: Vec<&str> = match source {
        CheckSource::Output(o) => {
            if !spec.item_outputs.contains(&o.field) {
                err(format!("output field `{}` is not in item_outputs", o.field));
            }
            Vec::new()
        }
        CheckSource::Script(path) => {
            if !super::nodes::script_check_path_ok(path) {
                err(format!("script `{path}` must be a path under scripts/"));
            }
            Vec::new()
        }
        CheckSource::Connector(c) => {
            if let Some(facts) = ctx.connectors.get(&c.name) {
                if !facts.functions.contains(&c.function) {
                    err(format!(
                        "connector `{}` has no function `{}`",
                        c.name, c.function
                    ));
                } else if facts.write_functions.contains(&c.function) {
                    err(format!(
                        "connector `{}` function `{}` is not read-only; an item check only reads",
                        c.name, c.function
                    ));
                }
            }
            if crate::baseline::extract::parse_path(&c.accept_when.path).is_err() {
                err(format!(
                    "accept_when.path `{}` is not a path",
                    c.accept_when.path
                ));
            }
            if c.accept_when.equals.is_some() == c.accept_when.exists.is_some() {
                err("accept_when sets exactly one of `equals` and `exists`".into());
            }
            // `accept_when.equals` is rendered like the args.
            c.args
                .values()
                .chain(c.accept_when.equals.iter())
                .flat_map(strings)
                .collect()
        }
        CheckSource::Judge(j) => {
            check_judge(node, j, r);
            j.state.iter().map(|(_, t)| t.as_str()).collect()
        }
    };
    for text in templates {
        for cap in template_refs(text) {
            let parts: Vec<&str> = cap.split('.').collect();
            let ok = match parts.as_slice() {
                ["item", rest @ ..] => {
                    !rest.is_empty() && rest.iter().all(|s| !s.trim().is_empty())
                }
                ["item_output", field] => spec.item_outputs.iter().any(|f| f == field),
                ["params", p] => playbook.params.iter().any(|d| d.name == *p),
                _ => false,
            };
            if !ok {
                r.error(
                    "V87",
                    Some(&node.id),
                    format!(
                        "list.check template `{{{{{cap}}}}}` may read only item.*, item_output.<item_outputs> and params.*"
                    ),
                );
            }
        }
    }
}

/// Every string inside a JSON value.
fn strings(v: &serde_json::Value) -> Vec<&str> {
    match v {
        serde_json::Value::String(s) => vec![s.as_str()],
        serde_json::Value::Array(a) => a.iter().flat_map(strings).collect(),
        serde_json::Value::Object(m) => m.values().flat_map(strings).collect(),
        _ => Vec::new(),
    }
}

/// V89: a judge check's answers.
fn check_judge(node: &Node, j: &crate::list::JudgeCheck, r: &mut ValidationReport) {
    let mut err = |msg: String| r.error("V89", Some(&node.id), format!("list.check.judge: {msg}"));
    let keys: Vec<&str> = j.criteria.keys().collect();
    if let Some(k) = keys.iter().find(|k| !VERDICT_OPTIONS.contains(k)) {
        err(format!(
            "criteria key `{k}` is not one of {}",
            VERDICT_OPTIONS.join(", ")
        ));
    }
    if !keys.contains(&"accept") {
        err("criteria must describe `accept`".into());
    }
    if let Some(c) = j.min_confidence
        && !(c > 0.0 && c <= 1.0)
    {
        err(format!("min_confidence {c} is outside (0, 1]"));
    }
    if j.on_unavailable().is_none() {
        err(format!(
            "on_unavailable `{}` is not accept_with_note, reject or needs_look",
            j.on_unavailable
        ));
    }
}

/// V88: a list step may write only through functions that declare
/// send-once (`idempotency`), so a re-run item never acts twice.
fn check_sends(node: &Node, ctx: &ValidationContext, r: &mut ValidationReport) {
    for b in node.kind.connector_bindings() {
        let Some(facts) = ctx.connectors.get(&b.name) else {
            continue;
        };
        let granted = super::decisions::granted(b, &facts.write_functions);
        for f in granted {
            if !facts.idempotency.contains_key(f) {
                r.error(
                    "V88",
                    Some(&node.id),
                    format!(
                        "list step is granted `{}.{f}`, which writes without an idempotency declaration; a re-run item could act twice",
                        b.name
                    ),
                );
            }
        }
    }
}

/// V95: a `type: list` goal criterion names a list node.
fn check_goal_lists(playbook: &Playbook, r: &mut ValidationReport) {
    let Some(goal) = &playbook.goal else {
        return;
    };
    for c in &goal.criteria {
        if let GoalCheck::List { node } = &c.check
            && !playbook.node(node).is_some_and(|n| n.list().is_some())
        {
            r.error(
                "V95",
                None,
                format!(
                    "goal criterion `{}` counts `{node}`, which is not a list node",
                    c.description
                ),
            );
        }
    }
}

/// Node ids that sit in a cycle (a multi-node SCC or a self-loop).
fn in_cycles(playbook: &Playbook) -> BTreeSet<String> {
    let self_loops: BTreeSet<&str> = playbook
        .edges
        .iter()
        .filter(|e| e.from == e.to)
        .map(|e| e.from.as_str())
        .collect();
    crate::graphutil::sccs(playbook)
        .into_iter()
        .filter(|g| g.len() > 1 || self_loops.contains(g[0].as_str()))
        .flatten()
        .collect()
}

fn is_identifier(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}
