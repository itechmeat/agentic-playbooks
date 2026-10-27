//! MCP tool handlers for the playbook domain. Split out of `server` so the
//! handler surface stays navigable; each block registers a named router that
//! `server::WfMcp::tool_router` combines. Handler logic delegates to
//! `crate::tools` / `profile_tools` / `advisory_tools` / `catalog`.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use serde_json::json;

use super::args::*;
use super::{WfMcp, to_call_tool_result};
use crate::tools;

#[tool_router(router = playbook_router, vis = "pub(crate)")]
impl WfMcp {
    #[tool(
        description = "List playbooks available in the project registry",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn playbook_list(
        &self,
        Parameters(WorkspaceArg { workspace }): Parameters<WorkspaceArg>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::playbook_list(&root))
    }

    #[tool(
        description = "Compact structured catalog of playbooks (project and global scope) with trigger, effects, trust and shadowing. Call once per task when matching a user request to a playbook. Pass revision to skip the body when unchanged. Optional query (the task in one sentence): where the machine enabled decision-model ranking, adds an advisory ranked list, needs_playbook_p and covered_by; entries stay unchanged and your own matching still decides. fit: true adds to every entry that is not shadowed whether it can run in this project (ready, compatible, setup, trusted, needs_consent); playbook_adopt_report gives the full report.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn playbook_catalog(
        &self,
        Parameters(PlaybookCatalogArgs {
            revision,
            limit,
            workspace,
            query,
            fit,
        }): Parameters<PlaybookCatalogArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        let fit = fit == Some(true);
        let Some(query) = query.filter(|q| !q.trim().is_empty()) else {
            // The fit reads every entry's tree: off the async runtime.
            let result = tokio::task::spawn_blocking(move || {
                tools::playbook_catalog(
                    &root,
                    &crate::catalog::CatalogRequest {
                        workspace_id: workspace.as_deref(),
                        revision: revision.as_deref(),
                        limit,
                        fit,
                    },
                )
            })
            .await
            .unwrap_or_else(|e| {
                Err(tools::ToolError::Engine(format!(
                    "catalog task failed: {e}"
                )))
            });
            return to_call_tool_result(result);
        };
        // Issue #165 Part 16: a decision request is blocking network IO, so
        // it runs on the blocking pool and never stalls the server's runtime.
        let cache = std::sync::Arc::clone(&self.rank_cache);
        let caller = (*self.root).clone();
        let result = tokio::task::spawn_blocking(move || {
            crate::catalog_rank::playbook_catalog_ranked(
                &caller,
                &root,
                &crate::catalog::CatalogRequest {
                    workspace_id: workspace.as_deref(),
                    revision: revision.as_deref(),
                    limit,
                    fit,
                },
                &query,
                &cache,
            )
        })
        .await
        .unwrap_or_else(|e| {
            Err(tools::ToolError::Engine(format!(
                "catalog task failed: {e}"
            )))
        });
        to_call_tool_result(result)
    }

    #[tool(
        description = "List the user's registered workspaces (current, global and other projects)",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn projects_list(&self) -> CallToolResult {
        to_call_tool_result(tools::projects_list())
    }

    #[tool(
        description = "Authoring details (tier 2): how to write playbook YAML, trigger fields, scopes and secrets. Pull only when creating or reworking a playbook.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn playbook_howto(&self) -> CallToolResult {
        to_call_tool_result(tools::playbook_howto())
    }

    #[tool(
        description = "Interview guide (tier 2): how to build a playbook by interviewing the user about a process they describe. Pull when the user wants to automate a process no playbook covers.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn playbook_interview(&self) -> CallToolResult {
        to_call_tool_result(tools::playbook_interview())
    }

    #[tool(
        description = "Will it fit here: adoption readiness of a playbook (or of every project and global playbook) for a run in this project. Each report has `fit` in four separate sections (compatibility: what the project cannot change; setup: profiles, skills, models, connector accounts and secrets, param values; trust: each digest and whether an acknowledge covers it; consent: irreversible sources and the tree's effects) with `ready` and `trusted` apart, plus `findings` with the profile and model codes. scope narrows to project or global; workspace assesses in another workspace. Read-only. Model availability is only asserted when detection authority is Full.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn playbook_adopt_report(
        &self,
        Parameters(AdoptReportArgs {
            id,
            scope,
            workspace,
        }): Parameters<AdoptReportArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        let result = tokio::task::spawn_blocking(move || {
            crate::advisory_tools::playbook_adopt_report(&root, id.as_deref(), scope.as_deref())
        })
        .await
        .unwrap_or_else(|e| Err(tools::ToolError::Engine(format!("report task failed: {e}"))));
        to_call_tool_result(result)
    }

    #[tool(
        description = "Remembered run input of a playbook in this project: the param values and instruction a start that passes none uses. action get (default) returns them checked against the playbook's params now (incompatible values with a reason, params still missing, where the instruction comes from); set remembers params and/or instruction (an empty instruction clears it on purpose); clear forgets what (all, params or instruction). Secret params, params named like a credential and credential-shaped values are never stored: set lists them under refused. scope picks the definition (project default, or global: a global playbook remembers per project). workspace reads another workspace's memory (get only).",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn playbook_memory(
        &self,
        Parameters(PlaybookMemoryArgs {
            id,
            scope,
            action,
            params,
            instruction,
            what,
            workspace,
        }): Parameters<PlaybookMemoryArgs>,
    ) -> CallToolResult {
        let action = action.as_deref().unwrap_or("get");
        // Memory is personal to a project: another workspace's is read,
        // never written from here.
        if workspace.is_some() && action != "get" {
            return to_call_tool_result(Ok(json!({
                "error": "workspace_param_unsupported",
                "detail": "set and clear work in the current workspace only",
            })));
        }
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        let scope = scope.as_deref().unwrap_or("project");
        let request = match action {
            "get" => tools::memory::MemoryAction::Get,
            "set" => tools::memory::MemoryAction::Set(apb_core::run_memory::RememberInput {
                params,
                instruction,
            }),
            "clear" => match tools::memory::parse_what(what.as_deref()) {
                Some(w) => tools::memory::MemoryAction::Clear(w),
                None => {
                    return to_call_tool_result(Ok(json!({
                        "error": "unknown_what",
                        "detail": "what must be all, params or instruction",
                    })));
                }
            },
            other => {
                return to_call_tool_result(Ok(json!({
                    "error": "unknown_action",
                    "detail": format!("action must be get, set or clear, got `{other}`"),
                })));
            }
        };
        let scope = scope.to_string();
        let result = tokio::task::spawn_blocking(move || {
            let target = tools::memory::MemoryTarget {
                id: &id,
                scope: &scope,
                version: None,
            };
            tools::memory::playbook_memory(&root, &target, request)
        })
        .await
        .unwrap_or_else(|e| Err(tools::ToolError::Engine(format!("memory task failed: {e}"))));
        to_call_tool_result(result)
    }

    #[tool(
        description = "Make a project playbook available in every project: copies it with all versions to the global store (digests and trust carry over), carries its remembered input to the global scope (moved, or copied with variant keep) and applies the chosen fate of the project copy (variant trash, retire or keep). Ask the user which variant first. dry_run: true only reports the plan (blockers, actions, warnings such as other projects with the same id). move_profiles copies the project profiles it binds to the global scope. A step that fails undoes the earlier ones. Once applied, answers with the global playbook's fit report (null with a warning when it cannot be built).",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn playbook_make_global(
        &self,
        Parameters(PlaybookMakeGlobalArgs {
            id,
            variant,
            move_profiles,
            dry_run,
        }): Parameters<PlaybookMakeGlobalArgs>,
    ) -> CallToolResult {
        let root = (*self.root).clone();
        let result = tokio::task::spawn_blocking(move || {
            let variant = match variant.as_deref().map(crate::promote::Variant::parse) {
                None => None,
                Some(Some(v)) => Some(v),
                Some(None) => {
                    return Ok(serde_json::json!({
                        "error": "unknown_variant",
                        "detail": "variant must be trash, retire or keep",
                    }));
                }
            };
            crate::promote::make_global(
                &root,
                &id,
                crate::promote::MakeGlobalOpts {
                    variant,
                    move_profiles,
                    dry_run,
                },
            )
        })
        .await
        .unwrap_or_else(|e| {
            Err(tools::ToolError::Engine(format!(
                "make-global task failed: {e}"
            )))
        });
        to_call_tool_result(result)
    }

    #[tool(
        description = "Capture a just-performed repeatable action as a draft playbook in the chosen scope. Creates a draft (not runnable until trial). Never include secret values in the synopsis.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn playbook_capture(
        &self,
        Parameters(PlaybookCaptureArgs {
            synopsis,
            selected_scope,
            yaml,
        }): Parameters<PlaybookCaptureArgs>,
    ) -> CallToolResult {
        to_call_tool_result(tools::playbook_capture(
            &self.root,
            &synopsis,
            &selected_scope,
            &yaml,
        ))
    }

    #[tool(
        description = "Record the user's decline of a save-as-playbook suggestion. kind soft is a not-now decline whose silence escalates with every repeat, kind hard (the default) is for an explicit never-again and silences the suggestion for one long fixed window rather than forever: the record expires at the returned snoozed_until, and a decline after that renews it. Always pass a one-sentence synopsis of the action: later sessions match against it by meaning. scope defaults to project; use global only when the user says everywhere. Returns the stored record with the computed snoozed_until.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn suggestion_dismiss(
        &self,
        Parameters(SuggestionDismissArgs {
            pattern,
            ttl_days,
            kind,
            synopsis,
            scope,
        }): Parameters<SuggestionDismissArgs>,
    ) -> CallToolResult {
        to_call_tool_result(tools::suggestion_dismiss(
            &self.root,
            tools::DismissRequest {
                pattern: &pattern,
                synopsis: &synopsis,
                kind: kind.as_deref(),
                scope: scope.as_deref(),
                ttl_days,
            },
        ))
    }

    #[tool(
        description = "Activate a playbook after a successful trial or explicit user confirmation: the digest becomes trusted and the lifecycle active. A version imported with scripts needs digest, its exact trust digest: without it the answer is digest_required with the digest and the scripts; show them to the user and approve again with that digest only after they agree. A digest that is not the content now is digest_mismatch.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn playbook_approve(
        &self,
        Parameters(PlaybookApproveArgs {
            id,
            version,
            scope,
            digest,
        }): Parameters<PlaybookApproveArgs>,
    ) -> CallToolResult {
        let scope = scope.as_deref().unwrap_or("project");
        to_call_tool_result(tools::playbook_approve(
            &self.root,
            &id,
            version.as_deref(),
            scope,
            digest.as_deref(),
        ))
    }

    #[tool(
        description = "Get a playbook definition by id and optional version. detail selects the shape: \"summary\" (default) returns the compact interface (id/name/version/schema, params, nodes without prompt bodies, edges, supervisor interface) sized for MCP injection; \"full\" returns the complete authoring payload (yaml + full playbook + layout) for editing. An unknown detail value falls back to summary. estimate: true adds `estimate`, how long a run of this version takes and what it spends (time, tokens, cost, the nodes with no estimate) from comparable past runs in the workspace: the read-only way to show it before a start.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn playbook_get(
        &self,
        Parameters(PlaybookGetArgs {
            id,
            version,
            workspace,
            detail,
            estimate,
        }): Parameters<PlaybookGetArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        let got = tools::playbook_get(
            &root,
            &id,
            version.as_deref(),
            tools::DetailMode::from_arg(detail.as_deref()),
        );
        // The estimate is an optional extra: a failure to read it is
        // reported inside it, the definition is returned all the same.
        to_call_tool_result(got.map(|mut out| {
            if estimate == Some(true) {
                out["estimate"] = tools::budget::playbook_estimate(&root, &id, version.as_deref())
                    .unwrap_or_else(|e| json!({ "error": e.to_string() }));
            }
            out
        }))
    }

    #[tool(
        description = "Validate a playbook definition and return the list of issues",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn playbook_validate(
        &self,
        Parameters(PlaybookIdArgs { id, workspace }): Parameters<PlaybookIdArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::playbook_validate(&root, &id))
    }

    #[tool(
        description = "Create a new playbook or a new minor version of an existing one",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn playbook_create(
        &self,
        Parameters(PlaybookWriteArgs { id, yaml }): Parameters<PlaybookWriteArgs>,
    ) -> CallToolResult {
        to_call_tool_result(tools::playbook_create(&self.root, &id, &yaml))
    }

    #[tool(
        description = "Update an existing playbook by creating a new minor version",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn playbook_update(
        &self,
        Parameters(PlaybookWriteArgs { id, yaml }): Parameters<PlaybookWriteArgs>,
    ) -> CallToolResult {
        to_call_tool_result(tools::playbook_update(&self.root, &id, &yaml))
    }

    #[tool(
        description = "Soft-delete a playbook by moving it to trash",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn playbook_delete(
        &self,
        Parameters(PlaybookIdArgs { id, workspace }): Parameters<PlaybookIdArgs>,
    ) -> CallToolResult {
        // Deletion is a local admin operation; we do not silently perform a
        // cross-workspace mutation in the current workspace - we refuse it.
        if workspace.is_some() {
            return to_call_tool_result(Ok(json!({
                "error": "workspace_param_unsupported",
                "detail": "run playbook_delete in the target workspace",
            })));
        }
        to_call_tool_result(tools::playbook_delete(&self.root, &id))
    }

    #[tool(
        description = "List the project's deleted playbooks (the trash), newest deletion first. Each entry has `name` (the handle playbook_trash_restore takes), `id`, `deleted_at_ms`, `versions`, `current`, and `conflict`: true when a playbook with that id exists again, so a restore would be refused until it is deleted or renamed",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn playbook_trash_list(
        &self,
        Parameters(WorkspaceArg { workspace }): Parameters<WorkspaceArg>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::playbook_trash_list(&root))
    }

    #[tool(
        description = "Restore a deleted playbook from the trash with all its versions. `name` is a trash entry name from playbook_trash_list or a playbook id (its latest deletion). The restored current version is approved like any save through apb when it has no scripts; with scripts it keeps the approval its digest already had. Refused with a conflict when a playbook with that id exists again; current workspace only",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn playbook_trash_restore(
        &self,
        Parameters(TrashRestoreArgs { name }): Parameters<TrashRestoreArgs>,
    ) -> CallToolResult {
        to_call_tool_result(tools::playbook_trash_restore(&self.root, &name))
    }
}
