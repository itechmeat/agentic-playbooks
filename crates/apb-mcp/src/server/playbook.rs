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
        description = "Compact structured catalog of playbooks (project and global scope) with trigger, effects, trust and shadowing. Call once per task when matching a user request to a playbook. Pass revision to skip the body when unchanged.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn playbook_catalog(
        &self,
        Parameters(PlaybookCatalogArgs {
            revision,
            limit,
            workspace,
        }): Parameters<PlaybookCatalogArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::playbook_catalog(
            &root,
            workspace.as_deref(),
            revision.as_deref(),
            limit,
        ))
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
        description = "Adoption readiness of a playbook (or all project playbooks): resolvable profiles, present skills, trusted bundles, and model availability per the free detection. Read-only. Model availability is only asserted when detection authority is Full.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn playbook_adopt_report(
        &self,
        Parameters(AdoptReportArgs { id }): Parameters<AdoptReportArgs>,
    ) -> CallToolResult {
        to_call_tool_result(crate::advisory_tools::playbook_adopt_report(
            &self.root,
            id.as_deref(),
        ))
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
        description = "Record that the user declined a save-as-playbook suggestion so it is not offered again (with a TTL).",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn suggestion_dismiss(
        &self,
        Parameters(SuggestionDismissArgs { pattern, ttl_days }): Parameters<SuggestionDismissArgs>,
    ) -> CallToolResult {
        to_call_tool_result(tools::suggestion_dismiss(&pattern, ttl_days))
    }

    #[tool(
        description = "Activate a playbook after a successful trial or explicit user confirmation: lifecycle becomes active and the current digest becomes trusted.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn playbook_approve(
        &self,
        Parameters(PlaybookApproveArgs { id, version, scope }): Parameters<PlaybookApproveArgs>,
    ) -> CallToolResult {
        let scope = scope.as_deref().unwrap_or("project");
        to_call_tool_result(tools::playbook_approve(
            &self.root,
            &id,
            version.as_deref(),
            scope,
        ))
    }

    #[tool(
        description = "Get a playbook definition by id and optional version",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn playbook_get(
        &self,
        Parameters(PlaybookGetArgs {
            id,
            version,
            workspace,
        }): Parameters<PlaybookGetArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::playbook_get(&root, &id, version.as_deref()))
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
        let res = tools::playbook_create(&self.root, &id, &yaml);
        // Local creation via the tool = local approval (spec 3.1): such a
        // playbook is trusted and passes the run gate without acknowledge.
        if let Ok(v) = &res
            && let Some(ver) = v["version"].as_str()
        {
            tools::approve_local(&self.root, &id, ver);
        }
        to_call_tool_result(res)
    }

    #[tool(
        description = "Update an existing playbook by creating a new minor version",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn playbook_update(
        &self,
        Parameters(PlaybookWriteArgs { id, yaml }): Parameters<PlaybookWriteArgs>,
    ) -> CallToolResult {
        let res = tools::playbook_update(&self.root, &id, &yaml);
        if let Ok(v) = &res
            && let Some(ver) = v["version"].as_str()
        {
            tools::approve_local(&self.root, &id, ver);
        }
        to_call_tool_result(res)
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
}
