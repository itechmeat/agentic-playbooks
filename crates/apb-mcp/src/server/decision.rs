//! MCP tool handler for host-task decisions (issue #193): `decision_ask`.
//! The logic lives in `crate::tools::decision`, shared in the engine with
//! `apb decide`.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

use super::args::*;
use super::{WfMcp, to_call_tool_result};
use crate::tools::{self, ToolError};

#[tool_router(router = decision_router, vis = "pub(crate)")]
impl WfMcp {
    #[tool(
        description = "Ask the configured decision providers (decisions.yaml) a bounded question instead of spending a model turn on it: kind choose (one of options), rank (items best first), filter (the items that pass), map (one of options per item), is (yes or no) or score (a position on options, lowest first). Allowed inside a host task: it is a decision call, not running the task elsewhere. Pass run_id (APB_RUN_ID) and node_id (APB_NODE_ID) from a host task so the decision is journaled in the run (decision_made, use host_task, with provider, cost and latency) and the run's decision budget applies. Returns answered: true with the answer and its probability or confidence, or answered: false with refused (off, no_provider, use_off, playbook_off, budget, privacy, run_ended) and a reason: then decide yourself.",
        annotations(destructive_hint = false, open_world_hint = true)
    )]
    pub(crate) async fn decision_ask(
        &self,
        Parameters(DecisionAskArgs {
            kind,
            question,
            options,
            items,
            criteria,
            run_id,
            node_id,
            workspace,
        }): Parameters<DecisionAskArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        let input = tools::DecisionAskInput {
            kind,
            question,
            options: options.unwrap_or_default(),
            items: items.unwrap_or_default(),
            criteria,
            run_id,
            node_id,
        };
        // A decision request is blocking network IO: off the runtime.
        let result = tokio::task::spawn_blocking(move || tools::decision_ask(&root, input))
            .await
            .unwrap_or_else(|e| Err(ToolError::Engine(format!("decision task failed: {e}"))));
        to_call_tool_result(result)
    }
}
