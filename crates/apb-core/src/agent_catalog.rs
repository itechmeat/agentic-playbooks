//! The one source of truth for agent and model lists (docs/PROFILES.md,
//! "Where the agent and model lists come from").
//!
//! Every consumer - the dashboard (`/api/agents`, `/api/models`), the CLI
//! (`apb detect`), the MCP tools (`agents_detect`, `profile_howto`), doctor
//! and adoption - gets its lists from [`load`] (or, where a broken overlay
//! must not stop it, [`agents`]). Both go through [`assemble`], a pure
//! function of:
//! - the models table (the data embedded from `assets/models.yaml` plus the
//!   user overlay), which owns the curated rows and the closed claude/codex
//!   lists, and apb's zcode allowlist; and
//! - the external probe results from [`crate::detect::probe`] (installed,
//!   version, `opencode models` output, auth/provider hints), which is the
//!   only part that may come from the detection memo.
//!
//! Nothing apb owns is ever read back from a cache: the closed lists and the
//! option lists are recomputed from the running binary's data on every call,
//! which costs microseconds.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::detect::{self, AgentInfo, Authority, ModelsInventory};
use crate::models_table::{self, ModelOption, ModelsError, ModelsTable};

/// Agents and their model lists, as every consumer sees them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Catalog {
    /// The merged models table (embedded data + user overlay).
    pub table: ModelsTable,
    /// Detected agents; an installed agent with a closed apb list carries it
    /// as its `Static` inventory.
    pub agents: Vec<AgentInfo>,
    /// Per-agent model options for profile authoring, keyed by agent id.
    pub options_by_agent: BTreeMap<String, Vec<ModelOption>>,
}

/// Combines the models table with raw probe results. Pure: the same table and
/// probes always give the same catalog.
///
/// An installed agent that has a closed apb list
/// ([`models_table::static_models_for_agent`]) reports that list as its
/// `Static` inventory; an agent that is not installed claims no list. Every
/// agent then gets its option list from
/// [`models_table::model_options_for_agent`], annotated by its inventory.
pub fn assemble(table: ModelsTable, probed: Vec<AgentInfo>) -> Catalog {
    let agents: Vec<AgentInfo> = probed
        .into_iter()
        .map(|mut a| {
            if a.installed
                && let Some(items) = models_table::static_models_for_agent(&a.agent, &table)
            {
                a.models = Some(ModelsInventory {
                    items,
                    authority: Authority::Static,
                });
            }
            a
        })
        .collect();
    let options_by_agent = agents
        .iter()
        .map(|a| {
            let detected = a
                .models
                .as_ref()
                .map(|m| m.items.clone())
                .unwrap_or_default();
            (
                a.agent.clone(),
                models_table::model_options_for_agent(&a.agent, &detected, &table),
            )
        })
        .collect();
    Catalog {
        table,
        agents,
        options_by_agent,
    }
}

/// The catalog for this machine. `refresh` re-probes the agents instead of
/// using the detection memo. Fails only when the user overlay is broken.
pub fn load(refresh: bool) -> Result<Catalog, ModelsError> {
    let table = models_table::load_merged()?;
    Ok(assemble(table, detect::probe(refresh)))
}

