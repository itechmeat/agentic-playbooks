//! Param rules (V99, V100): a secret param keeps its value out of the
//! definition and is plain text.

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
}
