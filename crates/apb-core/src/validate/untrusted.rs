//! The untrusted-input rules (V179 to V182), over the taint analysis of
//! [`crate::untrusted`]: an agent that renders untrusted text and can
//! send outward is refused without an accepted reason (V179), one that
//! also reaches secrets warns (V180), a marker on the wrong kind is
//! refused (V181), and an acceptance of nothing warns (V182).

use super::*;
use crate::untrusted::{self, Source};

/// Runs the V179 to V182 rules, after the isolation check.
pub(crate) fn check_untrusted(playbook: &Playbook, r: &mut ValidationReport) {
    let analysis = untrusted::analyze(playbook);

    // V181: the markers on their kinds. A param may not be both secret
    // and untrusted; `untrusted: true` sits on `agent_task` and `script`
    // only; `accept_untrusted` sits on a follower kind with a real
    // reason.
    for p in &playbook.params {
        if p.secret && p.untrusted {
            r.error(
                "V181",
                None,
                format!(
                    "param `{}` is both secret and untrusted; a secret is the operator's own value",
                    p.name
                ),
            );
        }
    }
    for node in &playbook.nodes {
        if node.untrusted
            && !matches!(
                node.kind,
                NodeKind::AgentTask { .. } | NodeKind::Script { .. }
            )
        {
            r.error(
                "V181",
                Some(&node.id),
                "`untrusted` applies to agent_task and script nodes only".to_string(),
            );
        }
    }
    let accepts: Vec<&crate::schema::Node> = playbook
        .nodes
        .iter()
        .filter(|n| n.accept_untrusted.is_some())
        .collect();
    for node in &accepts {
        let follower_kind = matches!(
            node.kind,
            NodeKind::AgentTask { .. }
                | NodeKind::Playbook { .. }
                | NodeKind::Finish {
                    prompt: Some(_),
                    ..
                }
        );
        if !follower_kind {
            r.error(
                "V181",
                Some(&node.id),
                "accept_untrusted needs an agent that reads instructions".to_string(),
            );
            continue;
        }
        let reason = node.accept_untrusted.as_deref().unwrap_or("");
        let trimmed = reason.trim();
        if trimmed.is_empty() || reason.chars().count() > 300 {
            r.error(
                "V181",
                Some(&node.id),
                "accept_untrusted needs a reason of 1 to 300 characters".to_string(),
            );
        }
    }

    // V179 and V180: the followers of untrusted text.
    for f in &analysis.followers {
        let sources = f
            .sources
            .iter()
            .map(Source::label)
            .collect::<Vec<_>>()
            .join(", ");
        let can_send = !f.outward.is_empty();
        // An empty or overlong reason (V181) accepts nothing.
        let accepted = f
            .accepted
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty() && v.chars().count() <= 300);
        if can_send && !accepted {
            r.error(
                "V179",
                Some(&f.node),
                format!(
                    "node `{}` renders untrusted text ({sources}) and can send outward ({}); an instruction in that text could make it send what it can read; isolate it with `isolation: {{ level: full, network: none }}` and no connector, or accept the risk with `accept_untrusted: <reason>`",
                    f.node,
                    f.outward.join(", ")
                ),
            );
        }
        if !f.secrets.is_empty() {
            r.warn(
                "V180",
                Some(&f.node),
                format!(
                    "node `{}` renders untrusted text ({sources}) and reaches secrets ({})",
                    f.node,
                    f.secrets.join(", ")
                ),
            );
        }
    }

    // V182: an acceptance of nothing. A node that renders no untrusted
    // text, or one that renders it without any way to send outward, has
    // nothing to accept.
    for node in &accepts {
        let reason_ok = node
            .accept_untrusted
            .as_deref()
            .map(|v| !v.trim().is_empty() && v.chars().count() <= 300)
            .unwrap_or(false);
        if !reason_ok {
            // V181 already said why the acceptance is not usable.
            continue;
        }
        let renders = analysis
            .followers
            .iter()
            .any(|f| f.node == node.id && !f.outward.is_empty());
        if !renders {
            r.warn(
                "V182",
                Some(&node.id),
                format!(
                    "node `{}` accepts untrusted input but renders none it could send outward; remove accept_untrusted",
                    node.id
                ),
            );
        }
    }
}
