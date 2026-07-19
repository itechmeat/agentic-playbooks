//! MCP tool handlers for the profile domain. Split out of `server` so the
//! handler surface stays navigable; each block registers a named router that
//! `server::WfMcp::tool_router` combines. Handler logic delegates to
//! `crate::tools` / `profile_tools` / `advisory_tools` / `catalog`.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

use super::args::*;
use super::{WfMcp, to_call_tool_result};
use crate::tools::ToolError;

#[tool_router(router = profile_router, vis = "pub(crate)")]
impl WfMcp {
    #[tool(
        description = "List agent profiles (project and global scope) with trust status. A node references an executor only through a profile.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn profile_list(
        &self,
        Parameters(ProfileListArgs { workspace }): Parameters<ProfileListArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(crate::profile_tools::profile_list(&root))
    }

    #[tool(
        description = "List installed connectors an agent_task node can bind (spec connectors): each with version, trust state, exposed function names, and configured account names. No account field or secret values. Read this before writing a node `connectors` binding.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn connectors_list(
        &self,
        Parameters(WorkspaceArg { workspace }): Parameters<WorkspaceArg>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(crate::tools::connectors_list(&root))
    }

    #[tool(
        description = "Get a profile's full content (profile.yaml + SOUL.md) and digests.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn profile_get(
        &self,
        Parameters(ProfileGetArgs { name, scope }): Parameters<ProfileGetArgs>,
    ) -> CallToolResult {
        to_call_tool_result(crate::profile_tools::profile_get(&self.root, &name, &scope))
    }

    #[tool(
        description = "Create or update an agent profile (agent+model+fallbacks, SOUL, skills). Update requires expected_digest (optimistic concurrency). Auto-approves the resulting bundle. Only for the current workspace. Create a project profile a directly requested playbook needs without extra questions; ask the user before an unexpected global mutation or an initiative-driven change to a profile other playbooks use.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn profile_write(
        &self,
        Parameters(ProfileWriteArgs {
            name,
            scope,
            description,
            soul_md,
            skills,
            agent,
            model,
            fallbacks,
            soul,
            expected_digest,
        }): Parameters<ProfileWriteArgs>,
    ) -> CallToolResult {
        let executor = crate::profile_tools::ExecutorInput {
            agent,
            model,
            fallbacks: fallbacks.into_iter().map(|f| (f.agent, f.model)).collect(),
        };
        let soul_requirement = match crate::profile_tools::parse_soul_requirement(soul.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Err(ToolError::Engine(e))),
        };
        to_call_tool_result(crate::profile_tools::profile_write(
            &self.root,
            crate::profile_tools::ProfileWrite {
                name,
                scope,
                description,
                soul_md,
                skills: crate::profile_tools::skill_refs(&skills),
                executor,
                expected_digest,
                soul_requirement,
            },
        ))
    }

    #[tool(
        description = "Copy a profile between scopes (project <-> global); the source stays. Deleting the source is a separate profile_delete. Only for the current workspace.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn profile_move(
        &self,
        Parameters(ProfileMoveArgs { name, from, to }): Parameters<ProfileMoveArgs>,
    ) -> CallToolResult {
        to_call_tool_result(crate::profile_tools::profile_move(
            &self.root, &name, &from, &to,
        ))
    }

    #[tool(
        description = "Delete a profile. Blocked when playbooks of this project reference it, unless force is true. Only for the current workspace.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn profile_delete(
        &self,
        Parameters(ProfileDeleteArgs { name, scope, force }): Parameters<ProfileDeleteArgs>,
    ) -> CallToolResult {
        to_call_tool_result(crate::profile_tools::profile_delete(
            &self.root, &name, &scope, force,
        ))
    }

    #[tool(
        description = "Detect installed coding agents: presence, version, category, and local model/provider/auth hints. Detection is local - apb runs each agent's --version and reads local config, and makes no network request of its own (it does not control a spawned agent's network when apb runs). Cached; pass refresh to re-probe.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn agents_detect(
        &self,
        Parameters(AgentsDetectArgs { refresh }): Parameters<AgentsDetectArgs>,
    ) -> CallToolResult {
        to_call_tool_result(crate::advisory_tools::agents_detect(refresh))
    }

    #[tool(
        description = "How to author agent profiles: format, selection rules, the curated models table with purposes, declared subscriptions, and detected agents. Pull only when working with profiles. Advisory only.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn profile_howto(&self) -> CallToolResult {
        to_call_tool_result(crate::advisory_tools::profile_howto())
    }

    #[tool(
        description = "Record the user's declared agent subscriptions (or that they declined the survey). Writes the local models overlay and onboarding state. Ask the user before changing this on their behalf.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn subscriptions_set(
        &self,
        Parameters(SubscriptionsSetArgs {
            subscriptions,
            declined,
        }): Parameters<SubscriptionsSetArgs>,
    ) -> CallToolResult {
        let mut subs = Vec::with_capacity(subscriptions.len());
        for s in subscriptions {
            if s.agent.trim().is_empty() {
                return to_call_tool_result(Err(ToolError::Engine(
                    "subscription agent must not be empty".into(),
                )));
            }
            // An unknown coverage is a refusal, not a silent Unknown (spec 8.4).
            let coverage = match s.coverage.as_deref() {
                None | Some("unknown") => apb_core::models_table::Coverage::Unknown,
                Some("full") => apb_core::models_table::Coverage::Full,
                Some("partial") => apb_core::models_table::Coverage::Partial,
                Some(other) => {
                    return to_call_tool_result(Err(ToolError::Engine(format!(
                        "invalid coverage `{other}` for `{}` (use full | partial | unknown)",
                        s.agent
                    ))));
                }
            };
            subs.push(apb_core::models_table::Subscription {
                agent: s.agent,
                plan: s.plan,
                coverage,
            });
        }
        to_call_tool_result(crate::advisory_tools::subscriptions_set(subs, declined))
    }
}
