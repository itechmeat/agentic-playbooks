//! The features of a definition an older executor would silently drop.
//!
//! An apb that does not know `approves`, `requires_approval` or
//! `person_only` drops them on load and would decide and run the gate as a
//! plain one; one that does not know `evidence` would run the node with no
//! reading session and no check of its claims. A run whose definition uses any of them carries a `run_guard`
//! naming them after every checkpoint, so such an apb refuses to continue
//! it. A later release adds names here instead of a new guard.

use serde::{Deserialize, Serialize};

use crate::schema::Playbook;

/// One feature a `run_guard` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardedFeature {
    /// A gate's request is bound to result snapshots (`approves`).
    ApprovalBinding,
    /// A gate only a person's channel decides (`person_only`).
    PersonOnly,
    /// A node needs a gate's approval before it starts
    /// (`requires_approval`).
    RequiredApproval,
    /// A node reads its sources through the evidence reader and its claims
    /// are checked (`evidence`).
    Evidence,
}

impl GuardedFeature {
    /// Every feature this binary knows, in declaration order.
    pub const ALL: [GuardedFeature; 4] = [
        GuardedFeature::ApprovalBinding,
        GuardedFeature::PersonOnly,
        GuardedFeature::RequiredApproval,
        GuardedFeature::Evidence,
    ];

    /// The name a `run_guard` carries.
    pub fn as_str(self) -> &'static str {
        match self {
            GuardedFeature::ApprovalBinding => "approval_binding",
            GuardedFeature::PersonOnly => "person_only",
            GuardedFeature::RequiredApproval => "required_approval",
            GuardedFeature::Evidence => "evidence",
        }
    }

    /// The feature a `run_guard` name stands for; `None` for a name a newer
    /// apb wrote, which refuses this binary.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.as_str() == name)
    }
}

/// The guarded features `playbook` uses, sorted and without repeats; empty
/// when a run of it needs no guard.
pub fn guarded_features(playbook: &Playbook) -> Vec<GuardedFeature> {
    let mut used = std::collections::BTreeSet::new();
    for node in &playbook.nodes {
        if node.approves.is_some() {
            used.insert(GuardedFeature::ApprovalBinding);
        }
        if node.person_only {
            used.insert(GuardedFeature::PersonOnly);
        }
        if node.requires_approval.is_some() {
            used.insert(GuardedFeature::RequiredApproval);
        }
        if node.evidence.is_some() {
            used.insert(GuardedFeature::Evidence);
        }
    }
    used.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playbook(nodes: &str) -> Playbook {
        Playbook::from_yaml(&format!(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n{nodes}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n"
        ))
        .unwrap()
    }

    /// Each declaration names its own feature, a definition using several
    /// names each once in declaration order, and a definition using none
    /// needs no guard.
    #[test]
    fn each_declaration_names_its_feature_once_in_order() {
        use GuardedFeature::*;
        let work = "  - { id: w, type: script, script: s, runner: sh }\n";
        let cases: [(String, Vec<GuardedFeature>); 6] = [
            (
                format!("{work}  - {{ id: g, type: human_review }}\n"),
                vec![],
            ),
            (
                format!(
                    "{work}  - {{ id: g, type: human_review, approves: {{ results: [w] }} }}\n"
                ),
                vec![ApprovalBinding],
            ),
            (
                "  - { id: g, type: human_review, person_only: true }\n".into(),
                vec![PersonOnly],
            ),
            (
                format!(
                    "{work}  - {{ id: g, type: human_review }}\n  - {{ id: x, type: script, script: s, runner: sh, requires_approval: {{ gate: g }} }}\n"
                ),
                vec![RequiredApproval],
            ),
            (
                format!(
                    "{work}  - {{ id: u, type: agent_task, prompt: p, evidence: {{ from: [w] }} }}\n"
                ),
                vec![Evidence],
            ),
            (
                format!(
                    "{work}  - {{ id: x, type: script, script: s, runner: sh, requires_approval: {{ gate: g2 }} }}\n  - {{ id: g, type: human_review, person_only: true, approves: {{ results: [w] }} }}\n  - {{ id: g2, type: human_review, person_only: true, approves: {{ results: [w] }} }}\n"
                ),
                vec![ApprovalBinding, PersonOnly, RequiredApproval],
            ),
        ];
        for (nodes, want) in cases {
            assert_eq!(guarded_features(&playbook(&nodes)), want, "{nodes}");
        }
    }

    #[test]
    fn a_name_parses_back_to_its_feature_and_a_newer_name_to_none() {
        for f in GuardedFeature::ALL {
            assert_eq!(GuardedFeature::parse(f.as_str()), Some(f));
        }
        assert_eq!(GuardedFeature::parse("a_future_feature"), None);
    }
}
