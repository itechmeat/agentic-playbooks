//! The features of a definition an older executor would silently drop.
//!
//! An apb that does not know `approves`, `requires_approval` or
//! `person_only` drops them on load and would decide and run the gate as a
//! plain one; one that does not know `evidence` would run the node without
//! its reading session and leave its claims unchecked; one that does not
//! know `run.previous_success` drops the block and would render the
//! namespace empty and re-read the whole window; one that does not know
//! the untrusted markers drops them and would run the node without the
//! prompt section. A run whose definition uses any of them carries a `run_guard`
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
    /// A node reads through an evidence session and its claims are checked
    /// (`evidence`).
    Evidence,
    /// An attempt's writes are scoped (`writes`, `max_changed_files`); an
    /// older apb drops the keys and runs the node unchecked.
    WriteScope,
    /// The definition reads `run.previous_success.*`; an older apb drops
    /// the block and would render the namespace empty and re-read the
    /// whole window.
    PreviousSuccess,
    /// A node renders untrusted text (`untrusted` markers); an older apb
    /// drops the markers and would run the node without the prompt
    /// section.
    UntrustedInput,
}

impl GuardedFeature {
    /// Every feature this binary knows, in declaration order.
    pub const ALL: [GuardedFeature; 7] = [
        GuardedFeature::ApprovalBinding,
        GuardedFeature::PersonOnly,
        GuardedFeature::RequiredApproval,
        GuardedFeature::Evidence,
        GuardedFeature::WriteScope,
        GuardedFeature::PreviousSuccess,
        GuardedFeature::UntrustedInput,
    ];

    /// The name a `run_guard` carries.
    pub fn as_str(self) -> &'static str {
        match self {
            GuardedFeature::ApprovalBinding => "approval_binding",
            GuardedFeature::PersonOnly => "person_only",
            GuardedFeature::RequiredApproval => "required_approval",
            GuardedFeature::Evidence => "evidence",
            GuardedFeature::WriteScope => "write_scope",
            GuardedFeature::PreviousSuccess => "previous_success",
            GuardedFeature::UntrustedInput => "untrusted_input",
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
        if node.writes.is_some() || node.max_changed_files.is_some() {
            used.insert(GuardedFeature::WriteScope);
        }
    }
    // The namespace and the untrusted markers are per-playbook checks: a
    // feature is in force when the definition reads or marks anything,
    // whatever node carries it.
    if crate::previous_success::reads(playbook) {
        used.insert(GuardedFeature::PreviousSuccess);
    }
    if !crate::untrusted::analyze(playbook).followers.is_empty() {
        used.insert(GuardedFeature::UntrustedInput);
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
        let cases: [(String, Vec<GuardedFeature>); 9] = [
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
                    "{work}  - {{ id: x, type: script, script: s, runner: sh, requires_approval: {{ gate: g2 }} }}\n  - {{ id: g, type: human_review, person_only: true, approves: {{ results: [w] }} }}\n  - {{ id: g2, type: human_review, person_only: true, approves: {{ results: [w] }} }}\n"
                ),
                vec![ApprovalBinding, PersonOnly, RequiredApproval],
            ),
            (
                format!(
                    "{work}  - {{ id: e, type: agent_task, prompt: p, evidence: {{ from: [w] }} }}\n"
                ),
                vec![Evidence],
            ),
            (
                format!(
                    "{work}  - {{ id: w2, type: script, script: s, runner: sh, writes: [\"src/**\"] }}\n"
                ),
                vec![WriteScope],
            ),
            (
                format!(
                    "{work}  - {{ id: p, type: agent_task, prompt: \"{{{{run.previous_success.id}}}}\" }}\n"
                ),
                vec![PreviousSuccess],
            ),
            (
                format!(
                    "{work}  - {{ id: u, type: agent_task, prompt: q, connectors: [{{ name: gh, untrusted: true }}] }}\n"
                ),
                vec![UntrustedInput],
            ),
        ];
        for (nodes, want) in cases {
            assert_eq!(guarded_features(&playbook(&nodes)), want, "{nodes}");
        }
    }

    /// A playbook with a follower that renders untrusted text lists
    /// `untrusted_input`; markers that reach no follower list none; a
    /// name round-trips.
    #[test]
    fn a_read_of_the_namespace_is_a_guarded_feature_in_order() {
        use GuardedFeature::*;
        let reader = "  - { id: p, type: agent_task, prompt: \"{{run.previous_success.id}}\" }\n";
        assert_eq!(guarded_features(&playbook(reader)), vec![PreviousSuccess]);
        let plain = "  - { id: q, type: agent_task, prompt: p }\n";
        assert_eq!(
            guarded_features(&playbook(plain)),
            Vec::<GuardedFeature>::new()
        );
        let both = "  - { id: w, type: script, script: s, runner: sh, writes: [\"src/**\"] }\n  - { id: p, type: agent_task, prompt: \"{{run.previous_success.id}}\" }\n";
        assert_eq!(
            guarded_features(&playbook(both)),
            vec![WriteScope, PreviousSuccess]
        );
    }

    #[test]
    fn a_name_parses_back_to_its_feature_and_a_newer_name_to_none() {
        for f in GuardedFeature::ALL {
            assert_eq!(GuardedFeature::parse(f.as_str()), Some(f));
        }
        assert_eq!(GuardedFeature::parse("evidence_v2"), None);
    }
}

#[test]
fn untrusted_markers_that_reach_a_follower_are_a_guarded_feature() {
    use GuardedFeature::*;
    let yaml = "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: dev }\nparams:\n  - { name: body, type: text, untrusted: true }\nnodes:\n  - { id: start, type: start }\n  - { id: u, type: agent_task, prompt: \"x {{params.body}}\" }\n  - { id: m, type: agent_task, prompt: q, connectors: [{ name: gh, untrusted: true }] }\n  - { id: quiet, type: agent_task, prompt: quiet }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: u }\n  - { from: u, to: m }\n  - { from: m, to: quiet }\n  - { from: quiet, to: done }\n";
    let pb = Playbook::from_yaml(yaml).unwrap();
    // `u` follows the untrusted param; `quiet` reads nothing untrusted.
    assert_eq!(guarded_features(&pb), vec![UntrustedInput]);
    assert_eq!(
        GuardedFeature::parse("untrusted_input"),
        Some(UntrustedInput)
    );
}
