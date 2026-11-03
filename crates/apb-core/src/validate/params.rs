//! Param rules (V99, V100): a secret param keeps its value out of the
//! definition and is plain text. Search rules (V160 to V162): a searchable
//! param is never secret and never named like a secret, and a playbook
//! marks at most [`crate::run_search::MAX_SEARCH_PARAMS`] of them.

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
