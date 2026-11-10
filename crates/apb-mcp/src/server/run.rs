//! MCP tool handlers for the run domain. Split out of `server` so the
//! handler surface stays navigable; each block registers a named router that
//! `server::WfMcp::tool_router` combines. Handler logic delegates to
//! `crate::tools` / `profile_tools` / `advisory_tools` / `catalog`.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};
use serde_json::json;
use std::collections::BTreeMap;

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
        args: Parameters<PlaybookTrialArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let client = ctx.peer.peer_info().map(|i| i.client_info.name.clone());
        self.playbook_trial_for(args, client)
    }

    /// [`Self::playbook_trial`] for the MCP client named `client`.
    pub(crate) fn playbook_trial_for(
        &self,
        Parameters(PlaybookTrialArgs {
            id,
            version,
            params,
            instruction,
            scope,
            confirm_launch,
            launch_version,
            launch_choice,
            external_data,
        }): Parameters<PlaybookTrialArgs>,
        client: Option<String>,
    ) -> CallToolResult {
        let scope = scope.as_deref().unwrap_or("project");
        // The effective flag, inherited from a flagged step's run, before
        // the gate (the caller's tri-state value kept until here).
        let external_data =
            match apb_engine::external_data::for_surface_start(&self.root, external_data, None) {
                Ok(flag) => flag,
                Err(refusal) => return to_call_tool_result(Ok(refusal.body())),
            };
        let launch = super::mcp_launch(
            client,
            apb_engine::gate::launch::LaunchKind::Trial,
            confirm_launch,
            launch_version,
            launch_choice,
        )
        .with_external_data(external_data);
        to_call_tool_result(tools::playbook_trial(
            &self.root,
            &id,
            version.as_deref(),
            params,
            instruction,
            scope,
            &launch,
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
            confirm_launch,
            launch_version,
            launch_choice,
            external_data,
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
        // The effective flag, inherited from a flagged step's run (a step of
        // this project), before the gate.
        let external_data =
            match apb_engine::external_data::for_surface_start(&self.root, external_data, None) {
                Ok(flag) => flag,
                Err(refusal) => return to_call_tool_result(Ok(refusal.body())),
            };
        let launch = super::mcp_launch(
            client.clone(),
            apb_engine::gate::launch::LaunchKind::ExecutePlan,
            confirm_launch,
            launch_version,
            launch_choice,
        )
        .with_external_data(external_data);
        let permit = match crate::policy::check_run(
            &root_b,
            &wref,
            acknowledge_untrusted == Some(true),
            false,
            &launch,
        ) {
            Ok(p) => p,
            Err(refusal) => {
                return to_call_tool_result(Ok(tools::actions::policy_refusal(
                    apb_engine::next_action::start::StartOf::of(&wref),
                    refusal,
                )));
            }
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
            Err(refusal) => {
                return to_call_tool_result(Ok(tools::actions::policy_refusal(
                    apb_engine::next_action::start::StartOf::of(&wref),
                    refusal,
                )));
            }
        };
        if let Err(refusal) =
            apb_engine::gate::check_params_with(&root_b, &wref, &payload.params, external_data)
        {
            return to_call_tool_result(Ok(tools::actions::policy_refusal(
                apb_engine::next_action::start::StartOf::of(&wref),
                refusal,
            )));
        }
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
        let mut warnings = checked.warnings;
        warnings.extend(apb_engine::gate::short_secret_warning(
            &root_b,
            &wref,
            &payload.params,
        ));
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
            launch: permit.launch,
            expected_children: Some(permit.children),
            consent,
            budget,
            estimate: checked.estimate,
            external_data,
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
        description = "Run a playbook with the given parameters and instruction. Pass supervise: \"self\" to run it in the background under the caller's supervision and receive a supervisor token; pass background: true to start it in the background and get a run_id immediately, then follow it with run_wait (not by polling run_status). Without either, the call blocks until the run ends. execution: leave it out (cli, the default: apb runs the agent CLIs the profiles name) unless the person asked for mono, host or single-agent mode, or for the run to use your own subagents; then pass execution: \"host\": apb spawns no CLI, the run starts in the background, and every agent step comes back from run_wait as a pending task that you execute with your own subagent tool (never by launching an agent CLI; see run_wait) and submit with run_task_submit. A background or supervised run may also hand you a task on its own when none of a step's CLIs can start (not installed or not logged in). A playbook whose effects include irreversible (push, merge, deploy, publish) is refused with policy irreversible_requires_confirmation, its sources and a consent_nonce: show the sources to the person and, if they agree, call again with confirm_irreversible set to that consent_nonce. acknowledge_untrusted answers trust only; a trust refusal of an irreversible playbook lists its irreversible sources and consent_nonce too, so one question covers both. budget (tokens, usd, on_no_estimate: ask, pause or continue) limits the run's spend over the playbook's own budget: block; the run pauses before a node whose median spend from comparable runs no longer fits. With on_no_estimate ask and nodes that have no estimate, the start is refused with budget_needs_decision, the estimate and the unestimated nodes: ask the person pause or continue and call again with budget.on_no_estimate set to the answer. Every start answer carries estimate (time, tokens, cost from comparable runs, with a summary) and budget when the run has one. notify (only when the person asked to hear about the run): true uses the machine's notify target, false turns notifications off, an object sets on and/or target; a target that cannot be used refuses the start with notify_unavailable. unattended: true (only when the person asked for a run nobody watches, such as CI): every wait without a declared unattended outcome fails the run with hitl_required instead of waiting; it grants no consent or trust, and with execution host or supervise self it is refused with unattended_unsupported. start_key: pass the scheduler's or event's id so a repeat answers the run already started (already_started: true) instead of starting another; another request under the key is refused start_key_conflict.",
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
            confirm_launch,
            launch_version,
            launch_choice,
            unattended,
            external_data,
            deadline,
            start_key,
            start_key_reuse,
        }): Parameters<PlaybookRunArgs>,
        client: Option<String>,
    ) -> CallToolResult {
        let unattended = unattended == Some(true);
        // A deadline argument that does not parse is refused first.
        let deadline_ms = match deadline.as_deref().map(apb_engine::run_deadline::parse_arg) {
            None => None,
            Some(Ok(ms)) => Some(ms),
            Some(Err(e)) => {
                return to_call_tool_result(Ok(json!({
                    "policy": apb_engine::run_deadline::DEADLINE_INVALID,
                    "detail": e,
                })));
            }
        };
        // External data (capability run-external-data): the effective flag
        // (inherited from a flagged predecessor) and the remember refusal,
        // before every other check and before anything is written.
        let external_data = match apb_engine::external_data::for_surface_start(
            &self.root,
            external_data,
            continued_from.as_deref(),
        )
        .and_then(|flag| {
            apb_engine::external_data::check_remember(flag, remember == Some(true)).map(|()| flag)
        }) {
            Ok(flag) => flag,
            Err(refusal) => return to_call_tool_result(Ok(refusal.body())),
        };
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
        // The start key: the request captured from the tool arguments as
        // passed (the version as asked, never the candidate the gate
        // picks), then the read-only lookup before the gate.
        let start_key = match start_key.as_deref() {
            None => None,
            Some(raw) => {
                let fields = start_request(&StartRequestOf {
                    version: version.as_deref(),
                    params: &params,
                    instruction: instruction.as_deref(),
                    worktree: worktree.as_deref(),
                    execution: requested_mode.map(|m| m.as_str()),
                    supervise: supervise.as_deref(),
                    deadline_ms,
                    budget: budget.as_ref(),
                    unattended,
                    continued_from: continued_from.as_deref(),
                    external_data,
                });
                let scope_label = match origin {
                    apb_core::scope::Origin::Global => "global",
                    _ => "project",
                };
                match tools::run::keyed_lookup(
                    &self.root,
                    scope_label,
                    &id,
                    raw,
                    start_key_reuse == Some(true),
                    &fields,
                ) {
                    Ok(tools::run::Keyed::Go(req)) => Some(req),
                    Ok(tools::run::Keyed::Answer(v)) => return to_call_tool_result(Ok(v)),
                    Err(v) => {
                        let wref = apb_core::scope::PlaybookRef {
                            origin: origin.clone(),
                            id: id.clone(),
                            version: version.clone(),
                        };
                        return to_call_tool_result(Ok(tools::actions::policy_refusal(
                            apb_engine::next_action::start::StartOf::of(&wref),
                            v,
                        )));
                    }
                }
            }
        };
        // The checks that read the environment or the settings (the
        // unattended mode's, the execution resolution) come after the
        // lookup: a repeat of a keyed start answers its run even when the
        // environment changed.
        // An unattended start that also asks for host execution (or
        // resolves to it) or for supervision is refused before anything
        // else, by the run gate's own check: both would wait with nobody to
        // answer.
        let unattended_start = apb_engine::gate::UnattendedStart {
            unattended,
            mode: if supervise.as_deref() == Some("self") {
                apb_engine::RunMode::Supervised
            } else {
                apb_engine::RunMode::Autonomous
            },
            execution: &apb_core::execution::ExecutionRequest {
                mode: requested_mode,
                client: client.clone(),
                ..Default::default()
            },
        };
        if let Err(refusal) = apb_engine::gate::check_unattended(&self.root, &unattended_start) {
            let wref = apb_core::scope::PlaybookRef {
                origin: apb_core::scope::Origin::Project { workspace_id: None },
                id: id.clone(),
                version: version.clone(),
            };
            return to_call_tool_result(Ok(tools::actions::policy_refusal(
                apb_engine::next_action::start::StartOf::of(&wref),
                refusal,
            )));
        }
        let execution = apb_core::execution::ExecutionRequest {
            mode: requested_mode,
            // An unattended run never falls back to a host task: nobody
            // would execute it.
            host_session: !unattended
                && (host_requested
                    || background == Some(true)
                    || supervise.as_deref() == Some("self")),
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
        let launch_kind = match &continued_from {
            Some(pred) => apb_engine::gate::launch::LaunchKind::ContinueFrom {
                run_id: pred.clone(),
                node: String::new(),
            },
            None => apb_engine::gate::launch::LaunchKind::Start,
        };
        let launch = super::mcp_launch(
            execution.client.clone(),
            launch_kind,
            confirm_launch,
            launch_version,
            launch_choice,
        )
        .with_external_data(external_data);
        let permit = match crate::policy::check_run(
            &self.root,
            &wref,
            acknowledge_untrusted == Some(true),
            false,
            &launch,
        ) {
            Ok(p) => p,
            Err(refusal) => {
                return to_call_tool_result(Ok(tools::actions::policy_refusal(
                    apb_engine::next_action::start::StartOf::of(&wref),
                    refusal,
                )));
            }
        };
        // Issue #192: the candidate version the gate chose (and pinned) for
        // a start without an explicit version is the one that runs.
        let version = permit.run_version(version.as_deref());
        // The start the gate pinned: a refusal's start actions name it, so
        // their remedy targets the version that was refused.
        let pinned = apb_core::scope::PlaybookRef {
            version: version.clone(),
            ..wref.clone()
        };
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
            Err(refusal) => {
                return to_call_tool_result(Ok(tools::actions::policy_refusal(
                    apb_engine::next_action::start::StartOf::of(&pinned),
                    refusal,
                )));
            }
        };
        // --- end 0.24.0 irreversible consent ---
        // A param the start leaves without a value, refused like the gate.
        if let Err(refusal) =
            apb_engine::gate::check_params_with(&self.root, &pinned, &params, external_data)
        {
            return to_call_tool_result(Ok(tools::actions::policy_refusal(
                apb_engine::next_action::start::StartOf::of(&pinned),
                refusal,
            )));
        }

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
        warnings.extend(apb_engine::gate::short_secret_warning(
            &self.root, &pinned, &params,
        ));
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
        let notify = match tools::notify::NotifyArg::request_of(notify.as_ref()).and_then(|req| {
            let key = apb_core::policy_file::PlaybookKey::of(&run_ref);
            tools::notify::start_check(&self.root, Some(&key), req.as_ref()).map(|w| (req, w))
        }) {
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
            launch: permit.launch,
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
            unattended,
            external_data,
            deadline_ms,
            start_key,
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
            let r = if background == Some(true) {
                apb_engine::start_detached_resolved(&resolved_def, opts)
                    .map(|run_id| json!({ "run_id": run_id, "scope": "global" }))
            } else {
                apb_engine::run_resolved(&resolved_def, opts).map(|res| {
                    json!({ "run_id": res.run_id, "outcome": res.outcome.as_str(), "scope": "global" })
                })
            }
            .map(|out| tools::budget::with_start_blocks(&self.root, out));
            tools::run::or_already_started(&self.root, r)
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
        description = "List runs recorded in the project; needs_look marks a run with a baseline regression, findings or warnings. Without a filter the answer is the array of runs. With playbook, scope (project or global) or fields (name=value terms, equality, all must match, at most 8, over the params the run's own start definition marks search: true; text compared without case) the answer is { runs, not_indexed, withheld }: each run with its scope and search fields, not_indexed, when fields are given, the runs the terms left out because they have no search fields (zero without fields, when nothing is left out), withheld the runs a term skipped because that field's value is withheld or too long. A withheld value is never shown or matched.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn runs_list(
        &self,
        Parameters(RunsListArgs {
            workspace,
            playbook,
            scope,
            fields,
        }): Parameters<RunsListArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        match tools::run::run_filter(playbook, scope, &fields, false) {
            Ok(None) => {
                // The plain listing also reads the journals: off the async
                // runtime like every other filesystem-bound handler.
                super::quality::off_runtime(move || tools::runs_list(&root)).await
            }
            Ok(Some(filter)) => {
                // The filtered listing reads every run's start definition
                // (blocking filesystem IO): off the async runtime like the
                // other heavy handlers do.
                super::quality::off_runtime(move || tools::run::runs_list_filtered(&root, &filter))
                    .await
            }
            Err(e) => to_call_tool_result(Err(e)),
        }
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
        to_call_tool_result(tools::run_status_as(&root, &run_id, self.action_caller()))
    }

    #[tool(
        description = concat!(
            "Wait for a run without spending turns: blocks server-side until the run finishes, needs input (a question, a human_review or publish gate, a supervisor decision, host tasks), stops (paused or driverless), or timeout_ms runs out, then returns a compact result with `reason`, `next` (the reason of the primary next action), `instruction` (how to act on it here, when it says more) and `next_actions`: each action names the tool that takes it, the arguments it carries (pass `request` back unchanged, so an answer to a request a later round replaced is refused), what you fill in, and whether you may take it or relay it to the person (`relay_to_person`). Use this after playbook_run with background: true, run_resume, run_answer, review_decide or run_task_submit, instead of polling run_status: every status call is a model turn. Pass the largest timeout_ms your host's tool timeout allows (default 50000, max 1800000); progress notifications are sent while it blocks. A publish gate (pending_review.publish) asks a person to consent to an exact push: relay that plan before any review_decide, as its `instruction` says. On reason timeout, call run_wait again with the same arguments. A run its budget paused (pause.cause budget) carries budget and an instruction that says to ask the person, then call run_resume with budget. Host tasks (needs: host_task, `pending_tasks`): the run waits for YOU to execute agent steps. Contract (also each task's `execution_note`): ",
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
        let caller = self.action_caller();
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
            let mut out = tools::run_wait_result_as(&root, &run_id, &res, caller)?;
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
        to_call_tool_result(tools::run_report_as(&root, &run_id, self.action_caller()))
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
        description = "The result snapshots of a run (its nodes' declared results: the output, the declared fields and files, stored at every settlement), oldest first, grouped per execution and list item: each row with its producing attempt, the commit it was taken at, verified (ok, corrupt, missing), the entries (output, fields, file:<scope>:<path>) with size, digest and whether capture redacted them, the omissions with their reason, and incomplete when a limit stopped capture; legacy lists artifact refs recorded without stored bytes. node keeps one node's rows. Every manifest and object is re-hashed on this read and the answer passes the run redactor. Cite an entry as r:<snapshot_id>/<entry> in run_result_read.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn run_results(
        &self,
        Parameters(RunResultsArgs {
            run_id,
            node,
            workspace,
        }): Parameters<RunResultsArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        super::quality::off_runtime(move || {
            tools::results::run_results(&root, &run_id, node.as_deref())
        })
        .await
    }

    #[tool(
        description = "One entry of a stamped result snapshot, as text: ref is r:<snapshot_id>/<entry> (from run_results), optionally with a byte range #[a,b) of at most 64 KiB; without a range, offset and limit select a window of at most 64 KiB; the next window starts at the end of content.range, and an empty range means the text ended. The snapshot is re-verified first: corrupt and secrets_unresolved are refused. Offsets count in the served text, which the run redactor may have changed (redacted_on_read). compare_live adds live: same, differs, missing, unreadable or not_comparable for a workspace file against the tree now, never live content.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn run_result_read(
        &self,
        Parameters(RunResultReadArgs {
            run_id,
            r#ref,
            offset,
            limit,
            compare_live,
            workspace,
        }): Parameters<RunResultReadArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        super::quality::off_runtime(move || {
            tools::results::run_result_read(
                &root,
                &run_id,
                &r#ref,
                offset,
                limit,
                compare_live.unwrap_or(false),
            )
        })
        .await
    }

    #[tool(
        description = "Change whether a person hears about a live run: notify true sends its transitions (a review or a question waiting, a pause nobody asked for, the end) through the run's current target, or the machine's notify target; false stops them; { on: [...], finished: [...] } keeps only these of the transitions it notifies about (review, question, paused, finished, host_task, supervisor) and of the outcomes its end-of-run notice covers (succeeded, needs_look, failed, aborted); a field left out keeps the run's. It never sets a target or widens the transitions or the outcomes: the person does that in the dashboard or with apb notify. Call it only when the person asked. The target is checked now (it can carry notifications, the connector and the account are approved) and its digests are pinned: a target that fails is refused with notify_unavailable and nothing changes. A run that has ended is refused with run_ended. Returns the run's notify block.",
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
            retry_now,
            node,
            allow_environment_drift,
            acknowledge_untrusted,
            confirm_irreversible,
            workspace,
            budget,
            confirm_budget,
            confirm_launch,
            launch_version,
            launch_choice,
            external_data,
            deadline,
        }): Parameters<RunResumeArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let client = ctx.peer.peer_info().map(|i| i.client_info.name.clone());
        self.run_resume_for(
            RunResumeArgs {
                run_id,
                from_node,
                retry_now,
                node,
                allow_environment_drift,
                acknowledge_untrusted,
                confirm_irreversible,
                workspace,
                budget,
                confirm_budget,
                confirm_launch,
                launch_version,
                launch_choice,
                external_data,
                deadline,
            },
            client,
        )
    }

    pub(crate) fn run_resume_for(
        &self,
        RunResumeArgs {
            run_id,
            from_node,
            retry_now,
            node,
            allow_environment_drift,
            acknowledge_untrusted,
            confirm_irreversible,
            workspace,
            budget,
            confirm_budget,
            confirm_launch,
            launch_version,
            launch_choice,
            external_data,
            deadline,
        }: RunResumeArgs,
        client: Option<String>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        let deadline_ms = match deadline.as_deref().map(apb_engine::run_deadline::parse_arg) {
            None => None,
            Some(Ok(ms)) => Some(ms),
            Some(Err(e)) => {
                return to_call_tool_result(Ok(json!({
                    "policy": apb_engine::run_deadline::DEADLINE_INVALID,
                    "detail": e,
                })));
            }
        };
        // A waiting retry to start now. A host over MCP may relay a person or
        // act on its own, so it is recorded as the host, never as the
        // operator. The order of refusals is the dashboard's: the arguments,
        // then the run gate, then "nothing waits to retry".
        let retry_by = apb_engine::scheduler::deferred::ExpediteBy::Host;
        if let Some(refusal) =
            tools::run::retry_now_arg_refusal(retry_now, from_node.is_some(), node.is_some())
        {
            return to_call_tool_result(Ok(refusal));
        }
        // The external data flag is fixed at start: a resume may repeat it,
        // never change it (an argument check, before the run gate).
        if let Err(refusal) = apb_engine::external_data::check_resume(
            &root.join(".apb/runs").join(&run_id),
            external_data,
        ) {
            return to_call_tool_result(Ok(refusal.body()));
        }
        // The origin and trust checks come first, for a live retry-now too:
        // it writes to the run's journal like any resume.
        if let Err(refusal) =
            crate::policy::check_resume(&root, &run_id, acknowledge_untrusted == Some(true))
        {
            return to_call_tool_result(Ok(json!({ "policy_refusal": refusal })));
        }
        let mut retry_nodes = None;
        if retry_now {
            match tools::run::retry_now_plan(&root, &run_id, node.as_deref()) {
                Err(e) => return to_call_tool_result(Err(e)),
                Ok(Err(refusal)) => return to_call_tool_result(Ok(refusal)),
                Ok(Ok(plan)) => retry_nodes = Some(plan),
            }
        }
        // On a live run the expedite is the whole action; on any other run
        // (or one whose driver let go meanwhile) the resume below expedites
        // the retry with its `run_resumed`, once every check passed.
        if retry_nodes.as_ref().is_some_and(|p| p.live) {
            match tools::run::retry_now_live(&root, &run_id, node.as_deref(), retry_by) {
                Ok(Some(mut answer)) => {
                    let unused = [
                        ("budget", budget.is_some()),
                        ("confirm_budget", confirm_budget.is_some()),
                        ("confirm_launch", confirm_launch.is_some()),
                        ("launch_version", launch_version.is_some()),
                        ("launch_choice", launch_choice.is_some()),
                        ("confirm_irreversible", confirm_irreversible.is_some()),
                        ("allow_environment_drift", allow_environment_drift),
                    ];
                    tools::run::note_not_applied(&mut answer, &unused);
                    return to_call_tool_result(Ok(answer));
                }
                Ok(None) => {}
                Err(e) => return to_call_tool_result(Err(e)),
            }
        }
        let budget = match BudgetResumeArg::request(budget.as_ref()) {
            Ok(b) => b,
            Err(refusal) => return to_call_tool_result(Ok(refusal)),
        };
        // The launch policy of a run never authorized (one an older apb
        // started): the decision table on its own snapshot and child tree.
        // An authorized run passes; its grant reaches the driver.
        let launch = super::mcp_launch(
            client.clone(),
            apb_engine::gate::launch::LaunchKind::Resume {
                run_id: run_id.clone(),
            },
            confirm_launch,
            launch_version,
            launch_choice,
        );
        let grant = match apb_engine::resume_launch::check_resume_launch(&root, &run_id, &launch) {
            Ok(g) => g,
            Err(refusal) => {
                let of = apb_engine::next_action::start::StartOf {
                    playbook: refusal["id"].as_str().unwrap_or(&run_id),
                    version: None,
                };
                return to_call_tool_result(Ok(tools::actions::policy_refusal(
                    of,
                    refusal.clone(),
                )));
            }
        };
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
        let resumed = tools::run::run_resume_with(
            &root,
            &run_id,
            &apb_engine::ResumeRequest {
                from_node: from_node.as_deref(),
                allow_environment_drift,
                budget: budget.as_ref(),
                launch: grant.as_ref(),
                retry_now: retry_nodes
                    .as_ref()
                    .map(|_| apb_engine::scheduler::RetryNow {
                        node: node.as_deref(),
                        by: retry_by,
                    }),
                deadline_ms,
                ..apb_engine::ResumeRequest::operator()
            },
        )
        .map(|mut ack| {
            // Only a resume that went ahead started the retry; a refused one
            // (`budget_paused` and the like) waives no delay.
            if let Some(plan) = &retry_nodes
                && ack["detached"] == true
            {
                ack["retry_now"] = json!(plan.nodes);
            }
            ack
        });
        super::with_deprecation(to_call_tool_result(resumed), note)
    }

    #[tool(
        description = "Decide a gate of a run (a human_review or a publish node): pass run_id, node, decision and an optional note. A publish gate (pending_review carries publish) is a person's consent to push exactly one plan: first show the person the plan in their language (URL, branch, the commits with their this-run tags, the remote's current state, outside_push_suspected), then pass decision publish with nonce set to pending_review.publish.nonce, only after they saw that plan and agreed; never decide publish on your own judgment. A stale nonce is refused with the current plan: show that plan and ask again. skip needs no nonce. A human_review gate whose pending_review carries binding approves exactly those result snapshots: show them to the person, and pass binding.nonce with an approving decision. A gate with pending_review.person_only is never decided here (refused agent_channel): ask the person to decide at an interactive terminal or on the dashboard run page where they are signed in, then wait. A run step never decides a gate of its own run tree (run_step_own_run).",
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
            request,
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
            request,
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
            request,
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
            request,
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

