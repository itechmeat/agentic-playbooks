//! Param rules (V99, V100): a secret param keeps its value out of the
//! definition and is plain text. Search rules (V160 to V162): a searchable
//! param is never secret and never named like a secret, and a playbook
//! marks at most [`crate::run_search::MAX_SEARCH_PARAMS`] of them.
//! Pattern rules (V177, V178): a text param's `pattern` compiles and is a
//! bounded size, and the param's own default matches it.

use super::*;

/// Runs every param rule.
pub(crate) fn check_params(playbook: &Playbook, r: &mut ValidationReport) {
    for p in playbook.params.iter().filter(|p| p.secret) {
        if p.default.is_some() {
            r.error(
                "V99",
                None,
                format!(
                    "param `{}` is secret but has a default: the secret would be stored in the playbook; remove the default",
                    p.name
                ),
            );
        }
        if p.kind != "text" {
            r.error(
                "V100",
                None,
                format!(
                    "param `{}` is secret, so it must be `type: text`, not `{}`",
                    p.name, p.kind
                ),
            );
        }
    }
    check_search(playbook, r);
    check_patterns(playbook, r);
}

/// V177 and V178: the shape of a text param's `pattern` and its default.
fn check_patterns(playbook: &Playbook, r: &mut ValidationReport) {
    for p in &playbook.params {
        let Some(pattern) = &p.pattern else {
            continue;
        };
        if p.kind != "text" {
            r.error(
                "V177",
                None,
                format!(
                    "param `{}` has a pattern but is `type: {}`; a pattern applies to text params only",
                    p.name, p.kind
                ),
            );
            continue;
        }
        if pattern.is_empty() {
            r.error(
                "V177",
                None,
                format!("param `{}`: the pattern is empty", p.name),
            );
            continue;
        }
        if let Err(why) = crate::param_pattern::compile(pattern) {
            r.error(
                "V177",
                None,
                format!("param `{}`: pattern does not compile: {why}", p.name),
            );
            continue;
        }
        if let Some(default) = p.default_text()
            && crate::param_pattern::check(p, &default).is_err()
        {
            r.error(
                "V178",
                None,
                format!(
                    "param `{}`: default does not match its pattern `{pattern}`",
                    p.name
                ),
            );
        }
    }
}

/// V160 to V162: what a run search may list.
fn check_search(playbook: &Playbook, r: &mut ValidationReport) {
    let searchable: Vec<&crate::schema::Param> =
        playbook.params.iter().filter(|p| p.search).collect();
    for p in &searchable {
        if p.secret {
            r.error(
                "V160",
                None,
                format!(
                    "param `{}` is secret, so it cannot be `search: true`: a run listing would show its value; remove `search`",
                    p.name
                ),
            );
        } else if crate::token_shape::name_looks_secret(&p.name) {
            r.error(
                "V161",
                None,
                format!(
                    "param `{}` is named like a secret, so it cannot be `search: true`: a run listing would show its value; rename it or remove `search`",
                    p.name
                ),
            );
        }
    }
    let max = crate::run_search::MAX_SEARCH_PARAMS;
    if searchable.len() > max {
        r.error(
            "V162",
            None,
            format!(
                "{} params are `search: true`; at most {max} may be",
                searchable.len()
            ),
        );
    }
}
