//! MCP tool handlers for the judge sandbox and labelled examples:
//! `judge_evaluate`, `judge_examples`, `judge_example_save` and
//! `judge_run_values`. The logic lives in `crate::tools::judge`, shared with
//! `apb judge` and the HTTP API, and every one of them answers the same
//! shapes. None is routed by a read-only server; a live ask, a live check
//! and a save are refused when this server runs as a step of a run.
//!
//! Their arguments live here: the inputs and answers are the engine's
//! shapes (`JudgeInputs`, `DecisionAnswer`), read strictly, so a caller can
//! never name a provider, a URL, a key or a model.

use std::collections::BTreeMap;

use apb_core::step_env::StepCaller;
use apb_engine::event::DecisionAnswer;
use apb_engine::judge_eval::JudgeInputs;
use apb_engine::judge_eval::example::{ExampleSaveRequest, ExpectedAnswer};
use apb_engine::judge_eval::sandbox::SandboxRequest;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use super::quality::{invalid, off_runtime};
use super::{WfMcp, to_call_tool_result};
use crate::tools;
use crate::tools::judge::{JudgeCaller, LABELLED_BY_MCP};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JudgeEvaluateArgs {
    /// The playbook id (a project playbook).
    pub id: String,
    /// The judge node.
    pub node: String,
    /// The version; `current` when absent.
    #[serde(default)]
    pub version: Option<String>,
    /// What the node's state renders from: `params`, `instruction`,
    /// `outputs` (by node id), `reviews` (by review node id: `{ decision,
    /// note? }`), `rejected_outputs`, `hooks`, `context`.
    #[serde(default)]
    pub inputs: Option<serde_json::Value>,
    /// Answers per question id (`{ value, p, confidence }` or `{ invalid }`):
    /// evaluated without asking anyone.
    #[serde(default)]
    pub answers: Option<serde_json::Value>,
    /// When the node would hand over to its emulation, also ask the
    /// configured `llm_emulation` providers (priced, inside the daily cap).
    #[serde(default)]
    pub emulate: bool,
    /// workspace_id of another workspace (spec 7). None - the current one.
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JudgeExamplesArgs {
    pub id: String,
    /// One judge node; every node with examples when absent.
    #[serde(default)]
    pub node: Option<String>,
    /// Check each example against `current`: stale, state_changed or the
    /// agreement of each label.
    #[serde(default)]
    pub check: bool,
    /// Check by asking the sandbox again (inside its daily cap) instead of
    /// evaluating the observed answers. Implies `check`.
    #[serde(default)]
    pub live: bool,
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JudgeExampleSaveArgs {
    pub run_id: String,
    pub node: String,
    /// The judge node's execution, 1-based; the latest when absent.
    #[serde(default)]
    pub execution: Option<u32>,
    /// The answer expected per question id: true or false for a yes or no
    /// question, else the option, band or level by name.
    pub expected: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JudgeRunValuesArgs {
    pub run_id: String,
    pub node: String,
    /// The judge node's execution, 1-based; the latest when absent.
    #[serde(default)]
    pub execution: Option<u32>,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// The sandbox request of `judge_evaluate`'s arguments.
fn sandbox_request(a: &JudgeEvaluateArgs) -> Result<SandboxRequest, String> {
    let inputs: JudgeInputs = match &a.inputs {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("inputs: {e}"))?,
        None => JudgeInputs::default(),
    };
    let answers: Option<BTreeMap<String, DecisionAnswer>> = match &a.answers {
        Some(v) => Some(serde_json::from_value(v.clone()).map_err(|e| format!("answers: {e}"))?),
        None => None,
    };
    Ok(SandboxRequest {
        version: a.version.clone(),
        inputs,
        answers,
        emulate: a.emulate,
    })
}

/// The save request of `judge_example_save`'s arguments.
fn save_request(a: &JudgeExampleSaveArgs) -> Result<ExampleSaveRequest, String> {
    let expected = a
        .expected
        .iter()
        .map(|(q, v)| {
            serde_json::from_value::<ExpectedAnswer>(v.clone())
                .map(|e| (q.clone(), e))
                .map_err(|_| format!("expected.{q}: true, false or a name"))
        })
        .collect::<Result<_, _>>()?;
    Ok(ExampleSaveRequest {
        execution: a.execution,
        expected,
        note: a.note.clone(),
    })
}

#[tool_router(router = judge_router, vis = "pub(crate)")]
impl WfMcp {
    #[tool(
        description = "Evaluate one judge node of a playbook outside any run, only when the user asks for it. Returns the raw answers per question, what the node outputs on this machine now (as_configured, at the judge_node use's mode), what it would output at enforce (at_enforce), the route of each, and the requests and cost. With answers given it asks nothing; without, it asks this machine's decision providers from decisions.yaml (at most 100 sandbox requests per project and UTC day). The answer is never applied anywhere. Refused from a run step unless answers are given.",
        annotations(destructive_hint = false)
    )]
    pub(crate) async fn judge_evaluate(
        &self,
        Parameters(args): Parameters<JudgeEvaluateArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(args.workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        let req = match sandbox_request(&args) {
            Ok(r) => r,
            Err(detail) => return invalid(detail),
        };
        off_runtime(move || {
            let step = StepCaller::from_env();
            tools::judge::judge_evaluate(&root, &args.id, &args.node, &req, JudgeCaller::of(&step))
                .and_then(tools::grade::to_value)
        })
        .await
    }

    #[tool(
        description = "List a playbook's labelled judge examples (stored under .apb/playbooks/<id>/evals/judge/<node>/), and with check: true say for each whether it is stale (the node changed since labelling), state_changed (its inputs render another state) or how each label agrees with what the node outputs now, with counts and a confusion table per node. live: true asks the sandbox again (inside its daily cap; refused from a run step), so it may spend this machine's decision requests. Nothing is tuned from it.",
        annotations(destructive_hint = false)
    )]
    pub(crate) async fn judge_examples(
        &self,
        Parameters(args): Parameters<JudgeExamplesArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(args.workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        off_runtime(move || {
            let step = StepCaller::from_env();
            tools::judge::judge_examples(
                &root,
                &args.id,
                args.node.as_deref(),
                args.check,
                args.live,
                JudgeCaller::of(&step),
            )
            .and_then(tools::grade::to_value)
        })
        .await
    }

    #[tool(
        description = "Save one judge execution of a run as a labelled example, only when the user asks for it and names the expected answers. The inputs are redacted and scrubbed, a secret param is kept as <secret>, and the save is refused when a credential-shaped value is left. The file is written under the playbook's evals/judge/<node>/, which is committed with the project. Returns the example as stored. Refused from a run step.",
        annotations(destructive_hint = false)
    )]
    pub(crate) async fn judge_example_save(
        &self,
        Parameters(args): Parameters<JudgeExampleSaveArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(args.workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        let req = match save_request(&args) {
            Ok(r) => r,
            Err(detail) => return invalid(detail),
        };
        off_runtime(move || {
            let step = StepCaller::from_env();
            tools::judge::judge_example_save(
                &root,
                &args.run_id,
                &args.node,
                &req,
                LABELLED_BY_MCP,
                JudgeCaller::of(&step),
            )
            .and_then(tools::grade::to_value)
        })
        .await
    }

    #[tool(
        description = "The inputs one judge execution of a run had when it started, every other value the run recorded (node outputs, review notes, params, baseline observations) and the model's answer then, redacted: what judge_evaluate can be filled from.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn judge_run_values(
        &self,
        Parameters(args): Parameters<JudgeRunValuesArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(args.workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        off_runtime(move || {
            tools::judge::judge_run_values(&root, &args.run_id, &args.node, args.execution)
                .and_then(tools::grade::to_value)
        })
        .await
    }
}
