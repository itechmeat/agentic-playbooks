//! Runs started from a run step (routine operations, D3): the starter a
//! new run's launch record names (`LaunchRecord.started_by`) and the bound
//! on chains of such starts.
//!
//! A start made inside a run step (an agent, a script or a host task's
//! subagent, through the CLI or MCP) reads its step from the environment
//! once ([`CallerSnapshot::from_env`]) and resolves it against the
//! starting run's journal ([`resolve`]): verified only for a run this
//! installation created, not ended, whose open attempt (or script visit)
//! the caller's claim binds to through the connector layer's own binding.
//! The attempt token is never stored, only the attempt number it resolved
//! to. Nothing is written into the starting run: the reverse links are
//! read from the new runs' records (`run_view::started_runs`).

use std::path::{Path, PathBuf};

use apb_core::launch::{MAX_START_CHAIN, StartedBy};
use serde_json::{Value, json};

use crate::connector::call::attempt::{AttemptClaim, bind};
use crate::event::{Event, EventPayload};

/// The refusal code of a start that would make a chain too deep.
pub const CHAIN_TOO_DEEP: &str = "start_chain_too_deep";

/// What this process knows about the run step it runs in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallerSnapshot {
    /// `APB_RUN_ID`.
    pub run_id: Option<String>,
    /// `APB_RUN_DIR`.
    pub run_dir: Option<PathBuf>,
    /// `APB_NODE_ID`.
    pub node: Option<String>,
    /// `APB_ATTEMPT`, `APB_ATTEMPT_TOKEN`, `APB_HOST_TASK` and
    /// `APB_ITEM_ID`, read by the connector layer's own reader.
    pub claim: AttemptClaim,
}

impl CallerSnapshot {
    /// The step of the current process environment.
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        CallerSnapshot {
            run_id: var("APB_RUN_ID"),
            run_dir: var("APB_RUN_DIR").map(PathBuf::from),
            node: var("APB_NODE_ID"),
            claim: AttemptClaim::from_env(),
        }
    }
}

/// The starter of a run-step start (`None` outside a run step), or the
/// refusal `start_chain_too_deep` when its chain would pass
/// [`MAX_START_CHAIN`].
pub fn resolve(root: &Path, caller: &CallerSnapshot) -> Result<Option<StartedBy>, Value> {
    let Some(run_id) = caller.run_id.clone() else {
        return Ok(None);
    };
    let found = starter_dir(root, caller, &run_id);
    let (verified, attempt) = match &found {
        Some((dir, _)) => verify(dir, &run_id, caller),
        None => (false, None),
    };
    let chain = match (&found, verified) {
        (Some((dir, _)), true) => chain_of(dir).saturating_add(1),
        _ => 1,
    };
    if chain > MAX_START_CHAIN {
        return Err(json!({
            "policy": CHAIN_TOO_DEEP,
            "detail": format!(
                "run `{run_id}` is the {} in a chain of runs started from run steps; a {} is refused. Start it outside a run step",
                ordinal(chain - 1),
                ordinal(chain)
            ),
            "chain": chain,
        }));
    }
    let host_task = caller.claim.host_task.clone();
    Ok(Some(StartedBy {
        workspace_id: found
            .as_ref()
            .and_then(|(_, project)| apb_core::projects::registered_id(project)),
        run_id,
        node: caller.node.clone(),
        attempt,
        host_task,
        verified,
        chain,
    }))
}

/// The chain depth of the run at `run_dir`: its launch record's
/// `started_by.chain`, else its parent's (a child, through its run
/// config's `parent_run`, at most `MAX_SUBPLAYBOOK_DEPTH` steps), else 0.
pub fn chain_of(run_dir: &Path) -> u32 {
    let mut dir = run_dir.to_path_buf();
    for _ in 0..=apb_core::schema::MAX_SUBPLAYBOOK_DEPTH {
        let record = crate::manifest::read(&dir)
            .ok()
            .flatten()
            .and_then(|m| m.launch)
            .and_then(|l| l.started_by);
        if let Some(by) = record {
            return by.chain;
        }
        let Some(parent) = crate::run_config::read_run_config(&dir)
            .ok()
            .and_then(|c| c.parent_run)
        else {
            return 0;
        };
        dir = crate::run_config::sibling_run(&dir, &parent);
    }
    0
}

