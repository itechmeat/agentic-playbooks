//! The `apb __ask-server` live-question sidecar (spec 2026-07-20-interactive-
//! nodes, Task 10).
//!
//! A hidden stdio MCP server, injected into the coding agent that runs a live
//! interactive `agent_task` node (Task 11 does the injection). It exposes
//! exactly one tool, `ask_user(question, options?) -> string`. On a call it
//! appends the question to the run's `questions.jsonl` channel via
//! `apb_engine::post_question`, then polls `answers.jsonl` until the matching
//! answer arrives and returns its text. While waiting it sends an MCP progress
//! notification on the request's progress token every `progress_interval`
//! (60 s by default) so the agent's idle timer never fires.
//!
//! The sidecar does NOT enforce `question_timeout_seconds`: the drive loop
//! does, and its timeout answer arrives on `answers.jsonl` like any other
//! answer (spec Exact answer semantics). The poll loop is therefore bounded
//! only by the client-side tool timeout (set in the Task 11 injection JSON)
//! and by connection cancellation, never by an internal deadline - a human may
//! genuinely take hours.
//!
//! Only the sidecar posts a live node's question: the drive's own
//! `post_question` fires solely on an `AttemptOutcome::Suspended` reprompt /
//! resume suspension (scheduler.rs), which is a different, non-live mechanism
//! and is further guarded by `node_has_unanswered_channel_question`. A live
//! node parked on a blocking `ask_user` call never takes that branch, so there
//! is no sidecar-vs-drive double post for one question.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use apb_core::registry::is_safe_segment;
use apb_engine::{PostedAnswer, post_question, read_answers_after};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ProgressNotificationParam, ServerCapabilities,
    ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

/// Interval between poll reads of `answers.jsonl`. The channel files are tiny
/// (append-only JSONL), so a 200 ms cadence is cheap and never a busy loop.
const POLL_STEP: Duration = Duration::from_millis(200);

/// Default cadence for the keep-alive progress notification (spec: every
/// 60 s). Overridable via `APB_ASK_PROGRESS_SECS` so a test can shorten it
/// rather than wait a real minute (`0` fires on every poll).
const DEFAULT_PROGRESS_SECS: u64 = 60;
const PROGRESS_SECS_ENV: &str = "APB_ASK_PROGRESS_SECS";

/// Arguments for the single `ask_user` tool.
#[derive(Debug, Deserialize, JsonSchema)]
struct AskUserArgs {
    /// The question to put to the human operator.
    question: String,
    /// Optional suggested choices to present alongside the question.
    #[serde(default)]
    options: Option<Vec<String>>,
}

/// The live-question sidecar server. One per `(run, node, attempt)`; the tuple
/// is fixed for the process lifetime because a sidecar is spawned per node
/// attempt (Task 11).
#[derive(Clone)]
pub struct AskServer {
    run_dir: Arc<PathBuf>,
    node: Arc<String>,
    attempt: u32,
    progress_interval: Duration,
    // Read by the code `#[tool_handler]` generates (call_tool / list_tools
    // dispatch); the dead_code lint does not see that access.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

/// The answer that resolves the question this sidecar just posted, by the
/// count discipline the scheduler consumes answers with (`nth(answered)`).
///
/// `tail` is the number of answers already present for `node` at the moment
/// the question was posted. The resolving answer is therefore the `tail`-th
/// (0-based) answer for this node - the first one appended after the post.
/// Returning it by position, not by content, is what makes a repeated
/// question on the same node pick up its own distinct answer.
pub fn resolve_answer<'a>(answers: &'a [PostedAnswer], node: &str, tail: usize) -> Option<&'a str> {
    answers
        .iter()
        .filter(|a| a.node == node)
        .nth(tail)
        .map(|a| a.answer.as_str())
}

#[tool_router]
impl AskServer {
    /// Constructs the server for one node attempt. `run_dir` is the resolved,
    /// verified `runs/<id>` directory; `node` and `attempt` come from argv.
    fn new(run_dir: PathBuf, node: String, attempt: u32, progress_interval: Duration) -> Self {
        Self {
            run_dir: Arc::new(run_dir),
            node: Arc::new(node),
            attempt,
            progress_interval,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Ask the human operator a question and block until they answer, returning the answer text. Provide `options` to suggest choices. Use this whenever you need a decision or input only the operator can give."
    )]
    async fn ask_user(
        &self,
        Parameters(AskUserArgs { question, options }): Parameters<AskUserArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> CallToolResult {
        match self.ask(&question, options.unwrap_or_default(), ctx).await {
            Ok(answer) => CallToolResult::success(vec![ContentBlock::text(answer)]),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e.to_string())]),
        }
    }

    /// Posts the question and polls for its answer. Sends a progress
    /// notification every `progress_interval` while waiting, and returns
    /// early if the connection is cancelled (client gone / stdin closed).
    async fn ask(
        &self,
        question: &str,
        options: Vec<String>,
        ctx: RequestContext<RoleServer>,
    ) -> anyhow::Result<String> {
        let run_dir = self.run_dir.as_path();
        let node = self.node.as_str();

        // Count the answers already present for this node BEFORE posting, so
        // the resolving answer is the first one that lands afterward.
        let tail = read_answers_after(run_dir, None)?
            .into_iter()
            .filter(|a| a.node == node)
            .count();

        post_question(run_dir, node, self.attempt, question, options)
            .context("posting the question to questions.jsonl")?;

        let token = ctx.meta.get_progress_token();
        let mut last_progress = Instant::now();
        let mut progress_count = 0u64;

        loop {
            let answers = read_answers_after(run_dir, None)?;
            if let Some(answer) = resolve_answer(&answers, node, tail) {
                return Ok(answer.to_string());
            }

            if last_progress.elapsed() >= self.progress_interval
                && let Some(token) = &token
            {
                progress_count += 1;
                // Best-effort: a failed notification (e.g. the peer is going
                // away) must not abort the wait; the next loop turn observes
                // cancellation and exits cleanly.
                let _ = ctx
                    .peer
                    .notify_progress(
                        ProgressNotificationParam::new(token.clone(), progress_count as f64)
                            .with_message(format!("waiting for an answer to `{node}`")),
                    )
                    .await;
                last_progress = Instant::now();
            }

            tokio::select! {
                _ = tokio::time::sleep(POLL_STEP) => {}
                _ = ctx.ct.cancelled() => {
                    bail!("connection cancelled while waiting for an answer to `{node}`");
                }
            }
        }
    }
}

