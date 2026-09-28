//! The review gate's recommendation (issue #165 Part 11, Part 14.4): the
//! drive-side facts the recommendation needs (the gate's options and
//! descriptions, the predecessors' outputs, the visit, the automatic-decision
//! refusal), handed to `decision::review_triage`.

use super::*;

use crate::decision::review_triage::{self, Recommendation};

/// The effects of every sub-playbook the playbook runs, recursively and in
/// any scope, resolved as the run gate resolves them. `None` when the tree
/// does not resolve: the automatic decision is then refused (fail-closed).
fn inherited_effects(
    root: &Path,
    run_dir: &Path,
    playbook: &Playbook,
) -> Option<Vec<apb_core::schema::Effect>> {
    let origin = node::parent_run_origin(run_dir);
    crate::gate::tree_effects(root, playbook, &origin).map(|set| set.into_iter().collect())
}

/// Asks for the recommendation of one gate visit.
pub(crate) fn recommend(
    root: &Path,
    run_dir: &Path,
    runner: &crate::decision::DecisionRunner,
    journal: &Journal,
    playbook: &Playbook,
    gate: &str,
    events: &[Event],
    child_run: bool,
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
    // A sub-playbook run cannot see what its parent does after it returns
    // (a push, a deploy, declared effects), so it never decides by itself.
    let refusal = node.auto_decide.as_ref().and_then(|_| {
        if child_run {
            return Some("effects");
        }
        match inherited_effects(root, run_dir, playbook) {
            Some(inherited) => apb_core::validate::auto_decide_refusal(playbook, gate, &inherited)
                .map(|_| "effects"),
            None => Some("effects"),
        }
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