/// The starting run's directory and its project root: `APB_RUN_DIR` when
/// set, else the run of that id under `root`. Accepted only when it is no
/// link, a directory named `run_id` under `<project>/.apb/runs/` for a
/// project that is `root` or a registered one.
fn starter_dir(root: &Path, caller: &CallerSnapshot, run_id: &str) -> Option<(PathBuf, PathBuf)> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return None;
    }
    let named = caller
        .run_dir
        .clone()
        .unwrap_or_else(|| root.join(".apb/runs").join(run_id));
    let meta = std::fs::symlink_metadata(&named).ok()?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return None;
    }
    let canon = std::fs::canonicalize(&named).ok()?;
    if canon.file_name()? != run_id {
        return None;
    }
    let runs = canon.parent()?;
    let apb = runs.parent()?;
    if runs.file_name()? != "runs" || apb.file_name()? != ".apb" {
        return None;
    }
    let project = apb.parent()?.to_path_buf();
    let known = std::fs::canonicalize(root).ok().as_ref() == Some(&project)
        || apb_core::projects::list_active()
            .iter()
            .filter_map(|e| std::fs::canonicalize(&e.path).ok())
            .any(|p| p == project);
    known.then_some((canon, project))
}

/// Whether the caller's step is verified on the run at `dir`, and the
/// attempt its claim bound to: a run this installation created, not ended,
/// whose open attempt of the caller's node the claim matches (the
/// connector layer's binding, which accepts a host task a resumed drive
/// adopted), or, for a script step (no attempt), whose visit of the node is
/// still open.
fn verify(dir: &Path, run_id: &str, caller: &CallerSnapshot) -> (bool, Option<u32>) {
    let Some(node) = caller.node.as_deref() else {
        return (false, None);
    };
    if !apb_core::run_origin::verify(dir, run_id) {
        return (false, None);
    }
    let Ok(events) = crate::event::read_all_lossy_tail(dir) else {
        return (false, None);
    };
    if crate::state::RunState::fold(&events)
        .run_status
        .is_terminal()
    {
        return (false, None);
    }
    let b = bind(&events, node, &caller.claim);
    let verified = b.may_send
        && match b.attempt {
            Some(_) => true,
            None => script_claim(dir, node, &caller.claim) && visit_open(&events, node),
        };
    (verified, b.attempt.filter(|_| verified))
}

/// Whether a claim bound to no attempt may stand for a script step: it
/// carries no attempt credentials (a script runs no executor, so it has
/// none; a list item's id is checked by the binding), and `node` is a
/// script node in the run's definition. An open visit of any other node,
/// or a claim holding another attempt's token, verifies nobody.
fn script_claim(dir: &Path, node: &str, claim: &AttemptClaim) -> bool {
    if claim.attempt.is_some() || claim.token.is_some() || claim.host_task.is_some() {
        return false;
    }
    std::fs::read_to_string(dir.join("playbook.yaml"))
        .ok()
        .and_then(|yaml| crate::legacy_snapshot::parse_snapshot_playbook(&yaml).ok())
        .is_some_and(|p| {
            p.node(node)
                .is_some_and(|n| matches!(n.kind, apb_core::schema::NodeKind::Script { .. }))
        })
}

/// Whether the latest visit of `node` started and did not finish.
fn visit_open(events: &[Event], node: &str) -> bool {
    let started = events.iter().rposition(
        |e| matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node),
    );
    started.is_some_and(|i| {
        !events[i + 1..]
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::NodeFinished { node: n, .. } if n == node))
    })
}

/// `1st`, `2nd`, `3rd`, `4th`, ...
fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (1, 11) | (2, 12) | (3, 13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

#[cfg(test)]
mod tests;
