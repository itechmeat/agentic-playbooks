//! Baseline check rules (V62-V68): the `baseline_check` block's structure,
//! the safety of its sources, its rules and paths, its templates, and the
//! reserved grant site.

use std::collections::HashSet;

use super::*;
use crate::baseline::{BASELINE_SITE, BaselineCheck, BaselineSource, MAX_TIMEOUT_SECONDS};

const ID_MAX: usize = 40;

fn id_ok(id: &str) -> bool {
    (1..=ID_MAX).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Runs every baseline rule.
pub(crate) fn check_baseline(
    playbook: &Playbook,
    ctx: &ValidationContext,
    r: &mut ValidationReport,
) {
    check_reserved_site(playbook, r);
    let Some(block) = &playbook.baseline_check else {
        return;
    };
    if block.checks.is_empty() {
        r.error("V62", None, "baseline_check.checks is empty".into());
        return;
    }
    let mut ids = HashSet::new();
    for c in &block.checks {
        if !ids.insert(c.id.as_str()) {
            r.error(
                "V62",
                None,
                format!("baseline check `{}` is declared twice", c.id),
            );
        }
        check_structure(c, r);
        check_source(c, ctx, r);
        check_observations(c, r);
        check_templates(playbook, c, r);
    }
    check_goal_pairing(playbook, r);
    check_tree(playbook, r);
}

/// V68: no node may take the reserved baseline grant site as its id.
fn check_reserved_site(playbook: &Playbook, r: &mut ValidationReport) {
    for n in playbook.nodes.iter().filter(|n| n.id == BASELINE_SITE) {
        r.error(
            "V68",
            Some(&n.id),
            format!("node id `{BASELINE_SITE}` is reserved for the baseline check"),
        );
    }
}

/// V62: id format, exactly one source, `observe` present exactly when the
/// source needs it, `complete_when` only on a connector, the timeout range,
/// unique observation ids, and the project check kinds.
fn check_structure(c: &BaselineCheck, r: &mut ValidationReport) {
    let at = format!("baseline check `{}`", c.id);
    if !id_ok(&c.id) {
        r.error(
            "V62",
            None,
            format!("{at}: the id must match [a-z0-9_]{{1,{ID_MAX}}}"),
        );
    }
    if c.source_count() != 1 {
        r.error(
            "V62",
            None,
            format!(
                "{at} must name exactly one source (connector, script or project_checks), found {}",
                c.source_count()
            ),
        );
    }
    match c.source() {
        Some(BaselineSource::ProjectChecks(spec)) => {
            if !c.observe.is_empty() {
                r.error(
                    "V62",
                    None,
                    format!("{at}: a project_checks source has fixed observations, drop `observe`"),
                );
            }
            for kind in &spec.only {
                if !crate::project_checks::KINDS.contains(&kind.as_str()) {
                    r.error(
                        "V62",
                        None,
                        format!(
                            "{at}: project_checks.only names `{kind}`; expected one of {}",
                            crate::project_checks::KINDS.join(", ")
                        ),
                    );
                }
            }
        }
        Some(src) if c.observe.is_empty() => r.error(
            "V62",
            None,
            format!(
                "{at}: a {} source needs at least one `observe` entry",
                src.kind()
            ),
        ),
        _ => {}
    }
    if c.complete_when.is_some() && !matches!(c.source(), Some(BaselineSource::Connector(_)) | None)
    {
        r.error(
            "V62",
            None,
            format!("{at}: `complete_when` applies only to a connector source; a script reports an incomplete read with `\"complete\": false`"),
        );
    }
    if c.timeout_seconds == 0 || c.timeout_seconds > MAX_TIMEOUT_SECONDS {
        r.error(
            "V62",
            None,
            format!("{at}: timeout_seconds must be 1 to {MAX_TIMEOUT_SECONDS}"),
        );
    }
    let mut seen = HashSet::new();
    for o in &c.observe {
        if !id_ok(&o.id) {
            r.error(
                "V62",
                None,
                format!(
                    "{at}: observation id `{}` must match [a-z0-9_]{{1,{ID_MAX}}}",
                    o.id
                ),
            );
        }
        if !seen.insert(o.id.as_str()) {
            r.error(
                "V62",
                None,
                format!("{at}: observation `{}` is declared twice", o.id),
            );
        }
    }
}

/// V63: a script stays under `scripts/`; a connector source, when the
/// installed connectors are known, names a declared read-only function
/// that is not an inbox function. The connector and function names are
/// checked once, with every other binding site (V23), and a connector
/// that does not load is reported there too (V42).
fn check_source(c: &BaselineCheck, ctx: &ValidationContext, r: &mut ValidationReport) {
    let at = format!("baseline check `{}`", c.id);
    match c.source() {
        Some(BaselineSource::Script(path)) if !nodes::script_check_path_ok(path) => r.error(
            "V63",
            None,
            format!("{at}: script `{path}` must be a relative path under scripts/"),
        ),
        Some(BaselineSource::Connector(conn)) => {
            let Some(facts) = ctx.connectors.get(&conn.name) else {
                return;
            };
            let f = &conn.function;
            let why = if facts.load_error.is_some() {
                return;
            } else if !facts.functions.contains(f) {
                "is not declared by the connector"
            } else if facts.inbox_functions.contains(f) {
                "is an inbox function"
            } else if facts.write_functions.contains(f) {
                "is not read-only"
            } else {
                return;
            };
            r.error(
                "V63",
                None,
                format!(
                    "{at}: connector `{}` function `{f}` {why}; a baseline read must not change anything",
                    conn.name
                ),
            );
        }
        _ => {}
    }
}

/// V64: each observation states exactly one rule that fits its type, a
/// finite bound, a valid path that maps over an array (`[]`) only for a
/// list or a count, and `count` only on a number; a connector's
/// `complete_when` path is valid and reaches one value (no `[]`).
fn check_observations(c: &BaselineCheck, r: &mut ValidationReport) {
    for o in &c.observe {
        let at = format!("baseline check `{}` observation `{}`", c.id, o.id);
        match o.expect.rule() {
            Ok(rule) => {
                if let Some(why) = rule.misfit(o.kind) {
                    r.error("V64", None, format!("{at}: {why}"));
                }
            }
            Err(why) => r.error("V64", None, format!("{at}: {why}")),
        }
        match crate::baseline::extract::parse_path(&o.path) {
            Err(why) => r.error("V64", None, format!("{at}: {why}")),
            Ok(segs)
                if segs.iter().any(|s| s.map)
                    && o.kind != crate::baseline::ObservationType::List
                    && !o.count =>
            {
                r.error(
                    "V64",
                    None,
                    format!(
                        "{at}: a path with `[]` reads a list; use `type: list` or `count: true`"
                    ),
                );
            }
            Ok(_) => {}
        }
        if o.count && o.kind != crate::baseline::ObservationType::Number {
            r.error(
                "V64",
                None,
                format!("{at}: `count` applies only to a number"),
            );
        }
    }
    if let Some(cw) = &c.complete_when {
        let why = match crate::baseline::extract::parse_path(&cw.path) {
            Err(why) => Some(why),
            Ok(segs) if segs.iter().any(|s| s.map) => Some(
                "a path with `[]` reads a list, and a read is complete only on one value"
                    .to_string(),
            ),
            Ok(_) => None,
        };
        if let Some(why) = why {
            r.error(
                "V64",
                None,
                format!("baseline check `{}` complete_when: {why}", c.id),
            );
        }
    }
}

/// Every string inside a JSON value.
fn strings<'a>(v: &'a serde_json::Value, out: &mut Vec<&'a str>) {
    match v {
        serde_json::Value::String(s) => out.push(s),
        serde_json::Value::Array(items) => items.iter().for_each(|i| strings(i, out)),
        serde_json::Value::Object(map) => map.values().for_each(|i| strings(i, out)),
        _ => {}
    }
}

