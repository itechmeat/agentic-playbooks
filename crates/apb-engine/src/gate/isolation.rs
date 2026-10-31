//! The isolation part of the run gate: every isolated node's contract on
//! this machine (`apb_core::isolation::contract`), before anything of the
//! run is written. `check_run` refuses a `full` node this machine cannot
//! isolate with the structured `isolation_unavailable`; prepare checks again
//! with the run's real execution mode, so no start path skips it; the fit
//! report and the doctor read the same contracts.

use std::path::Path;

use apb_core::isolation::Level;
use apb_core::isolation::contract::{self, Contract, NodeFacts, Reason};
use apb_core::profile_store::PlaybookOrigin;
use apb_core::schema::Playbook;
use serde_json::{Value, json};

/// One isolated node and its contract on this machine.
#[derive(Debug, Clone)]
pub struct NodeContract {
    pub node: String,
    pub level: Level,
    pub contract: Contract,
}

/// The contract of every isolated node of `playbook`. The agents of a
/// node's chain come from its profile; a profile that does not resolve is
/// left to the rest of the gate. `host_mode`: the run's agent steps are host
/// tasks.
pub fn node_contracts(
    root: &Path,
    playbook: &Playbook,
    origin: PlaybookOrigin,
    host_mode: bool,
) -> Vec<NodeContract> {
    // The probe runs a trial sandbox once per process: only a playbook
    // with an isolated node pays for it.
    if !playbook.nodes.iter().any(|n| n.kind.is_isolated()) {
        return Vec::new();
    }
    let host = crate::sandbox::probe::host_facts();
    playbook
        .nodes
        .iter()
        .filter_map(|n| {
            let spec = n.kind.isolation().filter(|s| s.is_isolated())?;
            let agents = chain_agents(root, playbook, &n.kind, origin);
            let facts = NodeFacts {
                level: spec.level,
                network: spec.network(),
                agents_without_needs: agents
                    .into_iter()
                    .filter(|a| {
                        apb_core::agents::lookup(a)
                            .and_then(|x| x.setup().sandbox_needs())
                            .is_none()
                    })
                    .collect(),
                host_mode,
                publish_only: playbook.publish_only,
            };
            Some(NodeContract {
                node: n.id.clone(),
                level: spec.level,
                contract: contract::evaluate(&host, &facts),
            })
        })
        .collect()
}

/// The agents of a node's executor chain (primary, then fallbacks),
/// canonical and without repeats.
fn chain_agents(
    root: &Path,
    playbook: &Playbook,
    kind: &apb_core::schema::NodeKind,
    origin: PlaybookOrigin,
) -> Vec<String> {
    let Some(r) = kind.effective_profile_ref(&playbook.defaults) else {
        return Vec::new();
    };
    let Ok(p) = apb_core::profile_store::resolve_profile(root, origin, &r) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    let exec = &p.doc.executor;
    for a in std::iter::once(&exec.agent).chain(exec.fallbacks.iter().map(|f| &f.agent)) {
        let id = apb_core::agents::canonical_id(a).to_string();
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// The refused nodes: every `full` node whose contract this machine cannot
/// hold.
pub fn refused(contracts: &[NodeContract]) -> Vec<(&str, &[Reason])> {
    contracts
        .iter()
        .filter(|c| !c.contract.allowed())
        .map(|c| (c.node.as_str(), c.contract.refusals.as_slice()))
        .collect()
}

/// The gate's refusal: `isolation_unavailable` with each node and its
/// reasons.
pub fn check(
    root: &Path,
    playbook: &Playbook,
    origin: PlaybookOrigin,
    host_mode: bool,
) -> Result<(), Value> {
    let contracts = node_contracts(root, playbook, origin, host_mode);
    let refused = refused(&contracts);
    if refused.is_empty() {
        return Ok(());
    }
    let nodes: Vec<Value> = refused
        .iter()
        .map(|(node, reasons)| json!({ "node": node, "reasons": reasons }))
        .collect();
    Err(json!({
        "policy": contract::UNAVAILABLE,
        "nodes": nodes,
        "detail": format!("this machine cannot isolate {} as `isolation: full` requires", text(&refused)),
    }))
}

/// `node `a` (bwrap_missing (install ...)); node `b` (...)`.
pub fn text(refused: &[(&str, &[Reason])]) -> String {
    refused
        .iter()
        .map(|(node, reasons)| {
            let r: Vec<String> = reasons.iter().map(ToString::to_string).collect();
            format!("node `{node}` ({})", r.join("; "))
        })
        .collect::<Vec<_>>()
        .join(", ")
}
