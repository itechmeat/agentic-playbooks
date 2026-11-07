//! The `run.previous_success` rules (V176): a malformed reference, an
//! unknown node, a read in the run `worktree` template, and a `when_none`
//! value that is neither an RFC 3339 time nor a template over declared
//! non-secret `params.*` only.

use super::*;
use crate::previous_success::{PrevRef, parse};

/// Runs the V176 rules, next to `check_templates`.
pub(crate) fn check_previous_success(playbook: &Playbook, r: &mut ValidationReport) {
    // References in prompts, judge state fields, workdir and cache key
    // templates: `parse` owns the grammar, V176 reports what it rejects
    // and checks the named node exists.
    let nodes: HashSet<&str> = playbook.nodes.iter().map(|n| n.id.as_str()).collect();
    let texts = templates::template_texts(playbook)
        .into_iter()
        .chain(templates::workdir_texts(playbook))
        .chain(templates::cache_key_texts(playbook));
    for (owner, text) in texts {
        for cap in crate::template::refs(text) {
            match parse(&cap) {
                Some(Ok(PrevRef::Output { node, .. })) if !nodes.contains(node.as_str()) => {
                    r.error(
                        "V176",
                        Some(owner),
                        format!("template `{{{{{cap}}}}}`: node `{node}` is not in this playbook"),
                    );
                }
                Some(Err(why)) => {
                    r.error(
                        "V176",
                        Some(owner),
                        format!("template `{{{{{cap}}}}}`: {why}; known: id, started_at, finished_at, nodes.<id>.output[.<field>]"),
                    );
                }
                _ => {}
            }
        }
    }
    worktree_rule(playbook, r);
    when_none_rule(playbook, r);
}

/// The run's `worktree` template may not read the namespace: it renders
/// before the lookup runs.
fn worktree_rule(playbook: &Playbook, r: &mut ValidationReport) {
    if let Some(w) = playbook.worktree_template()
        && crate::template::refs(w)
            .iter()
            .any(|cap| parse(cap).is_some())
    {
        r.error(
            "V176",
            None,
            "the run's worktree template reads `run.previous_success`, which is read after the worktree is resolved".to_string(),
        );
    }
}

/// The `when_none` fields of the playbook's block, if it has one.
fn when_none_fields(playbook: &Playbook) -> Vec<(&'static str, &str)> {
    match playbook
        .previous_success
        .as_ref()
        .and_then(|b| b.when_none.as_ref())
    {
        Some(wn) => [
            wn.started_at.as_deref().map(|v| ("started_at", v)),
            wn.finished_at.as_deref().map(|v| ("finished_at", v)),
        ]
        .into_iter()
        .flatten()
        .collect(),
        None => Vec::new(),
    }
}

/// A `when_none` time must be a literal RFC 3339 time or a template over
/// declared, non-secret params; the rendered time is journaled on
/// `run_provenance`, so a secret value may not reach it.
fn when_none_rule(playbook: &Playbook, r: &mut ValidationReport) {
    let params: HashMap<&str, bool> = playbook
        .params
        .iter()
        .map(|p| (p.name.as_str(), p.secret))
        .collect();
    for (field, value) in when_none_fields(playbook) {
        // A literal time is fine, whatever else it holds.
        if crate::duration::parse_rfc3339_ms(value.trim()).is_some() {
            continue;
        }
        let mut bad: Option<String> = None;
        let mut secret: Option<String> = None;
        for cap in crate::template::refs(value) {
            match cap.split('.').collect::<Vec<&str>>().as_slice() {
                ["params", name] if params.contains_key(name) => {
                    if params.get(name) == Some(&true) {
                        secret = Some(name.to_string());
                    }
                }
                _ => bad = Some(cap.clone()),
            }
        }
        if let Some(cap) = bad {
            r.error(
                "V176",
                None,
                format!("previous_success.when_none.{field} reads `{{{{{cap}}}}}`; only params.* may be read there"),
            );
        } else if let Some(name) = secret {
            r.error(
                "V176",
                None,
                format!("previous_success.when_none.{field} reads secret param `{name}`; its value would be written to the journal"),
            );
        } else if crate::template::refs(value).is_empty() {
            r.error(
                "V176",
                None,
                format!("previous_success.when_none.{field} `{value}` is neither an RFC 3339 time nor a template over params"),
            );
        }
    }
}