#[tool_handler]
impl ServerHandler for AskServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("apb-ask-server", env!("CARGO_PKG_VERSION")),
        )
    }
}

/// Reads the progress cadence from `APB_ASK_PROGRESS_SECS`, defaulting to
/// 60 s. A malformed value falls back to the default rather than failing the
/// sidecar - the cadence is a keep-alive knob, not a correctness input.
fn progress_interval_from_env() -> Duration {
    match std::env::var(PROGRESS_SECS_ENV) {
        Ok(v) => match v.trim().parse::<u64>() {
            Ok(secs) => Duration::from_secs(secs),
            Err(_) => Duration::from_secs(DEFAULT_PROGRESS_SECS),
        },
        Err(_) => Duration::from_secs(DEFAULT_PROGRESS_SECS),
    }
}

/// Resolves the run directory from `APB_RUN_DIR` (set by the engine on every
/// agent spawn, inherited by the sidecar as a child of the agent process) and
/// asserts its final segment equals `run`. Fails cleanly when the variable is
/// absent or points at a directory whose basename does not match, so a
/// mis-injected sidecar cannot write to the wrong run.
fn resolve_run_dir(run: &str) -> anyhow::Result<PathBuf> {
    let raw = std::env::var("APB_RUN_DIR").map_err(|_| {
        anyhow!(
            "APB_RUN_DIR is not set: the __ask-server sidecar must run as a child of an agent the engine spawned"
        )
    })?;
    let dir = PathBuf::from(raw);
    let base = dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("APB_RUN_DIR has no final path segment: {}", dir.display()))?;
    if base != run {
        bail!(
            "APB_RUN_DIR basename `{base}` does not match --run `{run}` ({})",
            dir.display()
        );
    }
    Ok(dir)
}

/// Blocking entry point invoked by `apb __ask-server`. Validates `run` / `node`
/// as safe path segments, resolves and verifies the run directory, then serves
/// the stdio MCP server until the client closes stdin (EOF), at which point
/// `serve(...).waiting()` returns and the runtime is dropped - no orphaned
/// poll thread, since the poll loop lives inside the served request future and
/// is cancelled when the transport closes.
pub fn serve(run: &str, node: &str, attempt: u32) -> anyhow::Result<()> {
    if !is_safe_segment(run) {
        bail!("--run `{run}` is not a safe path segment");
    }
    if !is_safe_segment(node) {
        bail!("--node `{node}` is not a safe path segment");
    }
    let run_dir = resolve_run_dir(run)?;
    let rt = tokio::runtime::Runtime::new().context("starting the ask-server tokio runtime")?;
    rt.block_on(serve_async(run_dir, node.to_string(), attempt))
}

async fn serve_async(run_dir: PathBuf, node: String, attempt: u32) -> anyhow::Result<()> {
    let service = AskServer::new(run_dir, node, attempt, progress_interval_from_env());
    let server = service.serve(rmcp::transport::stdio()).await?;
    server.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ans(node: &str, answer: &str) -> PostedAnswer {
        PostedAnswer {
            seq: 0,
            node: node.to_string(),
            answer: answer.to_string(),
            answered_by: "human".to_string(),
        }
    }

    #[test]
    fn resolve_answer_returns_the_first_answer_after_the_tail() {
        // Two prior answers for `ask` (tail = 2); the resolving answer is the
        // third one for that node, ignoring answers for other nodes.
        let answers = vec![
            ans("ask", "old-1"),
            ans("other", "noise"),
            ans("ask", "old-2"),
            ans("ask", "pg"),
        ];
        assert_eq!(resolve_answer(&answers, "ask", 2), Some("pg"));
    }

    #[test]
    fn resolve_answer_is_none_until_the_matching_answer_lands() {
        // tail = 1 but only one answer exists for `ask`: nothing at index 1 yet.
        let answers = vec![ans("ask", "old-1"), ans("other", "x")];
        assert_eq!(resolve_answer(&answers, "ask", 1), None);
    }

    #[test]
    fn resolve_answer_ignores_other_nodes_when_counting() {
        // The tail is a per-node count: answers for `other` never shift `ask`'s
        // index, so tail 0 resolves to `ask`'s very first answer.
        let answers = vec![ans("other", "a"), ans("other", "b"), ans("ask", "go")];
        assert_eq!(resolve_answer(&answers, "ask", 0), Some("go"));
    }
}
