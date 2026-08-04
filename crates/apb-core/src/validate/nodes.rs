//! Per-node rules: the fields a node kind may carry, what a finish node must
//! declare, and the shapes an agent_task's optional gates are allowed to take.

use super::*;

/// V17: structured trigger fields (spec 8.5) are machine-facing and compact.
/// Limits: at most 5 lines per field, each line <= 120 characters. Otherwise
/// the field starts carrying free-form text, which is unsafe to display and
/// match against.
pub(crate) const TRIGGER_MAX_ITEMS: usize = 5;

pub(crate) const TRIGGER_MAX_LEN: usize = 120;

pub(crate) fn check_trigger(playbook: &Playbook, r: &mut ValidationReport) {
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
pub(crate) fn check_isolation(playbook: &Playbook, r: &mut ValidationReport) {
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
pub(crate) fn check_expected_duration(playbook: &Playbook, r: &mut ValidationReport) {
    for n in &playbook.nodes {
        match &n.expected_duration {
            Some(ed) if ed.parsed().is_none() => {
                r.error(
                    "V20",
                    Some(&n.id),
                    format!(
                        "node `{}` has an unparsable expected_duration; use seconds like `90`, a single unit like `30s`, `5m`, `2h`, or a descending compound like `1h30m`",
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
pub(crate) fn check_finish(playbook: &Playbook, r: &mut ValidationReport) {
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
pub(crate) fn check_playbook_ref(playbook: &Playbook, r: &mut ValidationReport) {
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

/// V27 (error): `cache: auto` on a node kind the engine never caches - only
/// `agent_task` and `script` execute deterministically enough for a cached
/// result to be reused. V28 (warning): a `ttl` set while the cache mode is
/// `off`; the ttl can never take effect while caching stays disabled. V29
/// (error): an `inputs.files` or `outputs.files` entry that is not a valid
/// glob pattern.
pub(crate) fn check_cache(playbook: &Playbook, r: &mut ValidationReport) {
    for node in &playbook.nodes {
        let cacheable = matches!(
            node.kind,
            NodeKind::AgentTask { .. } | NodeKind::Script { .. }
        );
        if node.cache_mode() == CacheMode::Auto && !cacheable {
            r.error(
                "V27",
                Some(&node.id),
                format!(
                    "node `{}` sets cache: auto but only agent_task and script nodes are cached",
                    node.id
                ),
            );
        }
        if let Some(CacheSpec::Config(c)) = &node.cache
            && c.ttl.is_some()
            && c.mode == CacheMode::Off
        {
            r.warn(
                "V28",
                Some(&node.id),
                format!(
                    "node `{}` sets a cache ttl but cache mode is off; the ttl has no effect",
                    node.id
                ),
            );
        }
        for nf in [&node.inputs, &node.outputs].into_iter().flatten() {
            if let Err(bad) = build_globset(&nf.files) {
                r.error(
                    "V29",
                    Some(&node.id),
                    format!(
                        "node `{}` has an invalid glob `{bad}` in inputs/outputs files",
                        node.id
                    ),
                );
            }
        }
    }
}

/// V31/V32: interactive-node companion fields (spec 2026-07-20). Only
/// `agent_task` carries `interactive`/`answer_by`/`question_timeout_seconds`/
/// `default_answer`, so the node-kind guard is implicit: a non-agent_task node
/// can never set them.
pub(crate) fn check_interactive(playbook: &Playbook, r: &mut ValidationReport) {
    for n in &playbook.nodes {
        if let NodeKind::AgentTask {
            interactive,
            answer_by,
            question_timeout_seconds,
            default_answer,
            ..
        } = &n.kind
        {
            let has_companion = !answer_by.is_default()
                || question_timeout_seconds.is_some()
                || default_answer.is_some();
            if !interactive && has_companion {
                r.error(
                    "V31",
                    Some(&n.id),
                    "interactive companion fields (`answer_by`, `question_timeout_seconds`, `default_answer`) require `interactive: true`".to_string(),
                );
            }
            if default_answer.is_some() && question_timeout_seconds.is_none() {
                r.error(
                    "V32",
                    Some(&n.id),
                    "`default_answer` requires `question_timeout_seconds` (it is the answer used when the timeout elapses)".to_string(),
                );
            }
        }
    }
}

/// V33: a `success_check` is a post-agent gate that only an `agent_task` node
/// runs (the engine enforces it in the agent-attempt path). Declaring it on any
/// other node kind is a no-op that silently misleads the author, so it is an
/// error. A completion-marker check with an empty (or whitespace-only) marker
/// is also an error: the engine tests for the literal marker in the output, and
/// an empty marker would match every non-empty output, defeating the check.
pub(crate) fn check_success_check(playbook: &Playbook, r: &mut ValidationReport) {
    for n in &playbook.nodes {
        let Some(sc) = n.success_check.as_ref() else {
            continue;
        };
        if !matches!(n.kind, NodeKind::AgentTask { .. }) {
            r.error(
                "V33",
                Some(&n.id),
                "success_check is only valid on an agent_task node".to_string(),
            );
            continue;
        }
        if let SuccessCheck::Marker { marker } = sc
            && marker.trim().is_empty()
        {
            r.error(
                "V33",
                Some(&n.id),
                "success_check marker must not be empty".to_string(),
            );
        }
    }
}

pub(crate) fn check_scripts(playbook: &Playbook, r: &mut ValidationReport) {
    let escapes =
        |script: &str| script.starts_with('/') || script.split('/').any(|seg| seg == "..");
    for n in &playbook.nodes {
        if let NodeKind::Script { script, .. } = &n.kind
            && escapes(script)
        {
            r.error(
                "V12",
                Some(&n.id),
                format!("script path `{script}` must stay inside the version directory"),
            );
        }
        if let Some(script) = n.success_check.as_ref().and_then(SuccessCheck::script_path)
            && (escapes(script) || !script.starts_with("scripts/"))
        {
            r.error("V12", Some(&n.id),
                format!("success_check path `{script}` must live under `scripts/` inside the version directory"));
        }
    }
}