/// The tool arguments of `playbook_run` a start key's request binds, as
/// passed.
pub(crate) struct StartRequestOf<'a> {
    pub version: Option<&'a str>,
    pub params: &'a BTreeMap<String, String>,
    pub instruction: Option<&'a str>,
    pub worktree: Option<&'a str>,
    pub execution: Option<&'a str>,
    pub supervise: Option<&'a str>,
    pub deadline_ms: Option<u64>,
    pub budget: Option<&'a BudgetArg>,
    pub unattended: bool,
    pub continued_from: Option<&'a str>,
    pub external_data: bool,
}

/// The request a start key of MCP `playbook_run` binds: a plain mapping
/// of the arguments as passed.
pub(crate) fn start_request(a: &StartRequestOf<'_>) -> apb_core::start_key::StartRequestFields {
    let supervision = if a.supervise == Some("self") {
        apb_engine::RunMode::Supervised
    } else {
        apb_engine::RunMode::Autonomous
    };
    apb_core::start_key::StartRequestFields {
        v: apb_core::start_key::REQUEST_V,
        version: a.version.map(str::to_string),
        params: a.params.clone(),
        instruction: a.instruction.map(str::to_string),
        worktree: a.worktree.map(str::to_string),
        execution: a.execution.map(str::to_string),
        supervision: apb_core::start_key::label(&supervision),
        cache: apb_core::start_key::label(&apb_engine::run_config::CacheRunMode::Auto),
        deadline_ms: a.deadline_ms,
        budget: a.budget.and_then(|b| serde_json::to_value(b).ok()),
        unattended: a.unattended,
        continued_from: a.continued_from.map(str::to_string),
        external_data: a.external_data,
        ..Default::default()
    }
}
