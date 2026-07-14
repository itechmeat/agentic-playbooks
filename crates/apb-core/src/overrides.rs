use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::schema::{NodeKind, Playbook};

/// Per-run modifications without creating a new version (spec 11): the same
/// playbook can be run with a different profile on a node. Playbook semantics
/// do not change; only the execution of this particular run changes - so no
/// new version is created and `current` does not move.
///
/// schema 2: the `executors` section no longer exists. Overriding happens two
/// ways: by changing the node's profile (`profile`), or via a run-local
/// ephemeral executor (`ephemeral_executor`) - the latter keeps the node
/// profile's SOUL and skills but replaces the chain with a single agent+model
/// invocation. The ephemeral record is per-node in the manifest (not deduped
/// by `<scope>/<name>`), and is marked `ephemeral`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RunOverrides {
    /// Per-node overrides: a different profile for a specific agent node.
    pub nodes: BTreeMap<String, NodeOverride>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NodeOverride {
    /// Choosing a different profile for the node (schema 2, spec 10-11).
    #[serde(default)]
    pub profile: Option<crate::profile::QualifiedProfileRef>,
    /// Run-local ephemeral executor: SOUL and skills are taken from the
    /// node's profile, and the chain is replaced with a single invocation of
    /// this agent+model.
    #[serde(default)]
    pub ephemeral_executor: Option<EphemeralExecutor>,
}

/// Ad-hoc run executor: only an agent+model pair (no fallbacks, no own
/// SOUL/skills - those are inherited from the node's profile).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EphemeralExecutor {
    pub agent: String,
    pub model: String,
}

impl RunOverrides {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Parses overrides from YAML (the `nodes` section at the top level of the file).
    pub fn from_yaml(yaml: &str) -> Result<Self, String> {
        serde_yaml_ng::from_str(yaml).map_err(|e| e.to_string())
    }

    /// Applies overrides to the playbook, producing the "effective playbook" -
    /// the single definition the engine works with from then on. Empty
    /// overrides leave the playbook identical to the version. Every
    /// node-override must point to an existing agent_task node: an unknown id
    /// or an incompatible node type is an error (not a silent skip, otherwise
    /// a typo in the id would silently fail to apply).
    pub fn apply(&self, playbook: &mut Playbook) -> Result<(), String> {
        for (id, no) in &self.nodes {
            let node = playbook
                .nodes
                .iter_mut()
                .find(|n| &n.id == id)
                .ok_or_else(|| format!("override references unknown node `{id}`"))?;
            match &mut node.kind {
                NodeKind::AgentTask { profile, .. } => {
                    if no.profile.is_some() {
                        *profile = no.profile.clone();
                    }
                }
                _ => return Err(format!("override target `{id}` is not an agent_task node")),
            }
        }
        Ok(())
    }
}
