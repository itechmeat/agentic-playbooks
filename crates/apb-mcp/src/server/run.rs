//! MCP tool handlers for the run domain. Split out of `server` so the
//! handler surface stays navigable; each block registers a named router that
//! `server::WfMcp::tool_router` combines. Handler logic delegates to
//! `crate::tools` / `profile_tools` / `advisory_tools` / `catalog`.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};
use serde_json::json;

use super::args::*;
use super::{WfMcp, sliced_wait, to_call_tool_result, with_execution, with_warnings};
use crate::tools::{self, ToolError};

#[tool_router(router = run_router, vis = "pub(crate)")]
impl WfMcp {
    #[tool(
        description = "Trial-run a draft playbook by its effects matrix: filesystem-writing ones run in a throwaway git worktree and return a diff; irreversible ones are refused. Accepts an optional instruction, exactly like playbook_run, rendered as {{run.instruction}}. Does not activate the playbook.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn playbook_trial(
        &self,
        Parameters(PlaybookTrialArgs {
            id,
            version,
            params,
            instruction,
            scope,
        }): Parameters<PlaybookTrialArgs>,
    ) -> CallToolResult {
        let scope = scope.as_deref().unwrap_or("project");
        to_call_tool_result(tools::playbook_trial(
            &self.root,
            &id,
            version.as_deref(),
            params,
            instruction,
            scope,
        ))
    }

    #[tool(
        description = "Phase 1 of running a playbook in ANOTHER workspace: resolve the target, run preflight, and return a plan plus a short-lived signed plan_token. Read-only. Show the plan to the user, then call playbook_execute_plan after confirmation.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn playbook_prepare_run(
        &self,
        Parameters(PlaybookPrepareRunArgs {
            id,
            version,
            workspace,
            params,
        }): Parameters<PlaybookPrepareRunArgs>,
    ) -> CallToolResult {
        // The two-phase contract is only for ANOTHER workspace. Running in the
        // current one must go through playbook_run with its policy gate;
        // otherwise an agent could run a local untrusted playbook past the
        // acknowledge gate by giving its own workspace_id. Fail-closed: if we
        // could not determine our own workspace_id, we refuse rather than
        // skip the check.
        match apb_core::workspace::ensure_id(&self.root) {
            Ok(own) if workspace == own => {
                return to_call_tool_result(Ok(json!({
                    "error": "use_playbook_run_for_current_workspace",
                    "detail": "the two-phase plan flow is only for other workspaces",
                })));
            }
            Ok(_) => {}
            Err(_) => {
                return to_call_tool_result(Ok(json!({
                    "error": "cannot_verify_current_workspace",
                    "detail": "refusing prepare_run because the current workspace id could not be determined",
                })));
            }
        }
        to_call_tool_result(tools::playbook_prepare_run(
            &id,
            version.as_deref(),
            &workspace,
            params,
        ))
    }

    #[tool(
        name = "playbook_execute_plan",
        description = "Phase 2: execute a previously prepared cross-workspace plan by its plan_token. Verifies signature, expiry, single-use and that the playbook digest has not drifted, then runs it in the target workspace. An irreversible plan needs confirm_irreversible (the refusal's consent_nonce) after asking the person; acknowledge_untrusted answers trust only.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn playbook_execute_plan_tool(
        &self,
        params: Parameters<PlaybookExecutePlanArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> CallToolResult {
        // The MCP client name, recorded as `by: mcp:<client>` in the consent.
        let client = ctx.peer.peer_info().map(|i| i.client_info.name.clone());
        self.playbook_execute_plan_for(params, client).await
    }

    /// `playbook_execute_plan` without a request context (tests).
    #[cfg(test)]
    pub(crate) async fn playbook_execute_plan(
        &self,
        params: Parameters<PlaybookExecutePlanArgs>,
    ) -> CallToolResult {
        self.playbook_execute_plan_for(params, None).await
    }

    pub(crate) async fn playbook_execute_plan_for(
        &self,
        Parameters(PlaybookExecutePlanArgs {
            plan_token,
            acknowledge_untrusted,
            confirm_irreversible,
        }): Parameters<PlaybookExecutePlanArgs>,
        client: Option<String>,
    ) -> CallToolResult {
        let payload = match crate::plan::decode(&plan_token) {
            Some(p) => p,
            None => return to_call_tool_result(Ok(json!({ "error": "invalid_plan_token" }))),
        };
        let now = apb_core::clock::now_ms() as u64;
        if now > payload.exp_ms {
            return to_call_tool_result(Ok(json!({ "error": "plan_expired" })));
        }
        // Protection against self-routing: a plan for the current workspace
        // should not get here (prepare_run refuses it), but we check here too.
        if let Ok(own) = apb_core::workspace::ensure_id(&self.root)
            && payload.workspace_id == own
        {
            return to_call_tool_result(Ok(
                json!({ "error": "use_playbook_run_for_current_workspace" }),
            ));
        }
        // Single-use: the nonce was already used - a replay. We check early, but
        // only BURN it on actual execution (below), so a policy refusal
        // (untrusted/stale) does not burn the nonce and allows a retry with
        // acknowledge.
        if self.used_nonces.lock().unwrap().contains(&payload.nonce) {
            return to_call_tool_result(Ok(json!({ "error": "plan_replayed" })));
        }
        let root_b = match self.effective_root(Some(&payload.workspace_id)) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        // The run gate, in the target workspace, exactly as `playbook_run` runs
        // it locally: lifecycle, `requires`, the parent's digest and profile
        // trust, and the whole sub-playbook tree (every child's digest and
        // profile trust, with its pins), each gated by the caller's
        // acknowledge. Its permit is what the engine gets, so a child that
        // drifts after this check is refused at spawn too.
        let wref = apb_core::scope::PlaybookRef {
            origin: apb_core::scope::Origin::Project { workspace_id: None },
            id: payload.id.clone(),
            version: Some(payload.version.clone()),
        };
        let permit = match crate::policy::check_run(
            &root_b,
            &wref,
            acknowledge_untrusted == Some(true),
            false,
        ) {
            Ok(p) => p,
            Err(refusal) => return to_call_tool_result(Ok(json!({ "policy_refusal": refusal }))),
        };
        // 0.24.0 irreversible consent: as for playbook_run,
        // `confirm_irreversible` after asking the person, bound to the nonce
        // of the refusal they saw.
        let (consent, deprecation) = match super::mcp_consent(
            &permit,
            confirm_irreversible,
            acknowledge_untrusted,
            client.as_deref(),
        ) {
            Ok(c) => c,
            Err(refusal) => return to_call_tool_result(Ok(json!({ "policy_refusal": refusal }))),
        };
        // The plan the user confirmed must still be what runs: the digest and
        // the profile bundles the gate verified equal the signed plan's.
        if permit.playbook_digest != payload.digest {
            return to_call_tool_result(Ok(
                json!({ "error": "plan_stale", "detail": "playbook changed since prepare" }),
            ));
        }
        let now_profiles: Vec<crate::plan::PlanProfile> = permit
            .profile_bundles
            .iter()
            .map(|(key, bundle)| crate::plan::PlanProfile {
                key: key.clone(),
                bundle: bundle.clone(),
            })
            .collect();
        if now_profiles != payload.profiles {
            return to_call_tool_result(Ok(
                json!({ "error": "plan_stale", "detail": "profile or skill changed since prepare" }),
            ));
        }
        let resolved = match apb_core::store::resolve(&root_b, &wref) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Err(ToolError::from(e))),
        };
        // Burn the nonce right before launch (all gates passed): check-and-insert
        // atomically, so a race between two execute calls does not run the
        // plan twice.
        {
            let mut used = self.used_nonces.lock().unwrap();
            if !used.insert(payload.nonce.clone()) {
                return to_call_tool_result(Ok(json!({ "error": "plan_replayed" })));
            }
        }
        // The exact bundle map from the signed plan - the engine will check it
        // against the snapshot (exact-match), closing any drift between
        // execute and the snapshot.
        let expected_bundles: std::collections::BTreeMap<String, String> = payload
            .profiles
            .iter()
            .map(|p| (p.key.clone(), p.bundle.clone()))
            .collect();
        // Connectors are NOT threaded on the cross-workspace path: the signed
        // plan carries no connector permit, so `expected_connectors` stays
        // empty. A foreign playbook that binds connectors therefore fails
        // closed at run start (the engine refuses connector bindings without a
        // permit) rather than running with unverified connector trust. Full
        // cross-workspace connector consent is a separate plan-payload change.
        let opts = apb_engine::RunOptions {
            params: payload.params.clone(),
            expected_digest: Some(payload.digest.clone()),
            expected_profile_bundles: Some(expected_bundles),
            expected_children: Some(permit.children),
            consent,
            ..Default::default()
        };
        // Driven by a detached process, like every other background start:
        // the run must not die with this MCP session.
        super::with_deprecation(
            match apb_engine::start_detached_resolved(&resolved, opts) {
                Ok(run_id) => to_call_tool_result(Ok(json!({
                    "run_ref": { "workspace_id": payload.workspace_id, "run_id": run_id }
                }))),
                Err(e) => to_call_tool_result(Err(ToolError::from(e))),
            },
            deprecation,
        )
    }

    #[tool(
        name = "playbook_run",
        description = "Run a playbook with the given parameters and instruction. Pass supervise: \"self\" to run it in the background under the caller's supervision and receive a supervisor token; pass background: true to start it in the background and get a run_id immediately, then follow it with run_wait (not by polling run_status). Without either, the call blocks until the run ends. execution: leave it out (cli, the default: apb runs the agent CLIs the profiles name) unless the person asked for mono, host or single-agent mode, or for the run to use your own subagents; then pass execution: \"host\": apb spawns no CLI, the run starts in the background, and every agent step comes back from run_wait as a pending task that you execute with your own subagent tool (never by launching an agent CLI; see run_wait) and submit with run_task_submit. A background or supervised run may also hand you a task on its own when none of a step's CLIs can start (not installed or not logged in). A playbook whose effects include irreversible (push, merge, deploy, publish) is refused with policy irreversible_requires_confirmation, its sources and a consent_nonce: show the sources to the person and, if they agree, call again with confirm_irreversible set to that consent_nonce. acknowledge_untrusted answers trust only; a trust refusal of an irreversible playbook lists its irreversible sources and consent_nonce too, so one question covers both.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn playbook_run_tool(
        &self,
        params: Parameters<PlaybookRunArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> CallToolResult {
        // host execution mode (0.23.0): the MCP client name, for attribution.
        let client = ctx.peer.peer_info().map(|i| i.client_info.name.clone());
        self.playbook_run_for(params, client).await
    }

    /// `playbook_run` without a request context (tests): no client name.
    #[cfg(test)]
    pub(crate) async fn playbook_run(&self, params: Parameters<PlaybookRunArgs>) -> CallToolResult {
        self.playbook_run_for(params, None).await
    }

    pub(crate) async fn playbook_run_for(
        &self,
        Parameters(PlaybookRunArgs {
            id,
            version,
            params,
            instruction,
            supervise,
            background,
            acknowledge_untrusted,
            confirm_irreversible,
            scope,
            continued_from,
            worktree,
            execution,
        }): Parameters<PlaybookRunArgs>,
        client: Option<String>,
    ) -> CallToolResult {
        // --- host execution mode (0.23.0) ---
        // The mode is the caller's choice per run; the client name is only
        // recorded for attribution. A blocking call cannot serve host tasks,
        // so a host-mode run always starts in the background, and only a
        // background or supervised start gets the host fallback.
        let requested_mode = match execution.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(v) => match apb_core::execution::ExecutionMode::parse(v) {
                Some(m) => Some(m),
                None => {
                    return to_call_tool_result(Ok(json!({
                        "error": "unknown_execution",
                        "detail": format!("execution must be \"host\" or \"cli\", got `{v}`"),
                    })));
                }
            },
        };
        let host_requested = requested_mode == Some(apb_core::execution::ExecutionMode::Host);
        let execution = apb_core::execution::ExecutionRequest {
            mode: requested_mode,
            host_session: host_requested
                || background == Some(true)
                || supervise.as_deref() == Some("self"),
            client,
            inherited: false,
        };
        let resolved = match apb_core::execution::resolve_for(&self.root, &execution) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Err(ToolError::Engine(e))),
        };
        // A host request starts in the background even when APB_EXECUTION=cli
        // forced cli: the caller expected a quick start, and the response
        // carries the note that says what happened.
        let background =
            if host_requested || resolved.mode == apb_core::execution::ExecutionMode::Host {
                Some(true)
            } else {
                background
            };
        // --- end host execution mode ---
        // Definition scope: a global playbook runs in the current project.
        // An unknown scope is not silently treated as project - we refuse it (spec 9).
        let origin = match scope.as_deref() {
            None | Some("project") => apb_core::scope::Origin::Project { workspace_id: None },
            Some("global") => apb_core::scope::Origin::Global,
            Some(other) => {
                return to_call_tool_result(Ok(json!({
                    "error": "unknown_scope",
                    "detail": format!("scope must be \"project\" or \"global\", got `{other}`"),
                })));
            }
        };
        // The server-side policy gate (spec 9) applies to ALL run modes,
        // including supervise:"self": lifecycle, digest trust, preflight.
        // Cross-workspace goes through the two-phase contract. check_run
        // returns a verified digest - we pass it to the engine as
        // expected_digest, closing the TOCTOU window between check and load.
        let wref = apb_core::scope::PlaybookRef {
            origin: origin.clone(),
            id: id.clone(),
            version: version.clone(),
        };
        // check_run returns a permit: digest + the EXACT map of verified
        // bundles, gathered in the same pass. We pass that to the engine as
        // is - without a re-resolve (otherwise editing a profile/skill within
        // the window would give the engine a different set). The MCP path
        // (autonomous / supervise:"self") does not spawn an external
        // supervisor agent -> supervised: false (matches the manifest, where
        // neither mode is RunMode::AgentSupervised).
        let permit = match crate::policy::check_run(
            &self.root,
            &wref,
            acknowledge_untrusted == Some(true),
            false,
        ) {
            Ok(p) => p,
            Err(refusal) => return to_call_tool_result(Ok(json!({ "policy_refusal": refusal }))),
        };
        // --- 0.24.0 irreversible consent ---
        // `confirm_irreversible` is the consent, checked against the nonce of
        // the refusal the person saw; `acknowledge_untrusted` answers trust
        // only (for one release it still counts as a consent, with a
        // deprecation note in the response).
        let (consent, deprecation) = match super::mcp_consent(
            &permit,
            confirm_irreversible,
            acknowledge_untrusted,
            execution.client.as_deref(),
        ) {
            Ok(c) => c,
            Err(refusal) => return to_call_tool_result(Ok(json!({ "policy_refusal": refusal }))),
        };
        // --- end 0.24.0 irreversible consent ---

        // Consent-time warnings the gate produced (finding 11: a bound connector
        // with zero configured accounts). Surfaced on every successful run
        // response so the caller can show them to the user; never a refusal.
        let warnings = permit.warnings.clone();

        // supervise:"self" - a project-scoped supervised run; it does not
        // combine with a global scope.
        if supervise.as_deref() == Some("self") {
            if matches!(origin, apb_core::scope::Origin::Global) {
                return to_call_tool_result(Ok(json!({
                    "error": "supervise_self_global_unsupported",
                })));
            }
            return with_execution(
                self.run_supervised_self(
                    id,
                    version,
                    params,
                    instruction,
                    permit.playbook_digest,
                    permit.profile_bundles,
                    permit.children,
                    permit.connectors,
                    permit.connector_accounts,
                    continued_from,
                    worktree,
                    warnings,
                    execution,
                    consent,
                ),
                &resolved,
                deprecation,
            );
        }

        if matches!(origin, apb_core::scope::Origin::Global) {
            let resolved_def = match apb_core::store::resolve(&self.root, &wref) {
                Ok(r) => r,
                Err(e) => return to_call_tool_result(Err(ToolError::from(e))),
            };
            let opts = apb_engine::RunOptions {
                instruction,
                params,
                expected_digest: Some(permit.playbook_digest),
                expected_profile_bundles: Some(permit.profile_bundles),
                expected_children: Some(permit.children),
                expected_connectors: permit.connectors,
                expected_connector_accounts: permit.connector_accounts,
                continued_from,
                worktree,
                execution,
                consent,
                ..Default::default()
            };
            if background == Some(true) {
                return with_execution(
                    match apb_engine::start_detached_resolved(&resolved_def, opts) {
                        Ok(run_id) => to_call_tool_result(with_warnings(
                            Ok(json!({ "run_id": run_id, "scope": "global" })),
                            &warnings,
                        )),
                        Err(e) => to_call_tool_result(Err(ToolError::from(e))),
                    },
                    &resolved,
                    deprecation,
                );
            }
            return with_execution(
                match apb_engine::run_resolved(&resolved_def, opts) {
                    Ok(res) => to_call_tool_result(with_warnings(
                        Ok(
                            json!({ "run_id": res.run_id, "outcome": res.outcome.as_str(), "scope": "global" }),
                        ),
                        &warnings,
                    )),
                    Err(e) => to_call_tool_result(Err(ToolError::from(e))),
                },
                &resolved,
                deprecation,
            );
        }
        if background == Some(true) {
            return with_execution(
                to_call_tool_result(with_warnings(
                    tools::playbook_run_background(
                        &self.root,
                        &id,
                        version.as_deref(),
                        params,
                        instruction,
                        Some(permit.playbook_digest),
                        Some(permit.profile_bundles),
                        Some(permit.children),
                        permit.connectors,
                        permit.connector_accounts,
                        continued_from,
                        worktree,
                        execution,
                        consent,
                    ),
                    &warnings,
                )),
                &resolved,
                deprecation,
            );
        }
        with_execution(
            to_call_tool_result(with_warnings(
                tools::playbook_run(
                    &self.root,
                    &id,
                    version.as_deref(),
                    params,
                    instruction,
                    Some(permit.playbook_digest),
                    Some(permit.profile_bundles),
                    Some(permit.children),
                    permit.connectors,
                    permit.connector_accounts,
                    continued_from,
                    worktree,
                    execution,
                    consent,
                ),
                &warnings,
            )),
            &resolved,
            deprecation,
        )
    }

    #[tool(
        description = "List runs recorded in the project",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn runs_list(
        &self,
        Parameters(WorkspaceArg { workspace }): Parameters<WorkspaceArg>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::runs_list(&root))
    }

    #[tool(
        description = "Get the current status of a run, including liveness: `driver_alive` (null when no process claims the run), `node_times` with each node's start and the age and pid of its open attempt (plus `past_estimate`, true once an open attempt is running past its `expected_duration`), and the node status `lost` for a node whose attempt process is gone. Use `node_times` to tell a slow node from a stuck one, and `apb doctor --run <id>` for a full per-run diagnosis. The answer is large (every node output); to wait for a run to finish or need input, call run_wait instead of calling this repeatedly.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn run_status(
        &self,
        Parameters(RunRefArgs { run_id, workspace }): Parameters<RunRefArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::run_status(&root, &run_id))
    }

    #[tool(
        description = concat!(
            "Wait for a run without spending turns: blocks server-side until the run finishes, needs input (a question, a human_review gate, a supervisor decision, host tasks), stops (paused or driverless), or timeout_ms runs out, then returns a compact result with `reason` and `next`. Use this after playbook_run with background: true, run_resume, run_answer, review_decide or run_task_submit, instead of polling run_status: every status call is a model turn. Pass the largest timeout_ms your host's tool timeout allows (default 50000, max 1800000); progress notifications are sent while it blocks. On reason timeout, call run_wait again with the same arguments. Host tasks (needs: host_task, `pending_tasks`): the run waits for YOU to execute agent steps. Contract (also each task's `execution_note`): ",
            apb_engine::host_task_contract!(),
            " For each pending task give your subagent `role_prompt` as its system context and `prompt` as its task, have it load `skills` and work in `workdir` with `env` set (`hint_note` labels `model_hint` with its `hint_source`, and `fallback_of` says what closed the previous step), then submit its final reply verbatim with run_task_submit; independent tasks may run concurrently. If the subagent needs the user, submit status blocked with the question as output."
        ),
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn run_wait(
        &self,
        Parameters(RunWaitArgs {
            run_id,
            workspace,
            timeout_ms,
        }): Parameters<RunWaitArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        let total = tools::wait_timeout(timeout_ms);
        // One waiter across all slices, so a gate's grace is not restarted
        // at every slice boundary.
        let waiter = match apb_engine::run_wait::RunWaiter::new(&root, &run_id) {
            Ok(w) => std::sync::Mutex::new(w),
            Err(e) => return to_call_tool_result(Err(ToolError::from(e))),
        };
        let res = sliced_wait(
            &ctx,
            total,
            format!("waiting for run `{run_id}`"),
            move |slice| {
                let mut w = waiter
                    .lock()
                    .map_err(|_| ToolError::Engine("run_wait: poisoned waiter".into()))?;
                let res = w.wait(slice)?;
                let done = res.reason != apb_engine::run_wait::WaitReason::Timeout;
                Ok((res, done))
            },
        )
        .await;
        to_call_tool_result(res.and_then(|res| tools::run_wait_result(&root, &run_id, &res)))
    }

    #[tool(
        description = "Report cycle progress for a run: done of total iterations of the current cycle group, with an optional label. Scales the progress bar for loops with a known amount of work. Pass your own node id when you are the executing agent (it is in APB_NODE_ID); with concurrent branches in flight, omitting it means the report may be attributed to a sibling.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn run_progress_report(
        &self,
        Parameters(ProgressReportArgs {
            run_id,
            done,
            total,
            label,
            node,
            workspace,
        }): Parameters<ProgressReportArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::run_progress_report(
            &root, &run_id, done, total, label, node,
        ))
    }

    #[tool(
        description = "List events of a run, optionally starting from a given seq",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn run_events(
        &self,
        Parameters(RunEventsArgs {
            run_id,
            from_seq,
            workspace,
        }): Parameters<RunEventsArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::run_events(&root, &run_id, from_seq))
    }

    #[tool(
        description = "Retrospective numbers of a run, for improving its playbook: per node the time against expected_duration, executions, attempts, retries, fallbacks, re-entries, tokens and cost, the model each attempt actually ran on (host mode: the model the host reported), the host wait and the status-file verdicts; per run the goal results and the medians of the last compare_last finished runs of the same version. Works on a live run (as of now) and a finished one. The same report renders into a prompt as {{run.retro}}.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn run_retro_context(
        &self,
        Parameters(RunRetroContextArgs {
            run_id,
            compare_last,
            workspace,
        }): Parameters<RunRetroContextArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::run_retro_context(&root, &run_id, compare_last))
    }

    #[tool(
        description = "Get a summary report of a run",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn run_report(
        &self,
        Parameters(RunRefArgs { run_id, workspace }): Parameters<RunRefArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::run_report(&root, &run_id))
    }

    #[tool(
        description = "Stop a run: interrupts the node it is executing right now, and finalizes it outright if the process that was driving it is gone. Returns which of those happened.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn run_stop(
        &self,
        Parameters(RunRefArgs { run_id, workspace }): Parameters<RunRefArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::run_stop(&root, &run_id))
    }

    #[tool(
        description = "Resume a run, optionally from a given node. Only a run apb created on this machine can be resumed; a run whose playbook snapshot is not approved needs acknowledge_untrusted: true after user confirmation, like playbook_run. Returns the drift error inline (instead of detaching) when an agent binary changed since run start; pass allow_environment_drift to proceed anyway. A run with no valid consent to its irreversible effects (one an older apb started) is refused once with irreversible_requires_confirmation; pass confirm_irreversible set to its consent_nonce after asking the person.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn run_resume(
        &self,
        Parameters(RunResumeArgs {
            run_id,
            from_node,
            allow_environment_drift,
            acknowledge_untrusted,
            confirm_irreversible,
            workspace,
        }): Parameters<RunResumeArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let client = ctx.peer.peer_info().map(|i| i.client_info.name.clone());
        self.run_resume_for(
            RunResumeArgs {
                run_id,
                from_node,
                allow_environment_drift,
                acknowledge_untrusted,
                confirm_irreversible,
                workspace,
            },
            client,
        )
    }

    pub(crate) fn run_resume_for(
        &self,
        RunResumeArgs {
            run_id,
            from_node,
            allow_environment_drift,
            acknowledge_untrusted,
            confirm_irreversible,
            workspace,
        }: RunResumeArgs,
        client: Option<String>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        if let Err(refusal) =
            crate::policy::check_resume(&root, &run_id, acknowledge_untrusted == Some(true))
        {
            return to_call_tool_result(Ok(json!({ "policy_refusal": refusal })));
        }
        // 0.24.0 irreversible consent: a run with no valid consent recorded
        // (one an older apb started) asks once; the consent is then written
        // into its manifest.
        let (confirmation, acknowledge_note) =
            super::mcp_confirmation(confirm_irreversible, acknowledge_untrusted);
        let by = apb_engine::consent::RunConsent::mcp(client.as_deref()).by;
        let note = match apb_engine::gate::check_resume_consent(
            &root,
            &run_id,
            confirmation.as_ref(),
            &by,
        ) {
            Ok(n) => n.map(|n| acknowledge_note.unwrap_or(n)),
            Err(refusal) => return to_call_tool_result(Ok(json!({ "policy_refusal": refusal }))),
        };
        super::with_deprecation(
            to_call_tool_result(tools::run_resume(
                &root,
                &run_id,
                from_node.as_deref(),
                allow_environment_drift,
            )),
            note,
        )
    }

    #[tool(
        description = "Decide a human_review node of a run: pass run_id, node, decision and an optional note",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn review_decide(
        &self,
        Parameters(ReviewDecideArgs {
            run_id,
            node,
            decision,
            note,
            workspace,
        }): Parameters<ReviewDecideArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::review_decide(
            &root, &run_id, &node, &decision, &note,
        ))
    }

    #[tool(
        description = "Answer a pending interactive question on a run (an agent_task with interactive: true that asked the user something). Provide exactly one of run_id (plain/operator path, posts answered_by: \"human\") or token (supervisor-session path, posts answered_by: \"supervisor\"; a node declaring answer_by: human rejects this path with an error asking the supervisor to relay the question to the user instead). Omit node when exactly one question is pending (the common case); otherwise copy it verbatim from run_status's pending_question.node - an unrecognized node name is not checked against the pending channel and silently appends an orphaned answer rather than erroring.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn run_answer(
        &self,
        Parameters(RunAnswerArgs {
            run_id,
            token,
            node,
            answer,
            workspace,
        }): Parameters<RunAnswerArgs>,
    ) -> CallToolResult {
        let (root, run_id, answered_by) = match (run_id, token) {
            (Some(_), Some(_)) | (None, None) => {
                return to_call_tool_result(Ok(json!({
                    "error": "exactly_one_of_run_id_or_token_required",
                })));
            }
            (Some(_), None) if self.supervisor_role => {
                return to_call_tool_result(Ok(json!({
                    "error": "supervisor_session_requires_token",
                    "detail": "a supervisor answers with its token (answered_by: supervisor); the run_id path answers as the human and is not available to a supervisor",
                })));
            }
            (Some(run_id), None) => {
                let root = match self.effective_root(workspace.as_deref()) {
                    Ok(r) => r,
                    Err(e) => return to_call_tool_result(Ok(e)),
                };
                (root, run_id, "human")
            }
            (None, Some(token)) => {
                let run_id = match self.resolve_session(&token, "run_answer") {
                    Ok(r) => r,
                    Err(e) => return to_call_tool_result(Err(e)),
                };
                ((*self.root).clone(), run_id, "supervisor")
            }
        };
        to_call_tool_result(tools::run_answer(
            &root,
            &run_id,
            node.as_deref(),
            &answer,
            answered_by,
        ))
    }

    // --- host execution mode (0.23.0) ---
    #[tool(
        description = concat!(
            "Submit the result of a host task (a run in host execution mode, or a step whose CLI could not start, hands its agent steps to you through run_wait's pending_tasks). ",
            apb_engine::host_task_contract!(),
            " Pass run_id and task_id verbatim from pending_tasks, status succeeded (the subagent did the task), failed (it could not; the run applies its retry policy) or blocked (it needs the user: output is the question, the run waits for run_answer), and output: the subagent's final reply verbatim, including its closing yaml status block. usage (token counts), note and model (the model the subagent actually ran on) are optional. Then call run_wait again."
        ),
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn run_task_submit(
        &self,
        Parameters(RunTaskSubmitArgs {
            run_id,
            task_id,
            status,
            output,
            usage,
            note,
            model,
            workspace,
        }): Parameters<RunTaskSubmitArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        let client = ctx.peer.peer_info().map(|i| i.client_info.name.clone());
        let usage = usage.map(|u| apb_engine::host_task::SubmittedUsage {
            input_tokens: u.input_tokens.unwrap_or(0),
            output_tokens: u.output_tokens.unwrap_or(0),
            cache_read_tokens: u.cache_read_tokens.unwrap_or(0),
            cache_write_tokens: u.cache_write_tokens.unwrap_or(0),
            cost_usd: u.cost_usd,
        });
        to_call_tool_result(tools::run_task_submit(
            &root, &run_id, &task_id, &status, output, usage, note, "host", client, model,
        ))
    }
    // --- end host execution mode ---
}
