//! The review gate's recommendation (issue #165 Part 11, Part 14.4): the
//! drive-side facts the recommendation needs (the gate's options and
//! descriptions, the predecessors' outputs, the visit, the automatic-decision
//! refusal), handed to `decision::review_triage`.

use super::*;

use crate::decision::review_triage::{self, Recommendation};

/// The declared effects of every sub-playbook the playbook runs, loaded from
/// the project's registry (best effort: a child that does not load adds
/// nothing, and the playbook's own declaration still counts).
fn inherited_effects(root: &Path, playbook: &Playbook) -> Vec<apb_core::schema::Effect> {
    let Ok(reg) = apb_core::registry::Registry::open(root) else {
        return Vec::new();
    };
    playbook
        .nodes
        .iter()
        .filter_map(|n| match &n.kind {
            NodeKind::Playbook { playbook, .. } => reg.load(&playbook.id, None).ok(),
            _ => None,
        })
        .flat_map(|l| l.playbook.effects.clone())
        .collect()
}

/// Asks for the recommendation of one gate visit.
pub(crate) fn recommend(
    root: &Path,
    runner: &crate::decision::DecisionRunner,
    journal: &Journal,
    playbook: &Playbook,
    gate: &str,
    events: &[Event],
) -> Recommendation {
    let Some(node) = playbook.node(gate) else {
        return Recommendation::default();
    };
    let NodeKind::HumanReview { options, prompt } = &node.kind else {
        return Recommendation::default();
    };
    let options = apb_core::schema::effective_review_options(options);
    // Direct predecessors that ran: their latest output.
    let mut inputs: Vec<(String, String)> = Vec::new();
    for e in playbook.edges.iter().filter(|e| e.to == gate) {
        if inputs.iter().any(|(id, _)| *id == e.from) {
            continue;
        }
        let out = events.iter().rev().find_map(|ev| match &ev.payload {
            EventPayload::NodeFinished { node, output, .. } if *node == e.from => {
                Some(output.clone())
            }
            _ => None,
        });
        if let Some(out) = out {
            inputs.push((e.from.clone(), out));
        }
    }
    let visit = crate::event::review_requested_count(events, gate) as u32 + 1;
    let refusal = node.auto_decide.as_ref().and_then(|_| {
        apb_core::validate::auto_decide_refusal(playbook, gate, &inherited_effects(root, playbook))
            .map(|_| "effects")
    });
    review_triage::recommend(
        runner,
        journal,
        review_triage::Gate {
            node: gate,
            title: node.title.as_deref(),
            prompt: prompt.as_deref(),
            options: &options,
            descriptions: &node.option_descriptions,
            inputs,
            visit,
            auto_decide: node.auto_decide.as_ref(),
            refusal,
        },
    )
}
