//! The model each attempt actually ran on (issue #193 part C), for the run
//! surfaces (`run_status`, `apb runs <id>`, the dashboard node list), `apb
//! stats` and `apb doctor --run`.
//!
//! A CLI attempt runs on the model its `attempt_started` names (the model the
//! engine passed to the agent CLI). A host attempt (a host-mode step, or a
//! step that fell back to the host) runs on whatever the host chose: the
//! model is the one its `host_task_submitted` reports, and unknown when the
//! host reported none. The profile's model is only a hint there, so
//! `attempt_started.model` of a host attempt is never read as the actual one.
//!
//! The expected model of a node is the primary model of the profile the run
//! bound it to (the run manifest's `node_bindings`, first chain step). An
//! attempt is a mismatch when both are known and neither names the other
//! (see [`same_model`]): a fallback step on another model counts too, which
//! is what a reader comparing runs wants to see.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::event::{Event, EventPayload};

/// `executed_by` of an attempt the engine ran through an agent CLI.
pub const EXECUTED_BY_CLI: &str = "cli";
/// `executed_by` of an attempt a host executed (host execution mode).
pub const EXECUTED_BY_HOST: &str = "host";
/// The `attempt_started.agent` of a host attempt.
const HOST_AGENT: &str = "host";

/// One attempt and the model it ran on.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttemptModel {
    pub node: String,
    pub attempt: u32,
    /// [`EXECUTED_BY_CLI`] or [`EXECUTED_BY_HOST`].
    pub executed_by: String,
    /// The agent CLI of a CLI attempt.
    pub agent: Option<String>,
    /// The model the attempt actually ran on; `None` when a host did not
    /// report it.
    pub model: Option<String>,
    /// The primary model of the node's profile, from the run manifest.
    pub expected: Option<String>,
    /// Both models are known and differ.
    pub mismatch: bool,
}

/// Whether two model names name the same model: equal once lowercased with
/// separators (`-`, `_`, `.`, `/`, spaces) removed, or one contains the
/// other (`opus` against `claude-opus-4-1`), so an alias never reads as a
/// mismatch.
pub fn same_model(a: &str, b: &str) -> bool {
    let norm = |s: &str| -> String {
        s.chars()
            .filter(|c| !matches!(c, '-' | '_' | '.' | '/' | ' '))
            .flat_map(char::to_lowercase)
            .collect()
    };
    let (a, b) = (norm(a), norm(b));
    !a.is_empty() && !b.is_empty() && (a == b || a.contains(&b) || b.contains(&a))
}

/// The primary model of each node's profile, from the run manifest
/// (`node_bindings` and the profile's first chain step). Empty when the run
/// has no manifest.
pub fn primary_models(run_dir: &Path) -> BTreeMap<String, String> {
    let Ok(Some(m)) = crate::manifest::read(run_dir) else {
        return BTreeMap::new();
    };
    m.node_bindings
        .iter()
        .filter_map(|(node, key)| {
            let p = m.profiles.iter().find(|p| &p.key() == key)?;
            Some((node.clone(), p.chain.first()?.model.clone()))
        })
        .filter(|(_, model)| !model.trim().is_empty())
        .collect()
}

/// Every attempt of `events` in the order it started, with the model it ran
/// on, checked against `expected` (node to model, see [`primary_models`]).
pub fn attempt_models(events: &[Event], expected: &BTreeMap<String, String>) -> Vec<AttemptModel> {
    let mut out: Vec<AttemptModel> = Vec::new();
    let mut tasks: BTreeMap<&str, (String, u32)> = BTreeMap::new();
    fn slot<'a>(out: &'a mut Vec<AttemptModel>, node: &str, attempt: u32) -> &'a mut AttemptModel {
        let i = match out
            .iter()
            .position(|a| a.node == node && a.attempt == attempt)
        {
            Some(i) => i,
            None => {
                out.push(AttemptModel {
                    node: node.to_string(),
                    attempt,
                    executed_by: EXECUTED_BY_CLI.to_string(),
                    agent: None,
                    model: None,
                    expected: None,
                    mismatch: false,
                });
                out.len() - 1
            }
        };
        &mut out[i]
    }
    for e in events {
        match &e.payload {
            EventPayload::HostTaskRequested {
                task_id,
                node,
                attempt,
                ..
            } => {
                tasks.insert(task_id, (node.clone(), *attempt));
                let s = slot(&mut out, node, *attempt);
                if s.executed_by != EXECUTED_BY_HOST {
                    s.executed_by = EXECUTED_BY_HOST.to_string();
                    s.agent = None;
                    s.model = None;
                }
            }
            EventPayload::AttemptStarted {
                node,
                attempt,
                agent,
                model,
                ..
            } => {
                let s = slot(&mut out, node, *attempt);
                if agent == HOST_AGENT {
                    s.executed_by = EXECUTED_BY_HOST.to_string();
                } else if s.executed_by == EXECUTED_BY_CLI {
                    s.agent = Some(agent.clone());
                    s.model = model.clone().filter(|m| !m.trim().is_empty());
                }
            }
            EventPayload::HostTaskSubmitted {
                task_id,
                model: Some(model),
                submitted_by,
                ..
            } if submitted_by != "engine" && !model.trim().is_empty() => {
                if let Some((node, attempt)) = tasks.get(task_id.as_str()) {
                    slot(&mut out, node, *attempt).model = Some(model.clone());
                }
            }
            _ => {}
        }
    }
    for a in &mut out {
        a.expected = expected.get(&a.node).cloned();
        a.mismatch = matches!((&a.model, &a.expected), (Some(m), Some(x)) if !same_model(m, x));
    }
    out
}

/// [`attempt_models`] of the run at `run_dir`, its expected models read
/// from the run manifest.
pub fn run_attempt_models(run_dir: &Path, events: &[Event]) -> Vec<AttemptModel> {
    attempt_models(events, &primary_models(run_dir))
}

/// One line per mismatched attempt, for a terminal: `node attempt 1 ran on
/// M (host), the profile names P`.
pub fn mismatch_lines(models: &[AttemptModel]) -> Vec<String> {
    models
        .iter()
        .filter(|a| a.mismatch)
        .map(|a| {
            format!(
                "{} attempt {} ran on {} ({}), the profile names {}",
                a.node,
                a.attempt,
                a.model.as_deref().unwrap_or("?"),
                a.executed_by,
                a.expected.as_deref().unwrap_or("?")
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_are_the_same_model_and_other_families_are_not() {
        assert!(same_model("opus", "claude-opus-4-1"));
        assert!(same_model("GLM-5.3-Flash", "glm_5.3_flash"));
        assert!(!same_model("GLM-5.3-Flash", "opus"));
        assert!(!same_model("", "opus"));
    }
}
