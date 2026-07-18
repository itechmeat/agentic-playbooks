//! Model of run effects (spec 8.5).
//!
//! What the author declares cannot be taken on faith: a playbook may declare
//! `fs_read` while actually writing files and reaching the network. Therefore:
//! - `inferred` - conservatively derived from node types (what the engine can
//!   definitely do);
//! - `effective` = `inferred ∪ declared` - this is what the run policy
//!   actually uses.
//!
//! Key invariant: a declaration can only WIDEN the set, never narrow it -
//! `effective` is built by union, so understating effects via a declaration is
//! structurally impossible.

use std::collections::BTreeSet;

use crate::schema::{Effect, Playbook};

/// Effects conservatively derived from the definition. An acting node - one
/// that runs an agent, runs a script, or runs a sub-playbook in the shared
/// workdir - is considered capable of file writes, network access, and
/// external actions. Membership is exactly `NodeKind::takes_workdir_lock`
/// (= `runs_agent() || Script || Playbook`): effects and the workdir-lock
/// predicate share one definition so they can never diverge (review I5/M3).
/// The compile-time "a new variant must be classified" guard now lives in
/// `NodeKind::runs_agent`, which `takes_workdir_lock` builds on.
pub fn inferred(playbook: &Playbook) -> BTreeSet<Effect> {
    let mut set = BTreeSet::new();
    for n in &playbook.nodes {
        if n.kind.takes_workdir_lock() {
            set.insert(Effect::FsRead);
            set.insert(Effect::FsWrite);
            set.insert(Effect::Network);
            set.insert(Effect::External);
        }
    }
    set
}

/// Effective effects = inferred ∪ declared (spec 8.5). A declaration only
/// adds (e.g. `secrets`/`irreversible`, which the engine cannot derive from
/// the node type), never removes.
pub fn effective(playbook: &Playbook) -> BTreeSet<Effect> {
    let mut set = inferred(playbook);
    set.extend(playbook.effects.iter().copied());
    set
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Playbook;

    fn playbook(yaml: &str) -> Playbook {
        Playbook::from_yaml(yaml).unwrap()
    }

    const SCRIPT_WF: &str = r#"
schema: 1
id: s
name: S
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/x.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

    #[test]
    fn script_and_agent_nodes_infer_pessimistic_effects() {
        let set = inferred(&playbook(SCRIPT_WF));
        assert!(set.contains(&Effect::FsRead));
        assert!(set.contains(&Effect::FsWrite));
        assert!(set.contains(&Effect::Network));
        assert!(set.contains(&Effect::External));
        assert!(!set.contains(&Effect::Secrets));
    }

    #[test]
    fn declared_cannot_narrow_inferred() {
        let yaml = SCRIPT_WF.replace("nodes:", "effects: [fs_read]\nnodes:");
        let set = effective(&playbook(&yaml));
        assert!(set.contains(&Effect::FsWrite));
        assert!(set.contains(&Effect::Network));
    }

    #[test]
    fn declared_can_widen_with_irreversible() {
        let yaml = SCRIPT_WF.replace("nodes:", "effects: [irreversible]\nnodes:");
        let set = effective(&playbook(&yaml));
        assert!(set.contains(&Effect::Irreversible));
    }

    #[test]
    fn control_only_playbook_has_no_effects() {
        let yaml = r#"
schema: 1
id: c
name: C
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hi" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;
        assert!(inferred(&playbook(yaml)).is_empty());
    }
}
