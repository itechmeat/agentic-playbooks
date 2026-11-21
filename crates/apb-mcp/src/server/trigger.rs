//! MCP tools for start rules: the list (`trigger_list`) and the dry test
//! (`trigger_test`). Both are reads; there is no write tool, because a
//! person writes and approves a rule (`apb trigger` at a terminal, or the
//! dashboard signed in), never an agent (D0 "A person's command"). The
//! test's answer carries no rendered value: a person's terminal shows
//! values with `apb trigger test`. Every handler reads signed files and
//! runs the dry checks, so it goes to the blocking pool.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use serde_json::json;

use super::WfMcp;
use super::quality::off_runtime;
use crate::tools::grade::to_value;
use crate::tools::trigger::{list, test_rule};

#[tool_router(router = trigger_router, vis = "pub(crate)")]
impl WfMcp {
    #[tool(
        description = "List the start rules of the project: each rule's name, whether it is enabled, its source, its pinned playbook and version, the runs it started in the last hour, its last recorded verdict and whether it needs a person's approval. A rule a person wrote and approved starts a pinned playbook from an inbox delivery; you never write, change, approve or remove one (apb trigger, or the dashboard).",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn trigger_list(
        &self,
        Parameters(_a): Parameters<super::args::TriggerListArgs>,
    ) -> CallToolResult {
        let root = (*self.root).clone();
        off_runtime(move || {
            let rules = list(&root)?;
            to_value(json!({ "rules": rules }))
        })
        .await
    }

    #[tool(
        description = "Dry-test a start rule against one stored event of its source: how the filter and conditions judged it, every param's render, the start key and its no-write peek, and the checks a start surface runs before preparation, each passed, refused with its code, or not run. Writes nothing and starts nothing: probation, readiness, the concurrency key and the working tree are checked only when a run actually starts (the report names them as checked at start). Refused from a run step.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn trigger_test(
        &self,
        Parameters(a): Parameters<super::args::TriggerTestArgs>,
    ) -> CallToolResult {
        let root = (*self.root).clone();
        off_runtime(move || {
            let report = test_rule(&root, &a.rule, a.event_seq, false)?;
            to_value(report)
        })
        .await
    }
}
