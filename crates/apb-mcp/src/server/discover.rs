//! MCP tool handlers for history discovery: the step sequences the user's
//! past agent sessions repeat, offered as playbook drafts. The logic and
//! the run-step guard live in `crate::tools::discover`, shared with `apb
//! discover` and the HTTP API. Every handler reads files and may wait on a
//! lock, so it runs off the async runtime.

use apb_core::step_env::StepCaller;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

use super::args::*;
use super::quality::off_runtime;
use super::{WfMcp, to_call_tool_result};
use crate::tools::discover::{self, DiscoverArgs, ProjectPick, SaveRequest};
use crate::tools::grade::to_value;

#[tool_router(router = discover_router, vis = "pub(crate)")]
impl WfMcp {
    #[tool(
        description = "Look for step sequences the user's past agent sessions repeat (Claude Code and Codex history on this machine), only when the user asks for that, never on your own initiative. Reads only the named projects' sessions, makes no model or network call, and stores the result. Each suggestion has argument-free steps, support (sessions, days, projects), effects, scrubbed examples of what was asked, sources (root, file, lines) and a draft. Show the user each suggestion's sources, compare it with suppressed_suggestions by meaning, then save one with playbook_discover_save or record a decline with playbook_discover_dismiss on the user's word. Refused from a run step.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    pub(crate) async fn playbook_discover(
        &self,
        Parameters(a): Parameters<PlaybookDiscoverArgs>,
    ) -> CallToolResult {
        let current = (*self.root).clone();
        let projects = match a.projects {
            None => vec![ProjectPick::Root(current)],
            Some(list) if list.is_empty() => vec![ProjectPick::Root(current)],
            Some(list) => list
                .into_iter()
                .map(|p| {
                    if p == "current" {
                        ProjectPick::Root(current.clone())
                    } else {
                        ProjectPick::Workspace(p)
                    }
                })
                .collect(),
        };
        let args = DiscoverArgs {
            projects,
            roots: a.roots.unwrap_or_default(),
            extra_root: None,
            since_days: a.since_days,
        };
        off_runtime(move || {
            discover::run_discover(&args, &StepCaller::from_env()).and_then(to_value)
        })
        .await
    }

    #[tool(
        description = "The stored results of playbook_discover, newest first, or with result_id one stored result in full, with the declines that hold now (suppressed_suggestions). Only when the user asks about past discoveries. Refused from a run step.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    pub(crate) async fn playbook_discover_results(
        &self,
        Parameters(a): Parameters<PlaybookDiscoverResultsArgs>,
    ) -> CallToolResult {
        off_runtime(move || {
            let step = StepCaller::from_env();
            match a.result_id {
                Some(id) => discover::result_get(&id, &step).and_then(to_value),
                None => discover::results_list(&step).and_then(to_value),
            }
        })
        .await
    }

    #[tool(
        description = "Save one suggestion of a stored playbook_discover result as a draft playbook, only on the user's word, in the scope the user picked (project or global). yaml: the draft as you rewrote it (a better title or prompt), else the suggestion's own. Goes through the same secret and duplicate checks as playbook_capture; the draft records created_by discover (edited true when you rewrote it), and its own draft names the first profile where it is saved. A saved suggestion is not offered again where it was saved (everywhere for global), whatever id you give it. Refused from a run step.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn playbook_discover_save(
        &self,
        Parameters(a): Parameters<PlaybookDiscoverSaveArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(a.workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        off_runtime(move || {
            discover::discover_save(
                &root,
                SaveRequest {
                    id: &a.result_id,
                    slug: &a.slug,
                    scope: &a.scope,
                    yaml: a.yaml.as_deref(),
                },
                &StepCaller::from_env(),
            )
        })
        .await
    }

    #[tool(
        description = "Record the user's decline of one suggestion of a stored playbook_discover result: kind soft for not now, kind hard for an explicit never. Later discoveries leave it out. scope project by default, global only for everywhere-wording. Refused from a run step.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn playbook_discover_dismiss(
        &self,
        Parameters(a): Parameters<PlaybookDiscoverDismissArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(a.workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        off_runtime(move || {
            discover::discover_dismiss(
                &root,
                &a.result_id,
                &a.slug,
                &a.kind,
                a.scope.as_deref(),
                &StepCaller::from_env(),
            )
        })
        .await
    }
}
