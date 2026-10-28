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

/// Who a send intent settled through `run_intent_resolve` names.
const INTENT_RESOLVED_BY: &str = "a person, through MCP run_intent_resolve";

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
        description = "Phase 1 of running a playbook in ANOTHER workspace: resolve the target, run preflight, and return a plan plus a short-lived signed plan_token. Read-only. The plan carries the estimate (time, tokens, cost from comparable runs there). Show the plan to the user, then call playbook_execute_plan after confirmation; a budget is its argument.",
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
        description = "Phase 2: execute a previously prepared cross-workspace plan by its plan_token. Verifies signature, expiry, single-use and that the playbook digest has not drifted, then runs it in the target workspace. An irreversible plan needs confirm_irreversible (the refusal's consent_nonce) after asking the person; acknowledge_untrusted answers trust only. budget works as in playbook_run.",
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
            budget,
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
        // The run's budget, checked like playbook_run's before the nonce burns,
        // so a budget refusal allows a retry with the person's decision.
        let budget = match BudgetArg::request(budget.as_ref()) {
            Ok(b) => b,
            Err(refusal) => return to_call_tool_result(Ok(refusal)),
        };
        let execution_mode = match apb_core::execution::resolve_for(&root_b, &Default::default()) {
            Ok(r) => r.mode,
            Err(e) => return to_call_tool_result(Err(ToolError::Engine(e))),
        };
        let checked =
            match tools::budget::preflight(&resolved, execution_mode.as_str(), budget.as_ref()) {
                Ok(p) => p,
                Err(refusal) => return to_call_tool_result(Ok(refusal)),
            };
        let warnings = checked.warnings;
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
            budget,
            estimate: checked.estimate,
            ..Default::default()
        };
        // Driven by a detached process, like every other background start:
        // the run must not die with this MCP session.
        super::with_deprecation(
            match apb_engine::start_detached_resolved(&resolved, opts) {
                Ok(run_id) => {
                    let blocks = tools::budget::with_start_blocks(
                        &root_b,
                        json!({ "run_id": run_id.clone() }),
                    );
                    let mut out = json!({
                        "run_ref": { "workspace_id": payload.workspace_id, "run_id": run_id }
                    });
                    tools::budget::carry_blocks(&blocks, &mut out);
                    to_call_tool_result(with_warnings(Ok(out), &warnings))
                }
                Err(e) => to_call_tool_result(Err(ToolError::from(e))),
            },
            deprecation,
        )
    }

    #[tool(
        name = "playbook_run",
        description = "Run a playbook with the given parameters and instruction. Pass supervise: \"self\" to run it in the background under the caller's supervision and receive a supervisor token; pass background: true to start it in the background and get a run_id immediately, then follow it with run_wait (not by polling run_status). Without either, the call blocks until the run ends. execution: leave it out (cli, the default: apb runs the agent CLIs the profiles name) unless the person asked for mono, host or single-agent mode, or for the run to use your own subagents; then pass execution: \"host\": apb spawns no CLI, the run starts in the background, and every agent step comes back from run_wait as a pending task that you execute with your own subagent tool (never by launching an agent CLI; see run_wait) and submit with run_task_submit. A background or supervised run may also hand you a task on its own when none of a step's CLIs can start (not installed or not logged in). A playbook whose effects include irreversible (push, merge, deploy, publish) is refused with policy irreversible_requires_confirmation, its sources and a consent_nonce: show the sources to the person and, if they agree, call again with confirm_irreversible set to that consent_nonce. acknowledge_untrusted answers trust only; a trust refusal of an irreversible playbook lists its irreversible sources and consent_nonce too, so one question covers both. budget (tokens, usd, on_no_estimate: ask, pause or continue) limits the run's spend over the playbook's own budget: block; the run pauses before a node whose median spend from comparable runs no longer fits. With on_no_estimate ask and nodes that have no estimate, the start is refused with budget_needs_decision, the estimate and the unestimated nodes: ask the person pause or continue and call again with budget.on_no_estimate set to the answer. Every start answer carries estimate (time, tokens, cost from comparable runs, with a summary) and budget when the run has one. notify (only when the person asked to hear about the run): true uses the machine's notify target, false turns notifications off, an object sets on and/or target; a target that cannot be used refuses the start with notify_unavailable.",
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
            budget,
            remember,
            notify,
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
        // Issue #192: the candidate version the gate chose (and pinned) for
        // a start without an explicit version is the one that runs.
        let version = permit.run_version(version.as_deref());
        let mut candidate = super::StartNotes::of(&permit, &self.root);
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

        // supervise:"self" does not combine with a global scope: refused
        // before the budget check, so nobody is asked about a start that
        // cannot happen.
        if supervise.as_deref() == Some("self") && matches!(origin, apb_core::scope::Origin::Global)
        {
            return to_call_tool_result(Ok(json!({
                "error": "supervise_self_global_unsupported",
            })));
        }
        // The budget argument and the pre-start budget check: a policy the
        // person must decide first is refused here with the estimate, and
        // what may surprise them (a dollar limit with no cost history, an
        // estimate over the limit) joins the gate's warnings.
        let budget = match BudgetArg::request(budget.as_ref()) {
            Ok(b) => b,
            Err(refusal) => return to_call_tool_result(Ok(refusal)),
        };
        let run_ref = apb_core::scope::PlaybookRef {
            version: version.clone(),
            ..wref
        };
        let resolved_def = match apb_core::store::resolve(&self.root, &run_ref) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Err(ToolError::from(e))),
        };
        // Consent-time warnings the gate produced (finding 11: a bound connector
        // with zero configured accounts). Surfaced on every successful run
        // response so the caller can show them to the user; never a refusal.
        let mut warnings = permit.warnings.clone();
        let checked = match tools::budget::preflight(
            &resolved_def,
            resolved.mode.as_str(),
            budget.as_ref(),
        ) {
            Ok(p) => p,
            Err(refusal) => return to_call_tool_result(Ok(refusal)),
        };
        warnings.extend(checked.warnings);
        // The notification request: one the person made that cannot be
        // honoured refuses the start before anything is written; a machine
        // default that cannot be used only warns.
        let notify = match tools::notify::NotifyArg::request_of(notify.as_ref())
            .and_then(|req| tools::notify::start_check(&self.root, req.as_ref()).map(|w| (req, w)))
        {
            Ok((req, notes)) => {
                warnings.extend(notes);
                req
            }
            Err(refusal) => return to_call_tool_result(Ok(refusal.body())),
        };
        // What the person chose to keep is remembered once the start
        // succeeded (`StartNotes::annotate`).
        if remember == Some(true) {
            candidate.remember = Some(super::RememberAtStart {
                resolved: resolved_def.clone(),
                params: params.clone(),
                instruction: instruction.clone(),
            });
        }
        let start = tools::StartArgs {
            params,
            instruction,
            expected_digest: Some(permit.playbook_digest),
            expected_profile_bundles: Some(permit.profile_bundles),
            expected_children: Some(permit.children),
            expected_connectors: permit.connectors,
            expected_connector_accounts: permit.connector_accounts,
            continued_from,
            worktree,
            execution,
            consent,
            candidate_skipped: candidate.skipped.clone(),
            budget,
            estimate: checked.estimate,
            notify,
        };

        // supervise:"self" - a project-scoped supervised run (a global scope
        // was refused before the budget check).
        if supervise.as_deref() == Some("self") {
            return with_execution(
                self.run_supervised_self(id, version, start, warnings),
                &resolved,
                deprecation,
                &candidate,
            );
        }

        let started = if matches!(origin, apb_core::scope::Origin::Global) {
            let opts = tools::start_options(start, apb_engine::RunMode::Autonomous);
            if background == Some(true) {
                apb_engine::start_detached_resolved(&resolved_def, opts)
                    .map(|run_id| json!({ "run_id": run_id, "scope": "global" }))
            } else {
                apb_engine::run_resolved(&resolved_def, opts).map(|res| {
                    json!({ "run_id": res.run_id, "outcome": res.outcome.as_str(), "scope": "global" })
                })
            }
            .map(|out| tools::budget::with_start_blocks(&self.root, out))
            .map_err(ToolError::from)
        } else if background == Some(true) {
            tools::playbook_run_background(&self.root, &id, version.as_deref(), start)
        } else {
            tools::playbook_run(&self.root, &id, version.as_deref(), start)
        };
        with_execution(
            to_call_tool_result(with_warnings(started, &warnings)),
            &resolved,
            deprecation,
            &candidate,
        )
    }

    #[tool(
        description = "List runs recorded in the project; needs_look marks a run with a baseline regression, findings or warnings",
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
        description = "Get the current status of a run, including liveness: `driver_alive` (null when no process claims the run), `node_times` with each node's start and the age and pid of its open attempt (plus `past_estimate`, true once an open attempt is running past its `expected_duration`), and the node status `lost` for a node whose attempt process is gone. Use `node_times` to tell a slow node from a stuck one, and `apb doctor --run <id>` for a full per-run diagnosis. `estimate` is what the run was expected to take at start; `budget` (when the run has one) gives the limits, what was spent and is left per dimension, and the node a budget pause holds the run before. The answer is large (every node output); to wait for a run to finish or need input, call run_wait instead of calling this repeatedly.",
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
            "Wait for a run without spending turns: blocks server-side until the run finishes, needs input (a question, a human_review or publish gate, a supervisor decision, host tasks), stops (paused or driverless), or timeout_ms runs out, then returns a compact result with `reason` and `next`. Use this after playbook_run with background: true, run_resume, run_answer, review_decide or run_task_submit, instead of polling run_status: every status call is a model turn. Pass the largest timeout_ms your host's tool timeout allows (default 50000, max 1800000); progress notifications are sent while it blocks. A publish gate (pending_review.publish) asks a person to consent to an exact push: relay that plan before any review_decide, as its `next` says. On reason timeout, call run_wait again with the same arguments. A run its budget paused (pause.cause budget) carries budget and a next that says to ask the person, then call run_resume with budget. Host tasks (needs: host_task, `pending_tasks`): the run waits for YOU to execute agent steps. Contract (also each task's `execution_note`): ",
            apb_engine::host_task_contract!(),
            " For each pending task tell your subagent to read `prompt_path` as its task and `role_path` as its system context (`prompt` and `role_prompt` carry the same text inline; pass inline_prompt: false to drop them), have it load `skills` and work in `workdir` with `env` set (`hint_note` labels `model_hint` with its `hint_source`, and `fallback_of` says what closed the previous step), then submit its final reply verbatim with run_task_submit; independent tasks may run concurrently. If the subagent needs the user, submit status blocked with the question as output."
        ),
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn run_wait(
        &self,
        Parameters(RunWaitArgs {
            run_id,
            workspace,
            timeout_ms,
            inline_prompt,
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
        to_call_tool_result(res.and_then(|res| {
            let mut out = tools::run_wait_result(&root, &run_id, &res)?;
            if inline_prompt == Some(false) {
                tools::drop_inline_prompts(&mut out);
            }
            Ok(out)
        }))
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
        description = "Get a summary report of a run; outcome says what was done, what the engine verified, what nobody verified, and the findings and warnings to look at; notify (when the run has a subscription) says where its notifications go and what was sent.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn run_report(
        &self,
        Parameters(RunReportArgs {
            run_id,
            workspace,
            format,
        }): Parameters<RunReportArgs>,
    ) -> CallToolResult {
        if format.is_some() {
            return to_call_tool_result(Ok(json!({
                "error": "unknown_format",
                "detail": "run_report answers JSON only; run_report_html writes the report as a page",
            })));
        }
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::run_report(&root, &run_id))
    }

    #[tool(
        description = "Write a run's report as one self-contained page (no script, no network) to runs/<id>/report.html, replacing the one there, and return only path and bytes: give the person the path, do not read the page. Call it when the person asks for the report as a file to keep or share.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) async fn run_report_html(
        &self,
        Parameters(RunRefArgs { run_id, workspace }): Parameters<RunRefArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::run_report_html(&root, &run_id))
    }

    #[tool(
        description = "Change whether a person hears about a live run: notify true sends its transitions (a review or a question waiting, a pause nobody asked for, the end) through the run's current target, or the machine's notify target; false stops them; { on: [...] } keeps only these of the transitions it notifies about (review, question, paused, finished). It never sets a target or widens the transitions: the person does that in the dashboard or with apb notify. Call it only when the person asked. The target is checked now (it can carry notifications, the connector and the account are approved) and its digests are pinned: a target that fails is refused with notify_unavailable and nothing changes. A run that has ended is refused with run_ended. Returns the run's notify block.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn run_notify(
        &self,
        Parameters(RunNotifyArgs {
            run_id,
            notify,
            workspace,
        }): Parameters<RunNotifyArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        let res = tools::notify::set_by_host(&root, &run_id, &notify);
        match res {
            Ok(v) => to_call_tool_result(Ok(v)),
            Err(tools::notify::NotifyError::NotFound(m)) => {
                to_call_tool_result(Err(ToolError::NotFound(m)))
            }
            Err(e) => to_call_tool_result(Ok(e.body())),
        }
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
        description = "Record a person's decision on a send whose outcome is not known (a run outcome gap `call outcome unknown` that names `apb intent resolve`): sent true when the person confirmed it went out, false when it did not. This records a person's decision: ask the user first and pass on only their answer, never decide it yourself. A subagent (a host task's or an agent step's) must never call it; it reports the send as a finding instead. After sent true a repeat of the send answers already_sent without sending; after sent false a repeat sends it again. Refused for an intent whose outcome is already known or that is still in flight.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn run_intent_resolve(
        &self,
        Parameters(RunIntentResolveArgs {
            run_id,
            intent,
            sent,
            note,
            workspace,
        }): Parameters<RunIntentResolveArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::run_intent_resolve(
            &root,
            &run_id,
            &intent,
            sent,
            note,
            tools::IntentCaller::person(INTENT_RESOLVED_BY),
        ))
    }

    #[tool(
        description = "Pause a run so it can continue later (any live or paused run, by: operator; no supervisor token). mode drain (default): no new node starts, running nodes finish, then the run is paused and holds no process; the run reads `pausing` until then. mode now: running nodes are interrupted and run again on resume, none counts as failed. Clocks (host-task and node deadlines, statistics) freeze while paused. Open gates (reviews, questions, host tasks) stay open. Optional reason, until (RFC 3339 or a duration such as 90m: the run resumes by itself then) and keep_lock. Pausing a paused run updates its reason and until; a finished run is refused. run_resume continues a paused run, also before until.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn run_pause(
        &self,
        Parameters(RunPauseArgs {
            run_id,
            mode,
            reason,
            until,
            keep_lock,
            workspace,
        }): Parameters<RunPauseArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::pause_run(
            &root,
            &run_id,
            mode.as_deref(),
            reason,
            until.as_deref(),
            keep_lock,
        ))
    }

    #[tool(
        description = "Resume a run, optionally from a given node. Also continues a paused run; before its pause `until` that is an early resume, and the ack says so (resumed_early). Only a run apb created on this machine can be resumed; a run whose playbook snapshot is not approved needs acknowledge_untrusted: true after user confirmation, like playbook_run. Returns the drift error inline (instead of detaching) when an agent binary changed since run start; pass allow_environment_drift to proceed anyway. A run with no valid consent to its irreversible effects (one an older apb started) is refused once with irreversible_requires_confirmation; pass confirm_irreversible set to its consent_nonce after asking the person. A run its budget paused is refused with budget_paused until budget asks for a change: raise_tokens or raise_usd (added to the limit), tokens or usd (a new limit), on_no_estimate, or once (the paused node runs once over the budget); ask the person which first. A budget change is refused once with budget_change_requires_confirmation and a budget_nonce: show the person the change, and if they agree call again with the same budget and confirm_budget set to budget_nonce. A run a supervisor agent watches refuses budget changes here (budget_change_refused): a person uses apb resume or the dashboard.",
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
            budget,
            confirm_budget,
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
                budget,
                confirm_budget,
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
            budget,
            confirm_budget,
        }: RunResumeArgs,
        client: Option<String>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        let budget = match BudgetResumeArg::request(budget.as_ref()) {
            Ok(b) => b,
            Err(refusal) => return to_call_tool_result(Ok(refusal)),
        };
        if let Err(refusal) =
            crate::policy::check_resume(&root, &run_id, acknowledge_untrusted == Some(true))
        {
            return to_call_tool_result(Ok(json!({ "policy_refusal": refusal })));
        }
        // A budget change over MCP needs a person: the first call returns a
        // nonce to show them the change with, the second applies it.
        if let Some(b) = &budget
            && let Err(refusal) =
                tools::budget::confirm_budget_change(&root, &run_id, b, confirm_budget.as_deref())
        {
            return to_call_tool_result(Ok(refusal));
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
                budget.as_ref(),
            )),
            note,
        )
    }

    #[tool(
        description = "Decide a gate of a run (a human_review or a publish node): pass run_id, node, decision and an optional note. A publish gate (pending_review carries publish) is a person's consent to push exactly one plan: first show the person the plan in their language (URL, branch, the commits with their this-run tags, the remote's current state, outside_push_suspected), then pass decision publish with nonce set to pending_review.publish.nonce, only after they saw that plan and agreed; never decide publish on your own judgment. A stale nonce is refused with the current plan: show that plan and ask again. skip needs no nonce.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn review_decide(
        &self,
        Parameters(ReviewDecideArgs {
            run_id,
            node,
            decision,
            note,
            nonce,
            workspace,
        }): Parameters<ReviewDecideArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::review_decide(
            &root,
            &run_id,
            &node,
            &decision,
            &note,
            nonce.as_deref(),
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
            " Pass run_id and task_id verbatim from pending_tasks, status succeeded (the subagent did the task), failed (it could not; the run applies its retry policy) or blocked (it needs the user: output is the question, the run waits for run_answer), and output: the subagent's final reply verbatim, including its closing yaml status block. usage (token counts), note, model (the model the subagent actually ran on), findings and warnings are optional. Then call run_wait again."
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
            findings,
            warnings,
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
            &root,
            &run_id,
            tools::TaskReply {
                task_id,
                status,
                output,
                usage,
                note,
                submitted_by: "host".into(),
                client,
                model,
                findings,
                warnings,
            },
        ))
    }
    // --- end host execution mode ---
}
