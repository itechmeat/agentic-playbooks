//! MCP tool handlers for the run domain. Split out of `server` so the
//! handler surface stays navigable; each block registers a named router that
//! `server::WfMcp::tool_router` combines. Handler logic delegates to
//! `crate::tools` / `profile_tools` / `advisory_tools` / `catalog`.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use serde_json::json;

use super::args::*;
use super::{WfMcp, to_call_tool_result};
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
        description = "Phase 2: execute a previously prepared cross-workspace plan by its plan_token. Verifies signature, expiry, single-use and that the playbook digest has not drifted, then runs it in the target workspace.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn playbook_execute_plan(
        &self,
        Parameters(PlaybookExecutePlanArgs {
            plan_token,
            acknowledge_untrusted,
        }): Parameters<PlaybookExecutePlanArgs>,
    ) -> CallToolResult {
        let payload = match crate::plan::decode(&plan_token) {
            Some(p) => p,
            None => return to_call_tool_result(Ok(json!({ "error": "invalid_plan_token" }))),
        };
        let now = apb_engine::event::now_millis() as u64;
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
        // A repeat preflight: the digest must not have drifted between prepare and execute.
        let pf = match crate::policy::preflight(&root_b, &payload.id, Some(&payload.version)) {
            Ok(p) => p,
            Err(refusal) => return to_call_tool_result(Ok(json!({ "policy_refusal": refusal }))),
        };
        if pf.digest != payload.digest {
            return to_call_tool_result(Ok(
                json!({ "error": "plan_stale", "detail": "playbook changed since prepare" }),
            ));
        }
        // A drift in a profile or skill between prepare and execute also breaks the plan.
        let now_profiles: Vec<crate::plan::PlanProfile> = crate::policy::playbook_profile_bundles(
            &root_b,
            &payload.id,
            Some(&payload.version),
            false,
        )
        .into_iter()
        .map(|(key, bundle)| crate::plan::PlanProfile { key, bundle })
        .collect();
        if now_profiles != payload.profiles {
            return to_call_tool_result(Ok(
                json!({ "error": "plan_stale", "detail": "profile or skill changed since prepare" }),
            ));
        }
        // Trust: an unapproved digest requires the user's explicit confirmation
        // (spec 9). preflight does not check trust - we do it here, so an
        // untrusted playbook of another workspace is not run silently.
        if acknowledge_untrusted != Some(true)
            && !apb_core::trust::TrustStore::load().is_approved(&payload.digest)
        {
            return to_call_tool_result(Ok(json!({
                "policy_refusal": {
                    "policy": "untrusted_requires_acknowledge",
                    "id": payload.id,
                    "digest": payload.digest,
                    "detail": "re-run execute_plan with acknowledge_untrusted: true after user confirmation",
                }
            })));
        }
        // Trust of the plan's profiles: every bundle from the signed plan must
        // be approved (or an explicit acknowledge). Otherwise a trusted
        // playbook with an untrusted profile would run without confirmation
        // (spec 5.1).
        if acknowledge_untrusted != Some(true) {
            let store = apb_core::trust::TrustStore::load();
            let untrusted: Vec<String> = payload
                .profiles
                .iter()
                .filter(|p| !store.is_approved(&p.bundle))
                .map(|p| p.key.clone())
                .collect();
            if !untrusted.is_empty() {
                return to_call_tool_result(Ok(json!({
                    "policy_refusal": {
                        "policy": "untrusted_profile_requires_acknowledge",
                        "profiles": untrusted,
                        "detail": "re-run execute_plan with acknowledge_untrusted: true after user confirmation",
                    }
                })));
            }
        }
        let wref = apb_core::scope::PlaybookRef {
            origin: apb_core::scope::Origin::Project { workspace_id: None },
            id: payload.id.clone(),
            version: Some(payload.version.clone()),
        };
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
            ..Default::default()
        };
        match apb_engine::run_background_resolved(&resolved, opts) {
            Ok(run_id) => to_call_tool_result(Ok(json!({
                "run_ref": { "workspace_id": payload.workspace_id, "run_id": run_id }
            }))),
            Err(e) => to_call_tool_result(Err(ToolError::from(e))),
        }
    }

    #[tool(
        description = "Run a playbook with the given parameters and instruction. Pass supervise: \"self\" to run it in the background under the caller's supervision and receive a supervisor token; pass background: true to start it in the background and get a run_id immediately",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn playbook_run(
        &self,
        Parameters(PlaybookRunArgs {
            id,
            version,
            params,
            instruction,
            supervise,
            background,
            acknowledge_untrusted,
            scope,
        }): Parameters<PlaybookRunArgs>,
    ) -> CallToolResult {
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
        // supervisor_expected is also false for these modes).
        let permit = match crate::policy::check_run(
            &self.root,
            &wref,
            acknowledge_untrusted == Some(true),
            false,
        ) {
            Ok(p) => p,
            Err(refusal) => return to_call_tool_result(Ok(json!({ "policy_refusal": refusal }))),
        };

        // supervise:"self" - a project-scoped supervised run; it does not
        // combine with a global scope.
        if supervise.as_deref() == Some("self") {
            if matches!(origin, apb_core::scope::Origin::Global) {
                return to_call_tool_result(Ok(json!({
                    "error": "supervise_self_global_unsupported",
                })));
            }
            return self.run_supervised_self(
                id,
                version,
                params,
                instruction,
                permit.playbook_digest,
                permit.profile_bundles,
                permit.children,
                permit.connectors,
                permit.connector_accounts,
            );
        }

        if matches!(origin, apb_core::scope::Origin::Global) {
            let resolved = match apb_core::store::resolve(&self.root, &wref) {
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
                ..Default::default()
            };
            if background == Some(true) {
                return match apb_engine::start_detached_resolved(&resolved, opts) {
                    Ok(run_id) => {
                        to_call_tool_result(Ok(json!({ "run_id": run_id, "scope": "global" })))
                    }
                    Err(e) => to_call_tool_result(Err(ToolError::from(e))),
                };
            }
            return match apb_engine::run_resolved(&resolved, opts) {
                Ok(res) => to_call_tool_result(Ok(
                    json!({ "run_id": res.run_id, "outcome": res.outcome.as_str(), "scope": "global" }),
                )),
                Err(e) => to_call_tool_result(Err(ToolError::from(e))),
            };
        }
        if background == Some(true) {
            return to_call_tool_result(tools::playbook_run_background(
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
            ));
        }
        to_call_tool_result(tools::playbook_run(
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
        ))
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
        description = "Get the current status of a run, including liveness: `driver_alive` (null when no process claims the run), `node_times` with each node's start and the age and pid of its open attempt, and the node status `lost` for a node whose attempt process is gone. Use `node_times` to tell a slow node from a stuck one, and `apb doctor --run <id>` for a full per-run diagnosis.",
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
        description = "Report cycle progress for a run: done of total iterations of the current cycle group, with an optional label. Scales the progress bar for loops with a known amount of work.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn run_progress_report(
        &self,
        Parameters(ProgressReportArgs {
            run_id,
            done,
            total,
            label,
            workspace,
        }): Parameters<ProgressReportArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::run_progress_report(
            &root, &run_id, done, total, label,
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
        description = "Resume a run, optionally from a given node",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn run_resume(
        &self,
        Parameters(RunResumeArgs {
            run_id,
            from_node,
            workspace,
        }): Parameters<RunResumeArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::run_resume(&root, &run_id, from_node.as_deref()))
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
}
