//! The judge sandbox and a judge node's labelled examples on the surfaces:
//! the one tool layer behind MCP (`judge_evaluate`, `judge_examples`,
//! `judge_example_save`, `judge_run_values`), the HTTP API
//! (`apb_server::routes::judge`) and the CLI (`apb judge`). The evaluation,
//! the cap, the save and the check are [`apb_engine::judge_eval`]'s; this
//! layer decides who may spend.
//!
//! Who may do what:
//! - evaluating with given answers is free (no request): anyone, a run step
//!   included;
//! - a live ask, a live check and saving an example spend a provider's
//!   requests or write into the repository: refused from a run step
//!   (`APB_RUN_ID`, `APB_HOST_TASK`) and from a dashboard without keys,
//!   which anything on this machine can reach;
//! - "fill from a run" and listing examples are reads, allowed anywhere.
//!
//! A read-only MCP server does not route any of these tools.

use std::path::Path;

use apb_core::registry::is_safe_segment;
use apb_core::step_env::StepCaller;
use apb_engine::judge_eval::example::{
    self, ExampleSaveRequest, ExamplesCheckRequest, JudgeExample, NodeExamplesCheck,
};
use apb_engine::judge_eval::sandbox::{self, RunValues, SandboxEvaluation, SandboxRequest};
use serde::Serialize;

use super::{ToolError, outside_run};

/// Who asks: a run step or not, and whether through a dashboard without
/// keys.
#[derive(Debug, Clone, Copy)]
pub struct JudgeCaller<'a> {
    pub step: &'a StepCaller,
    /// A dashboard serving without keys: anything on this machine reaches
    /// it, so it never spends or saves.
    pub keyless_dashboard: bool,
}

impl<'a> JudgeCaller<'a> {
    /// A person on the CLI or an MCP client (`step` tells a run step apart).
    pub fn of(step: &'a StepCaller) -> Self {
        JudgeCaller {
            step,
            keyless_dashboard: false,
        }
    }

    /// Refuses `what` (a request to a provider, a write into the project)
    /// from a run step and from a dashboard without keys.
    fn may_spend(&self, what: &str) -> Result<(), ToolError> {
        outside_run(self.step, what)?;
        if self.keyless_dashboard {
            return Err(ToolError::Conflict(format!(
                "{what} is refused by a dashboard without keys, since anything on this machine can reach it; give the answers instead, use `apb judge`, or issue a key with `apb server key issue` and sign in"
            )));
        }
        Ok(())
    }
}

/// The surface name an example saved here is labelled by.
pub const LABELLED_BY_MCP: &str = "mcp";

fn checked(what: &str, id: &str) -> Result<(), ToolError> {
    if is_safe_segment(id) {
        Ok(())
    } else {
        Err(ToolError::NotFound(format!("{what} `{id}`")))
    }
}

/// Evaluates judge node `node` of project playbook `id` in the sandbox. A
/// request with answers asks nothing and is allowed anywhere; one without
/// asks this machine's providers inside the daily cap, refused from a run
/// step and from a dashboard without keys.
pub fn judge_evaluate(
    root: &Path,
    id: &str,
    node: &str,
    req: &SandboxRequest,
    caller: JudgeCaller,
) -> Result<SandboxEvaluation, ToolError> {
    checked("playbook", id)?;
    checked("node", node)?;
    if req.answers.is_none() {
        caller.may_spend("asking a decision model from the judge sandbox")?;
    }
    Ok(sandbox::evaluate(root, id, node, req)?)
}

/// The examples of one judge node.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeExamples {
    pub node: String,
    pub examples: Vec<JudgeExample>,
}

/// `judge_examples` and `GET .../judge/{node}/examples`: the examples as
/// stored, and their check when asked for.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JudgeExamplesView {
    pub playbook: String,
    pub nodes: Vec<NodeExamples>,
    /// Present when a check was asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub checks: Option<Vec<NodeExamplesCheck>>,
}

/// The examples of project playbook `id` (one node, or every node with
/// examples), checked against `current` when `check`; a `live` check asks
/// the sandbox again and is refused where spending is.
pub fn judge_examples(
    root: &Path,
    id: &str,
    node: Option<&str>,
    check: bool,
    live: bool,
    caller: JudgeCaller,
) -> Result<JudgeExamplesView, ToolError> {
    checked("playbook", id)?;
    if let Some(n) = node {
        checked("node", n)?;
    }
    if live {
        caller.may_spend("a live check of labelled examples")?;
    }
    let nodes = example::list(root, id, node)?
        .into_iter()
        .map(|(node, examples)| NodeExamples { node, examples })
        .collect();
    let checks = if check || live {
        Some(example::check(
            root,
            id,
            &ExamplesCheckRequest {
                node: node.map(str::to_string),
                live,
            },
        )?)
    } else {
        None
    };
    Ok(JudgeExamplesView {
        playbook: id.to_string(),
        nodes,
        checks,
    })
}

/// Saves a judge execution of project run `run_id` as a labelled example,
/// labelled by `by` (the surface). It writes into the repository, so it is
/// refused from a run step and from a dashboard without keys.
pub fn judge_example_save(
    root: &Path,
    run_id: &str,
    node: &str,
    req: &ExampleSaveRequest,
    by: &str,
    caller: JudgeCaller,
) -> Result<JudgeExample, ToolError> {
    checked("run", run_id)?;
    checked("node", node)?;
    caller.may_spend("saving a labelled example")?;
    Ok(example::save_from_run(root, run_id, node, req, by)?)
}

/// "Fill from a run": a judge execution's inputs and the run's other
/// recorded values, redacted. A read.
pub fn judge_run_values(
    root: &Path,
    run_id: &str,
    node: &str,
    execution: Option<u32>,
) -> Result<RunValues, ToolError> {
    checked("run", run_id)?;
    checked("node", node)?;
    Ok(example::run_values(root, run_id, node, execution)?)
}