/// Just the agents, for callers that must keep working when the user overlay
/// is broken (doctor reports that separately): it falls back to the embedded
/// table. Otherwise identical to `load(refresh)?.agents`.
pub fn agents(refresh: bool) -> Vec<AgentInfo> {
    let table = models_table::load_merged().unwrap_or_else(|_| models_table::builtin());
    assemble(table, detect::probe(refresh)).agents
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::AgentCategory;

    fn probed(agent: &str, installed: bool, models: Option<Vec<&str>>) -> AgentInfo {
        AgentInfo {
            agent: agent.to_string(),
            installed,
            canonical_path: None,
            version: None,
            category: AgentCategory::Vendor,
            models: models.map(|m| ModelsInventory {
                items: m.into_iter().map(String::from).collect(),
                authority: Authority::Full,
            }),
            providers: None,
            auth: None,
            notes: Vec::new(),
        }
    }

    fn ids(c: &Catalog, agent: &str) -> Vec<String> {
        c.options_by_agent[agent]
            .iter()
            .map(|o| o.id.clone())
            .collect()
    }

    /// The whole point of the module: for every agent with a closed list the
    /// detected inventory and the offered options are the SAME list, taken
    /// from the table handed in - there is no second copy to go stale.
    #[test]
    fn closed_lists_are_the_inventory_and_the_options() {
        let table = models_table::builtin();
        let c = assemble(
            table.clone(),
            vec![
                probed("claude", true, None),
                probed("codex", true, None),
                probed("zcode", true, None),
            ],
        );
        for (agent, want) in [
            ("claude", table.claude_static_models.clone()),
            ("codex", table.codex_static_models.clone()),
            ("zcode", crate::zcode::model_list()),
        ] {
            let a = c.agents.iter().find(|a| a.agent == agent).unwrap();
            let inv = a.models.as_ref().expect("installed: the closed list");
            assert_eq!(inv.authority, Authority::Static);
            assert_eq!(inv.items, want, "{agent} inventory");
            assert_eq!(ids(&c, agent), want, "{agent} options");
            assert!(c.options_by_agent[agent].iter().all(|o| o.detected));
        }
    }

    /// A changed table (a rebuilt binary or an overlay) changes every output
    /// at once: nothing else in the catalog remembers the old list.
    #[test]
    fn a_different_table_gives_different_lists() {
        let mut table = models_table::builtin();
        table
            .claude_static_models
            .push("claude-dummy-test".to_string());
        let c = assemble(table, vec![probed("claude", true, None)]);
        assert!(ids(&c, "claude").contains(&"claude-dummy-test".to_string()));
        assert!(
            c.agents[0]
                .models
                .as_ref()
                .unwrap()
                .items
                .contains(&"claude-dummy-test".to_string())
        );
    }

    /// Only the probe's external facts survive: a stale list in the probe
    /// result for a closed-list agent (e.g. from an old memo) is replaced.
    #[test]
    fn a_stale_probed_list_for_a_closed_list_agent_is_replaced() {
        let table = models_table::builtin();
        let c = assemble(
            table.clone(),
            vec![probed("claude", true, Some(vec!["claude-opus-5"]))],
        );
        assert_eq!(ids(&c, "claude"), table.claude_static_models);
        assert_eq!(
            c.agents[0].models.as_ref().unwrap().items,
            table.claude_static_models
        );
    }

    /// Not installed: no inventory is claimed, but the options are still the
    /// closed list, so a profile can be authored before the agent is set up.
    #[test]
    fn an_absent_agent_claims_no_inventory_but_keeps_its_options() {
        let table = models_table::builtin();
        let c = assemble(table.clone(), vec![probed("codex", false, None)]);
        assert!(c.agents[0].models.is_none());
        assert_eq!(ids(&c, "codex"), table.codex_static_models);
        assert!(c.options_by_agent["codex"].iter().all(|o| !o.detected));
    }

    /// An aggregator keeps its live listing: the curated table plus every
    /// model the agent itself reported.
    #[test]
    fn an_aggregator_gets_the_table_plus_its_own_listing() {
        let table = models_table::builtin();
        let c = assemble(
            table.clone(),
            vec![probed("opencode", true, Some(vec!["opencode/big-pickle"]))],
        );
        let got = ids(&c, "opencode");
        assert_eq!(got.len(), table.models.len() + 1);
        assert_eq!(got.last().unwrap(), "opencode/big-pickle");
        assert_eq!(
            c.agents[0].models.as_ref().unwrap().items,
            vec!["opencode/big-pickle".to_string()]
        );
    }
}
