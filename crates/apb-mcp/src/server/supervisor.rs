//! MCP tool handlers for the supervisor domain. Split out of `server` so the
//! handler surface stays navigable; each block registers a named router that
//! `server::WfMcp::tool_router` combines. Handler logic delegates to
//! `crate::tools` / `profile_tools` / `advisory_tools` / `catalog`.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

use super::args::*;
use super::{WfMcp, to_call_tool_result};
use crate::tools::{self, ToolError};

#[tool_router(router = supervisor_router, vis = "pub(crate)")]
impl WfMcp {
    #[tool(
        description = "Block until the next supervisor wake for a run (or timeout), then return it with fresh status. Requires the `observe` capability",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn supervisor_wait_event(
        &self,
        Parameters(SupervisorWaitArgs {
            token,
            after_seq,
            timeout_ms,
        }): Parameters<SupervisorWaitArgs>,
    ) -> CallToolResult {
        let run_id = match self.resolve_session(&token, "supervisor_wait_event") {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Err(e)),
        };
        let root = self.root.clone();
        let res = tokio::task::spawn_blocking(move || {
            tools::supervisor_wait_event(&root, &run_id, after_seq, timeout_ms)
        })
        .await
        .unwrap_or_else(|e| Err(ToolError::Engine(format!("wait task failed: {e}"))));
        to_call_tool_result(res)
    }

    #[tool(
        description = "Get a full inspection report of a supervised run (status, nodes, context, wakes, actions, events). Requires the `observe` capability",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn supervisor_run_inspect(
        &self,
        Parameters(SupervisorRunRefArgs { token }): Parameters<SupervisorRunRefArgs>,
    ) -> CallToolResult {
        let run_id = match self.resolve_session(&token, "supervisor_run_inspect") {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Err(e)),
        };
        to_call_tool_result(tools::sv_run_inspect(&self.root, &run_id))
    }

    #[tool(
        description = "Retry a failed node in a supervised run, optionally overriding its prompt. Requires the `retry` capability",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn supervisor_node_retry(
        &self,
        Parameters(SupervisorRetryArgs {
            token,
            node,
            prompt_override,
        }): Parameters<SupervisorRetryArgs>,
    ) -> CallToolResult {
        let run_id = match self.resolve_session(&token, "supervisor_node_retry") {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Err(e)),
        };
        to_call_tool_result(tools::node_retry(
            &self.root,
            &run_id,
            &node,
            prompt_override,
        ))
    }

    #[tool(
        description = "Continue a supervised run from a given node, skipping the failed one. Requires the `retry` capability",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn supervisor_run_continue_from(
        &self,
        Parameters(SupervisorContinueArgs { token, node }): Parameters<SupervisorContinueArgs>,
    ) -> CallToolResult {
        let run_id = match self.resolve_session(&token, "supervisor_run_continue_from") {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Err(e)),
        };
        to_call_tool_result(tools::run_continue_from(&self.root, &run_id, &node))
    }

    #[tool(
        description = "Pause a supervised run. Requires the `retry` capability",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn supervisor_run_pause(
        &self,
        Parameters(SupervisorRunRefArgs { token }): Parameters<SupervisorRunRefArgs>,
    ) -> CallToolResult {
        let run_id = match self.resolve_session(&token, "supervisor_run_pause") {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Err(e)),
        };
        to_call_tool_result(tools::run_pause(&self.root, &run_id))
    }

    #[tool(
        description = "Abort a supervised run. Requires the `retry` capability",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn supervisor_run_abort(
        &self,
        Parameters(SupervisorRunRefArgs { token }): Parameters<SupervisorRunRefArgs>,
    ) -> CallToolResult {
        let run_id = match self.resolve_session(&token, "supervisor_run_abort") {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Err(e)),
        };
        to_call_tool_result(tools::run_abort(&self.root, &run_id))
    }

    #[tool(
        description = "Append a supervisor note for subsequent agent attempts on this run. Delivery scope: once the drive applies the note (control cursor advances), every NEW agent_task or finish-with-prompt attempt that starts afterward receives all applied notes so far in a trailing `Supervisor notes:` block (oldest first, most recent last), whether or not the node template references `{{run.context}}`. Notes do not reach an already-running attempt, do not enter script nodes, and are not written into the immutable run manifest. They also remain in context.md / `{{run.context}}` for templates that use that. Requires the `retry` capability",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn supervisor_context_append(
        &self,
        Parameters(SupervisorContextArgs { token, note }): Parameters<SupervisorContextArgs>,
    ) -> CallToolResult {
        let run_id = match self.resolve_session(&token, "supervisor_context_append") {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Err(e)),
        };
        to_call_tool_result(tools::context_append(&self.root, &run_id, &note))
    }

    #[tool(
        description = "Interrupt the RUNNING attempt of a supervised run: SIGKILL the wedged agent so the attempt is journaled failed and ordinary retry/fallback/patch proceeds at the next attempt boundary. Use after a stall anomaly to break a hang rather than wait it out; unlike supervisor_run_abort it does NOT stop the run. A no-op when no attempt is running. The interrupt terminates every currently running attempt in the run, not a single node; parallel branches recover via their normal retry and fallback paths. Requires the `retry` capability",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn supervisor_interrupt_attempt(
        &self,
        Parameters(SupervisorInterruptArgs { token, reason }): Parameters<SupervisorInterruptArgs>,
    ) -> CallToolResult {
        let run_id = match self.resolve_session(&token, "supervisor_interrupt_attempt") {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Err(e)),
        };
        to_call_tool_result(tools::interrupt_attempt(
            &self.root,
            &run_id,
            reason.as_deref(),
        ))
    }

    #[tool(
        description = "Write the final supervisor report for a supervised run. Requires the `observe` capability",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn supervisor_report(
        &self,
        Parameters(SupervisorReportArgs { token, text }): Parameters<SupervisorReportArgs>,
    ) -> CallToolResult {
        let run_id = match self.resolve_session(&token, "supervisor_report") {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Err(e)),
        };
        to_call_tool_result(tools::supervisor_report(&self.root, &run_id, &text))
    }

    #[tool(
        description = "Patch the playbook of a supervised run: create a patch version from the given YAML and migrate the run onto it, continuing from the given node. classification is `improvement` or `workaround`. Requires the `patch_playbook` capability",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn supervisor_patch_playbook(
        &self,
        Parameters(SupervisorPatchArgs {
            token,
            yaml,
            classification,
            continue_from,
        }): Parameters<SupervisorPatchArgs>,
    ) -> CallToolResult {
        let run_id = match self.resolve_session(&token, "supervisor_patch_playbook") {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Err(e)),
        };
        to_call_tool_result(tools::playbook_patch(
            &self.root,
            &run_id,
            &yaml,
            &classification,
            &continue_from,
        ))
    }
}
