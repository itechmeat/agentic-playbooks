//! MCP tool handlers for known problems: a playbook's problems
//! (`playbook_known_problems`, read-only, allowed anywhere), a person's
//! decision relayed by the host (`known_problem_decide`) and a recheck
//! (`known_problem_recheck`, by the decision model or by the host). The
//! logic and the run-step guard live in `crate::tools::problems`, shared
//! with `apb problems` and the HTTP API. Every handler reads or writes
//! files under a lock, so it runs off the async runtime.

use apb_core::step_env::StepCaller;
use apb_engine::known_problems::RecheckBy;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

use super::args::*;
use super::quality::{detail_of, invalid, off_runtime};
use super::{WfMcp, to_call_tool_result};
use crate::tools;
use crate::tools::grade::to_value;
use crate::tools::problems::{Target, action_of, proposal_of};

/// Who decided or rechecked, as the store keeps it.
const BY: &str = "mcp";

#[tool_router(router = problems_router, vis = "pub(crate)")]
impl WfMcp {
    #[tool(
        description = "A playbook's known problems: trouble with its tools that earlier runs reported (a connector function that keeps failing, a skill or a file that misleads), each with its scope, symptom, workaround, how many runs saw it, its status (open, closed, dismissed), its latest recheck, whether a run starting now is shown it (significant), whether a person should look at it (needs_look) and whether its source changed since it was checked (source_now). With problem: that one problem and the passages of its source that are new since it was seen. Read-only.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn playbook_known_problems(
        &self,
        Parameters(KnownProblemsArgs {
            id,
            scope,
            problem,
            workspace,
        }): Parameters<KnownProblemsArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        off_runtime(move || {
            let t = Target::resolve(&root, &id, scope.as_deref())?;
            match problem {
                Some(p) => tools::problems::problem_detail(&root, &t, &p).and_then(to_value),
                None => tools::problems::problems_view(&root, &t).and_then(to_value),
            }
        })
        .await
    }

    #[tool(
        description = "Record a person's decision on a known problem, only on the person's explicit word in this conversation: confirm (shown to runs even before it recurs), dismiss (never shown), reopen, or close with quote. A close needs quote copied exactly from the new text of the problem's source (see playbook_known_problems with problem: changed_passages) that the old text does not have; anything else is refused. Never decide or close a problem on your own judgment. Refused from a run step, and while a run of the playbook waits on a host task (this session is doing its work).",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn known_problem_decide(
        &self,
        Parameters(a): Parameters<KnownProblemDecideArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(a.workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        let action = match action_of(&a.action, a.quote) {
            Ok(x) => x,
            Err(e) => return invalid(detail_of(e)),
        };
        off_runtime(move || {
            let t = Target::resolve(&root, &a.id, a.scope.as_deref())?;
            tools::problems::outside_host_task(&root, &t)?;
            tools::problems::problem_decide(
                &root,
                &t,
                &a.problem,
                action,
                a.reason.as_deref(),
                BY,
                &StepCaller::from_env(),
            )
            .and_then(to_value)
        })
        .await
    }

    #[tool(
        description = "Recheck a known problem whose source changed. by model: ask the project's decision model (only when decisions.yaml turns its problem_recheck use on; capped per day). by host: your own reading of the changed passages (playbook_known_problems with problem) with verdict holds, closed or undetermined; a closed verdict needs quote copied exactly from one changed passage and is stored as closed_proposed, hidden until a person decides. verdict, quote and why apply to by host only. Returns the stored recheck record. Refused from a run step, and while a run of the playbook waits on a host task.",
        annotations(destructive_hint = false)
    )]
    pub(crate) async fn known_problem_recheck(
        &self,
        Parameters(a): Parameters<KnownProblemRecheckArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(a.workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        let by = match (a.by.as_str(), a.verdict.as_deref()) {
            ("model", None) if a.quote.is_none() && a.why.is_none() => RecheckBy::Model,
            ("model", _) => {
                return invalid(
                    "verdict, quote and why apply to by host only: the model reads the changed passages itself"
                        .into(),
                );
            }
            ("host", None) => {
                return invalid("by host needs verdict: holds, closed or undetermined".into());
            }
            ("host", Some(v)) => match proposal_of(v, a.quote, a.why) {
                Ok(p) => RecheckBy::Host(p),
                Err(e) => return invalid(detail_of(e)),
            },
            (other, _) => return invalid(format!("unknown by `{other}`: use model or host")),
        };
        off_runtime(move || {
            let t = Target::resolve(&root, &a.id, a.scope.as_deref())?;
            tools::problems::outside_host_task(&root, &t)?;
            tools::problems::problem_recheck(&root, &t, &a.problem, by, BY, &StepCaller::from_env())
                .and_then(to_value)
        })
        .await
    }
}
