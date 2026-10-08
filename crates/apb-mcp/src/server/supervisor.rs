//! MCP tool handlers for the supervisor domain. Split out of `server` so the
//! handler surface stays navigable; each block registers a named router that
//! `server::WfMcp::tool_router` combines. Handler logic delegates to
//! `crate::tools` / `profile_tools` / `advisory_tools` / `catalog`.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};

use serde_json::json;

use super::args::*;
use super::{WfMcp, sliced_wait, to_call_tool_result};
use crate::tools::{self, ToolError};

#[tool_router(router = supervisor_router, vis = "pub(crate)")]
impl WfMcp {
    #[tool(
        description = "Block server-side until the next supervisor wake, a new human_review gate to relay, the end of the run, or timeout_ms, then return it with fresh status. Every return costs you a turn, so wait long: pass the largest timeout_ms your host allows (default 50000, max 1800000); the server keeps your heartbeat alive and sends progress notifications meanwhile. reason timeout means nothing happened: call again at once with after_seq = next_after_seq, without other calls. Requires the `observe` capability",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn supervisor_wait_event(
        &self,
        Parameters(SupervisorWaitArgs {
            token,
            after_seq,
            timeout_ms,
            inline_prompt,
        }): Parameters<SupervisorWaitArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let run_id = match self.resolve_session(&token, "supervisor_wait_event") {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Err(e)),
        };
        let root = (*self.root).clone();
        let total = tools::wait_timeout(timeout_ms);
        let (r, id) = (root.clone(), run_id.clone());
        let outcome = sliced_wait(
            &ctx,
            total,
            format!("waiting for a supervisor wake on run `{run_id}`"),
            move |slice| {
                let o = apb_engine::run_wait::wait_supervisor_event(&r, &id, after_seq, slice)?;
                let done = !matches!(o, apb_engine::run_wait::SupervisorWait::TimedOut);
                Ok((o, done))
            },
        )
        .await;
        to_call_tool_result(outcome.and_then(|o| {
            let mut out = tools::supervisor_wait_result(&root, &run_id, after_seq, &o)?;
            if inline_prompt == Some(false) {
                tools::drop_inline_prompts(&mut out);
            }
            Ok(out)
        }))
    }

    #[tool(
        description = "Get an inspection report of a supervised run (status, nodes, outputs, context, wakes, actions, events). Long texts inside events are elided since outputs, context and wakes already carry them; pass full_events: true for the raw texts. Call it only when a wake detail is not enough. Requires the `observe` capability",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn supervisor_run_inspect(
        &self,
        Parameters(SupervisorInspectArgs { token, full_events }): Parameters<SupervisorInspectArgs>,
    ) -> CallToolResult {
        let run_id = match self.resolve_session(&token, "supervisor_run_inspect") {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Err(e)),
        };
        to_call_tool_result(tools::sv_run_inspect_with(
            &self.root,
            &run_id,
            full_events == Some(true),
        ))
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
        description = "Append a supervisor note for subsequent agent attempts on this run. Delivery scope: once the drive applies the note (control cursor advances), every NEW agent_task or finish-with-prompt attempt that starts afterward receives all applied notes so far in a trailing `Supervisor notes (these override the node template and the run instruction on conflict):` block (oldest first, most recent last), whether or not the node template references `{{run.context}}`. On conflict, applied notes override both the node template and the run instruction. Notes do not reach an already-running attempt, do not enter script nodes, and are not written into the immutable run manifest. They also remain in context.md / `{{run.context}}` for templates that use that. Requires the `retry` capability",
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
        description = "Interrupt a RUNNING attempt of a supervised run: SIGKILL the wedged agent so the attempt is journaled failed and ordinary retry/fallback/patch proceeds at the next attempt boundary. Use after a stall anomaly to break a hang rather than wait it out; unlike supervisor_run_abort it does NOT stop the run. A no-op when no attempt is running. Pass `node` to interrupt ONLY that node's attempt, which is what a wedged branch of a concurrent fan-out needs: its healthy siblings keep running. With `node` omitted the interrupt terminates every currently running attempt in the run, not a single node; every interrupted branch recovers via its normal retry and fallback paths. Requires the `retry` capability",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn supervisor_interrupt_attempt(
        &self,
        Parameters(SupervisorInterruptArgs {
            token,
            reason,
            node,
        }): Parameters<SupervisorInterruptArgs>,
    ) -> CallToolResult {
        let run_id = match self.resolve_session(&token, "supervisor_interrupt_attempt") {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Err(e)),
        };
        to_call_tool_result(tools::interrupt_attempt(
            &self.root,
            &run_id,
            reason.as_deref(),
            node.as_deref(),
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
        description = "Rebind a node's executor profile mid-run when its bound agent is wedged (issue #45 finding 5): a sanctioned escape hatch past the immutable manifest's per-node executor pin. Re-runs the trust gate for the NEW profile bundle exactly as run start does (an untrusted bundle is refused with `untrusted_profile_requires_acknowledge` unless acknowledge_untrusted is set; an unresolved one with `profile_unresolved`), journals the accepted rebind as `profile_rebound`, and changes the node's EFFECTIVE binding for future attempts via a journaled overlay - the original run manifest stays intact. The verified bundle is pinned and re-checked from the run snapshot at apply time, so drift between gate and apply is refused (`rebind_rejected`). The next supervisor_node_retry picks up the new profile. Requires the `rebind` capability",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn supervisor_rebind_profile(
        &self,
        Parameters(SupervisorRebindArgs {
            token,
            node,
            profile,
            scope,
            acknowledge_untrusted,
            reason,
        }): Parameters<SupervisorRebindArgs>,
    ) -> CallToolResult {
        let run_id = match self.resolve_session(&token, "supervisor_rebind_profile") {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Err(e)),
        };
        let scope = match scope.as_deref() {
            None | Some("auto") => apb_core::profile::ProfileScope::Auto,
            Some("project") => apb_core::profile::ProfileScope::Project,
            Some("global") => apb_core::profile::ProfileScope::Global,
            Some(other) => {
                return to_call_tool_result(Err(ToolError::Engine(format!(
                    "unknown scope `{other}`"
                ))));
            }
        };
        // The run's own origin drives `auto` scope resolution, so a rebind
        // resolves the new profile exactly as run start resolved node profiles.
        let origin = match apb_engine::run_profile_origin(&self.root, &run_id) {
            Ok(o) => o,
            Err(e) => return to_call_tool_result(Err(ToolError::from(e))),
        };
        // Trust gate for the new bundle - same refusal surface as run start. The
        // verified digest is passed verbatim to the engine (never recomputed), so
        // no profile edit can slip in between the gate and the pinned bundle.
        let bundle = match crate::policy::check_rebind(
            &self.root,
            origin,
            &profile,
            scope,
            acknowledge_untrusted,
        ) {
            Ok(b) => b,
            Err(refusal) => {
                return to_call_tool_result(Ok(json!({ "policy_refusal": refusal })));
            }
        };
        to_call_tool_result(tools::rebind_profile(
            &self.root, &run_id, &node, &profile, scope, &bundle, reason,
        ))
    }

    #[tool(
        description = "Patch the playbook of a supervised run. scope current_run (default): create a patch version from the given YAML and migrate the run onto it, continuing from continue_from (required); classification is `improvement` or `workaround`. scope next_runs: a forward patch for later runs, allowed while the run is live and up to 30 minutes after it ended; it may change nodes that already ran, the run keeps its version, and the version becomes the playbook's candidate, which the next runs try before it is promoted (classification must be `improvement`; pass a rationale and evidence such as journal seqs, node ids and durations). A next_runs patch must build on `current` or the candidate on trial and may not change the goal, effects, irreversible steps, requires, the supervisor block, the decision opt-ins in defaults, worktree, connector grants or sub-playbooks. Requires the `patch_playbook` capability",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn supervisor_patch_playbook(
        &self,
        Parameters(SupervisorPatchArgs {
            token,
            yaml,
            classification,
            continue_from,
            scope,
            rationale,
            evidence,
        }): Parameters<SupervisorPatchArgs>,
    ) -> CallToolResult {
        let run_id = match self.resolve_session(&token, "supervisor_patch_playbook") {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Err(e)),
        };
        match scope.as_deref() {
            None | Some("current_run") => {
                let Some(continue_from) = continue_from else {
                    return to_call_tool_result(Err(tools::ToolError::Engine(
                        "continue_from is required for scope current_run".into(),
                    )));
                };
                to_call_tool_result(tools::playbook_patch(
                    &self.root,
                    &run_id,
                    &yaml,
                    &classification,
                    &continue_from,
                ))
            }
            Some("next_runs") => to_call_tool_result(tools::playbook_forward_patch(
                &self.root,
                &run_id,
                &apb_engine::forward_patch::ForwardPatchRequest {
                    yaml,
                    classification,
                    rationale,
                    evidence: evidence.unwrap_or_default(),
                },
            )),
            Some(other) => to_call_tool_result(Err(tools::ToolError::Engine(format!(
                "scope must be `current_run` or `next_runs`, got `{other}`"
            )))),
        }
    }
}
