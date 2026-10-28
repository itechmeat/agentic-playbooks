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

use crate::baseline::BaselineSource;
use crate::schema::{Effect, Node, Playbook};

/// The full set an acting node is considered capable of.
const ACTING: [Effect; 4] = [
    Effect::FsRead,
    Effect::FsWrite,
    Effect::Network,
    Effect::External,
];
/// What reaching an external service over HTTP implies.
const REACHES_OUT: [Effect; 2] = [Effect::Network, Effect::External];

/// Whether executing `node` can change anything: it holds the workdir lock
/// (an agent, a script, a sub-playbook, a finish with a prompt; see
/// `NodeKind::takes_workdir_lock`), it binds a connector, or it declares an
/// effect other than `fs_read`. The baseline's before snapshot is taken
/// before the first such node, and [`inferred`] is built on it so the two
/// cannot drift.
pub fn node_acts(node: &Node) -> bool {
    node.kind.takes_workdir_lock()
        || !node.kind.connector_bindings().is_empty()
        || node.own_effects().iter().any(|e| *e != Effect::FsRead)
}

/// Effects conservatively derived from the definition. An acting node - one
/// that runs an agent, runs a script, or runs a sub-playbook in the shared
/// workdir - is considered capable of file writes, network access, and
/// external actions; a node that only binds a connector reaches an external
/// service over HTTP regardless of its kind (spec 12's automatic catalog
/// flag, which also feeds `preflight`). A node acting only through a
/// declared effect adds nothing here: its declaration joins in
/// [`effective`].
///
/// The baseline check's sources count too: a connector source reaches out,
/// a script or project-checks source runs a program in the tree like a
/// script node.
pub fn inferred(playbook: &Playbook) -> BTreeSet<Effect> {
    let mut set = BTreeSet::new();
    for n in playbook.nodes.iter().filter(|n| node_acts(n)) {
        if n.kind.takes_workdir_lock() {
            set.extend(ACTING);
        }
        if !n.kind.connector_bindings().is_empty() {
            set.extend(REACHES_OUT);
        }
        set.extend(n.kind.implied_effects().iter().copied());
    }
    for c in playbook.baseline_checks() {
        match c.source() {
            Some(BaselineSource::Connector(_)) => set.extend(REACHES_OUT),
            Some(BaselineSource::Script(_) | BaselineSource::ProjectChecks(_)) => {
                set.extend(ACTING)
            }
            None => {}
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
    // A node's own declaration (0.23.0) widens the same way.
    for n in &playbook.nodes {
        set.extend(n.own_effects());
    }
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
    fn bound_connector_implies_network_and_external() {
        let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: agent_task, prompt: hi, profile: x, connectors: [jira] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: done }
"#;
        let set = effective(&playbook(yaml));
        assert!(set.contains(&Effect::Network));
        assert!(set.contains(&Effect::External));
    }

    #[test]
    fn a_node_declaration_widens_the_effective_effects() {
        let yaml = SCRIPT_WF.replace(
            "script: \"scripts/x.sh\", runner: sh }",
            "script: \"scripts/x.sh\", runner: sh, effects: [irreversible] }",
        );
        let p = playbook(&yaml);
        assert_eq!(p.nodes[1].effects, [Effect::Irreversible]);
        assert!(effective(&p).contains(&Effect::Irreversible));
        assert!(!effective(&playbook(SCRIPT_WF)).contains(&Effect::Irreversible));
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
        assert!(playbook(yaml).nodes.iter().all(|n| !node_acts(n)));
    }

    #[test]
    fn a_node_acts_by_kind_binding_or_declared_effect() {
        let p = playbook(SCRIPT_WF);
        assert!(node_acts(p.node("work").unwrap()));
        assert!(!node_acts(p.node("start").unwrap()));
        let yaml = SCRIPT_WF.replace(
            "{ id: done, type: finish, outcome: success }",
            "{ id: done, type: finish, outcome: success, effects: [fs_read] }",
        );
        assert!(!node_acts(playbook(&yaml).node("done").unwrap()));
        let yaml = SCRIPT_WF.replace(
            "{ id: done, type: finish, outcome: success }",
            "{ id: done, type: finish, outcome: success, effects: [external] }",
        );
        assert!(node_acts(playbook(&yaml).node("done").unwrap()));
    }

    #[test]
    fn baseline_sources_widen_the_inferred_effects() {
        let control = r#"
schema: 2
id: c
name: C
version: 1.0.0
baseline_check:
  checks:
    - { id: deals, connector: { name: crm, function: count }, observe: [{ id: n, type: number, path: n, expect: { equals: 0 } }] }
nodes:
  - { id: start, type: start }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: done }
"#;
        let set = inferred(&playbook(control));
        assert!(set.contains(&Effect::Network) && set.contains(&Effect::External));
        assert!(!set.contains(&Effect::FsWrite));
        let tests = control.replace(
            "connector: { name: crm, function: count }, observe: [{ id: n, type: number, path: n, expect: { equals: 0 } }]",
            "project_checks: { only: [test] }",
        );
        assert!(inferred(&playbook(&tests)).contains(&Effect::FsWrite));
    }
}