/// V65: connector args and `complete_when` may template only
/// `params.<declared>`: both snapshots must read the same input, and run
/// state changes between them.
fn check_templates(playbook: &Playbook, c: &BaselineCheck, r: &mut ValidationReport) {
    let mut texts = Vec::new();
    if let Some(conn) = &c.connector {
        conn.args.values().for_each(|v| strings(v, &mut texts));
    }
    if let Some(cw) = &c.complete_when {
        texts.push(&cw.path);
        strings(&cw.equals, &mut texts);
    }
    for t in texts {
        for reference in crate::template::refs(t) {
            let declared = matches!(
                reference.split('.').collect::<Vec<_>>().as_slice(),
                ["params", p] if playbook.params.iter().any(|d| d.name == *p)
            );
            if !declared {
                r.error(
                    "V65",
                    None,
                    format!(
                        "baseline check `{}` templates `{{{{{reference}}}}}`; only a declared params.<name> is allowed, so both snapshots read the same input",
                        c.id
                    ),
                );
            }
        }
    }
}

/// V66 (warning): a baseline shows the change, not whether the goal was
/// reached; without a goal, or with only manual criteria, nothing checks
/// that.
fn check_goal_pairing(playbook: &Playbook, r: &mut ValidationReport) {
    let checked = playbook
        .goal
        .as_ref()
        .is_some_and(|g| g.criteria.iter().any(|c| c.check != GoalCheck::Manual));
    if !checked {
        r.warn(
            "V66",
            None,
            "a baseline shows the change, not whether the goal was reached: add a goal with a script or marker criterion".into(),
        );
    }
}

/// V67 (warning): a script or project_checks source runs in the run's
/// tree; when `worktree` reads a node output, the before snapshot runs in
/// the execution root and the after snapshot in the resolved tree.
fn check_tree(playbook: &Playbook, r: &mut ValidationReport) {
    let Some(node) = playbook.worktree_source_node() else {
        return;
    };
    for c in playbook.baseline_checks() {
        if matches!(
            c.source(),
            Some(BaselineSource::Script(_) | BaselineSource::ProjectChecks(_))
        ) {
            r.warn(
                "V67",
                None,
                format!(
                    "baseline check `{}` runs in the run's tree, but `worktree` is resolved from node `{node}`: the before and after snapshots would run in different trees",
                    c.id
                ),
            );
        }
    }
}
