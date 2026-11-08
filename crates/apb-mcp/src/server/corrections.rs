//! MCP tool handlers for the corrections ledger: a playbook's corrections
//! (`playbook_corrections`, read-only, allowed anywhere) and a person's
//! decision relayed by the host (`correction_decide`, dismiss and reopen
//! only). The logic and the run-step guard live in
//! `crate::tools::corrections`, shared with `apb corrections` and the HTTP
//! API. Marking a version as carrying the fix is a person's statement about
//! a definition, so the MCP layer never constructs
//! `tools::corrections::Decision::MarkFixed`: its args have no fixed
//! decision, and the person marks it with `apb corrections` or on the
//! dashboard. Every handler reads or writes files under a lock, so it runs
//! off the async runtime.

use apb_core::step_env::StepCaller;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use super::WfMcp;
use super::quality::off_runtime;
use super::to_call_tool_result;
use crate::tools::ToolError;
use crate::tools::corrections::{Decision, correction_decide, corrections_view};
use crate::tools::grade::to_value;
use crate::tools::problems::Target;

/// Who decided, as the store keeps it.
const BY: &str = "mcp";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CorrectionsArgs {
    /// The playbook id.
    pub id: String,
    /// `project` or `global`; defaults to where the playbook lives.
    #[serde(default)]
    pub scope: Option<String>,
    /// One correction's id (`c-...`): returns only that entry.
    #[serde(default)]
    pub entry: Option<String>,
    /// workspace_id of another workspace (spec 7). None - the current one.
    #[serde(default)]
    pub workspace: Option<String>,
}

/// The decision an MCP client may relay: a person's dismiss or reopen.
/// There is no fixed decision here: marking a version as carrying the fix
/// is the CLI's and the dashboard's.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionDecision {
    Dismiss,
    Reopen,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CorrectionDecideArgs {
    /// The playbook id.
    pub id: String,
    /// The correction's id (`c-...`, see playbook_corrections).
    pub entry: String,
    /// `dismiss` (never shown again) or `reopen` (back in the list).
    pub decision: CorrectionDecision,
    /// The person's reason, kept with the decision.
    #[serde(default)]
    pub reason: Option<String>,
    /// `project` or `global`; defaults to where the playbook lives.
    #[serde(default)]
    pub scope: Option<String>,
    /// workspace_id of another workspace (spec 7). None - the current one.
    #[serde(default)]
    pub workspace: Option<String>,
}

#[tool_router(router = corrections_router, vis = "pub(crate)")]
impl WfMcp {
    #[tool(
        description = "List a playbook's corrections ledger (a person's review notes, node notes, question answers and rejections that recurred): entries by status, the leading wording and the draft a person may add to a node's prompt. Never put a draft into a playbook on your own; a person applies it and marks the version that carries the fix. Refused from a run step.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn playbook_corrections(
        &self,
        Parameters(a): Parameters<CorrectionsArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(a.workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        off_runtime(move || {
            let t = Target::resolve(&root, &a.id, a.scope.as_deref())?;
            let mut view = corrections_view(&root, &t, &StepCaller::from_env())?;
            if let Some(entry) = a.entry.as_deref() {
                let one = view
                    .entries
                    .into_iter()
                    .find(|e| e.correction.id == entry)
                    .ok_or_else(|| ToolError::NotFound(format!("correction `{entry}`")))?;
                view.entries = vec![one];
            }
            to_value(view)
        })
        .await
    }

    #[tool(
        description = "Dismiss or reopen one correction entry, on the person's explicit word only. Marking a fix is the CLI's and the dashboard's. Refused from a run step.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn correction_decide(
        &self,
        Parameters(a): Parameters<CorrectionDecideArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(a.workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        let decision = match a.decision {
            CorrectionDecision::Dismiss => Decision::Dismiss,
            CorrectionDecision::Reopen => Decision::Reopen,
        };
        off_runtime(move || {
            let t = Target::resolve(&root, &a.id, a.scope.as_deref())?;
            correction_decide(
                &root,
                &t,
                &a.entry,
                decision,
                BY,
                a.reason.as_deref(),
                &StepCaller::from_env(),
            )
            .and_then(to_value)
        })
        .await
    }
}
