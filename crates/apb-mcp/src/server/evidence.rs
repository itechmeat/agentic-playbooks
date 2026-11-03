//! MCP tool handler for reading through an attempt's evidence session
//! (`run_evidence_read`). The logic lives in `crate::tools::evidence`,
//! shared with `apb evidence read`. The handler reads and writes the
//! session file under its lock, so it runs off the async runtime. It is
//! not served by a read-only server: every read spends the session's
//! budget and writes its trace.

use apb_core::step_env::StepCaller;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

use super::args::*;
use super::{WfMcp, to_call_tool_result};
use crate::tools;
use crate::tools::evidence::ReadArgs;

#[tool_router(router = evidence_router, vis = "pub(crate)")]
impl WfMcp {
    #[tool(
        description = "Read the results of earlier steps through an attempt's evidence session, for an agent_task with evidence: session is the id the step's prompt names (also APB_EVIDENCE_SESSION). op outline lists the views (n:<view>); children lists a view's record references with a snippet (view, from, count); read returns one record (ref), whole or a character span; page returns consecutive records of a view, each whole (view, from, count); search returns the references whose text holds phrase (view optional). Only a record returned whole by read or page may be cited in the step's claims (citable: true); outline, children and search only show where to look. Every returned character counts against the session's budget. Refused after the attempt ended and from a step of another run.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    pub(crate) async fn run_evidence_read(
        &self,
        Parameters(a): Parameters<RunEvidenceReadArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(a.workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        let args = ReadArgs {
            op: a.op,
            r#ref: a.r#ref,
            span: a.span,
            view: a.view,
            from: a.from,
            count: a.count,
            phrase: a.phrase,
        };
        let session = a.session;
        super::quality::off_runtime(move || {
            tools::evidence::run_evidence_read(&root, &session, &args, &StepCaller::from_env())
        })
        .await
    }
}
