//! MCP server on rmcp (stdio): a thin wrapper over `tools::*`.
//!
//! Registers the nine read/run tools of Phase 3, the three write tools of
//! Phase 5a, and the eight supervisor tools of Phase 4b (gated by token and
//! capability). Cancellation is available two ways: `supervisor_run_abort`
//! for a supervisor holding a session token, and the operator-facing
//! `run_stop`, which additionally finalizes a run whose driver has died.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, ServiceExt, tool_handler};
use serde_json::{Value, json};

use crate::tools::{self, ToolError};

mod args;
pub use args::*;

mod playbook;
mod profile;
mod run;
mod supervisor;

/// A supervisor session: which run it observes and which capabilities were
/// granted at the moment the token was minted (minted by
/// `playbook_run(supervise: "self")`).
#[derive(Clone)]
struct SupervisorSession {
    run_id: String,
    capabilities: Vec<String>,
}

/// Server state: the project root (resolved from cwd by the caller) and the
/// table of supervisor sessions, which live for the lifetime of the server
/// process.
#[derive(Clone)]
pub struct WfMcp {
    root: Arc<PathBuf>,
    // The routes this server serves: the call_tool/list_tools dispatch that
    // #[tool_handler] generates reads this field (not a fresh router), so a
    // server built with fewer routes (`for_supervisor`) really serves fewer.
    tool_router: ToolRouter<Self>,
    /// Token fingerprint -> session. The token is the supervisor tools'
    /// credential; the table keys on its SHA-256 so the raw value is not kept.
    sessions: Arc<Mutex<HashMap<String, SupervisorSession>>>,
    /// Plan nonces already consumed (spec 7): guarantees single-use for a
    /// plan_token within the lifetime of the server process.
    used_nonces: Arc<Mutex<HashSet<String>>>,
    /// This server serves a background supervisor agent (see
    /// [`WfMcp::for_supervisor`]).
    supervisor_role: bool,
}

/// Longest single blocking slice of a server-side wait (`run_wait`,
/// `supervisor_wait_event`). Between slices the server sends a progress
/// notification, when the caller supplied a progress token, and stops early if
/// the caller cancelled the request. Neither costs the caller a model turn.
pub(crate) const WAIT_SLICE: std::time::Duration = std::time::Duration::from_secs(15);

/// [`WAIT_SLICE`], overridable through `APB_WAIT_SLICE_MS` so tests can
/// observe the progress keep-alive in milliseconds. A malformed value falls
/// back to the default: it is a keep-alive knob, not a correctness input.
fn wait_slice() -> std::time::Duration {
    std::env::var("APB_WAIT_SLICE_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or(WAIT_SLICE)
}

/// Runs a blocking wait in slices of at most [`wait_slice`] on the blocking
/// pool. `step(slice)` waits up to `slice` and returns its result plus whether
/// the wait is over; a slice that only timed out is retried until `total`
/// runs out, and its result is what the caller gets on the final timeout.
pub(crate) async fn sliced_wait<T, F>(
    ctx: &rmcp::service::RequestContext<rmcp::RoleServer>,
    total: std::time::Duration,
    label: String,
    step: F,
) -> Result<T, ToolError>
where
    T: Send + 'static,
    F: Fn(std::time::Duration) -> Result<(T, bool), ToolError> + Send + Sync + 'static,
{
    let step = Arc::new(step);
    let deadline = std::time::Instant::now() + total;
    let token = ctx.meta.get_progress_token();
    let mut ticks = 0u64;
    let max_slice = wait_slice();
    loop {
        let slice = deadline
            .saturating_duration_since(std::time::Instant::now())
            .min(max_slice);
        let f = Arc::clone(&step);
        let (value, done) = tokio::task::spawn_blocking(move || f(slice))
            .await
            .map_err(|e| ToolError::Engine(format!("wait task failed: {e}")))??;
        if done || std::time::Instant::now() >= deadline {
            return Ok(value);
        }
        if ctx.ct.is_cancelled() {
            return Err(ToolError::Engine(format!("{label}: request cancelled")));
        }
        if let Some(token) = &token {
            ticks += 1;
            // Best-effort keep-alive: a failed notification must not end the wait.
            let _ = ctx
                .peer
                .notify_progress(
                    rmcp::model::ProgressNotificationParam::new(token.clone(), ticks as f64)
                        .with_message(label.clone()),
                )
                .await;
        }
    }
}

/// Convert the result of a pure tool function into an MCP response.
///
/// Both `ToolError::NotFound` and `ToolError::Engine` become a tool-level
/// error (`CallToolResult::error`) with a readable message - this is what
/// the calling MCP client will see. Protocol-level JSON-RPC errors are not
/// used here: they hide the message from the client.
fn to_call_tool_result(result: Result<Value, ToolError>) -> CallToolResult {
    match result {
        Ok(value) => match ContentBlock::json(value) {
            Ok(block) => CallToolResult::success(vec![block]),
            Err(err) => CallToolResult::error(vec![ContentBlock::text(err.message.into_owned())]),
        },
        Err(err) => CallToolResult::error(vec![ContentBlock::text(err.to_string())]),
    }
}

/// Merges the policy gate's non-fatal consent-time warnings (finding 11 of
/// issue #42 - a bound connector with zero configured accounts) into a
/// successful run response object, so the caller can show them to the user
/// before the run proceeds. A no-op for an error, a non-object payload, or an
/// empty warning list, and it never converts a permit into a refusal.
fn with_warnings(
    result: Result<Value, ToolError>,
    warnings: &[String],
) -> Result<Value, ToolError> {
    let mut result = result;
    if !warnings.is_empty()
        && let Ok(Value::Object(obj)) = &mut result
    {
        obj.insert("warnings".to_string(), json!(warnings));
    }
    result
}

fn capability_for_tool(name: &str) -> &'static str {
    match name {
        // `run_answer`'s supervisor-token path (spec 2026-07-20-interactive-
        // nodes, Task 8): answering a question is an observational act from
        // the supervisor session's point of view (it does not retry, patch,
        // or otherwise alter the run's control flow), so it shares
        // `supervisor_wait_event`'s capability rather than `retry`'s.
        "supervisor_wait_event" | "supervisor_run_inspect" | "supervisor_report" | "run_answer" => {
            "observe"
        }
        "supervisor_node_retry"
        | "supervisor_run_continue_from"
        | "supervisor_run_pause"
        | "supervisor_run_abort"
        | "supervisor_context_append"
        // Interrupting a wedged attempt is a control-flow intervention that
        // forces the attempt boundary so retry/fallback/patch can proceed - it
        // belongs with `retry`, the same capability its sibling
        // `supervisor_node_retry` requires.
        | "supervisor_interrupt_attempt" => "retry",
        // Rebinding a node's executor profile mid-run (issue #45 finding 5) is
        // its own capability: it re-runs the trust gate for a new bundle and
        // changes the run's effective binding, a strictly larger act than a
        // retry, so a policy can grant retry without granting rebind.
        "supervisor_rebind_profile" => "rebind",
        "supervisor_patch_playbook" => "patch_playbook",
        // An unknown tool name must not pass the gate under any policy.
        _ => "unknown",
    }
}

impl WfMcp {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root: Arc::new(root),
            tool_router: Self::tool_router(),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            used_nonces: Arc::new(Mutex::new(HashSet::new())),
            supervisor_role: false,
        }
    }

    /// The server a background supervisor agent connects to. A supervisor's
    /// authority is its capability set on the token-bearing `supervisor_*`
    /// tools, so this server offers only those and the read-only tools: the
    /// operator's run control (`run_stop`, `run_resume`, `review_decide`),
    /// authoring and run starts take no token and would bypass the
    /// capabilities. `run_answer` stays for its token path (answered by the
    /// supervisor); its `run_id` path, which answers as the human, is refused.
    pub fn for_supervisor(root: PathBuf) -> Self {
        let mut server = Self::new(root);
        let refused: Vec<String> = server
            .tool_router
            .list_all()
            .into_iter()
            .filter(|t| {
                let read_only = t.annotations.as_ref().and_then(|a| a.read_only_hint) == Some(true);
                !(t.name.starts_with("supervisor_") || t.name == "run_answer" || read_only)
            })
            .map(|t| t.name.to_string())
            .collect();
        for name in refused {
            server.tool_router.remove_route(&name);
        }
        server.supervisor_role = true;
        server
    }

    /// Mints a new supervisor session token (256 random bits, see
    /// `apb_engine::mint_supervisor_token`) and stores the session in the
    /// server's table under the token's fingerprint, never the token itself.
    ///
    /// Additionally makes a best-effort persist of the session to disk
    /// (`write_supervisor_session`) so a separate `apb mcp` process (for a
    /// background agent, Task 4) can resolve the same token without access
    /// to this process's in-memory session table. If the run directory does
    /// not exist yet at minting time, the write is simply skipped - the
    /// in-memory path still works for this same process.
    fn mint_token(&self, run_id: String, capabilities: Vec<String>) -> Result<String, ToolError> {
        let token = apb_engine::mint_supervisor_token()
            .map_err(|e| ToolError::Engine(format!("cannot mint a supervisor token: {e}")))?;
        // best-effort: if the run directory does not exist yet, the write fails - fine,
        // the in-memory path still resolves the token for this same process.
        let _ = apb_engine::write_supervisor_session(&self.root, &run_id, &token, &capabilities);
        self.sessions.lock().unwrap().insert(
            apb_engine::supervisor_token_fingerprint(&token),
            SupervisorSession {
                run_id,
                capabilities,
            },
        );
        Ok(token)
    }

    /// Resolves a supervisor token to a run_id and checks that the session
    /// has the capability required for this tool. The session mutex is held
    /// only for the duration of the lookup itself: the run_id is cloned and
    /// the lock is released before returning, so the calling code can then
    /// block-wait or make an arbitrarily long tools::* call without delaying
    /// other sessions.
    ///
    /// On an in-memory table miss, a disk fallback is performed
    /// (`find_session_by_token`): this is the path by which a separate
    /// `apb mcp` process (background agent, Task 4) validates a token issued
    /// by the process that started the run.
    fn resolve_session(&self, token: &str, tool_name: &str) -> Result<String, ToolError> {
        {
            let sessions = self.sessions.lock().unwrap();
            if let Some(session) = sessions.get(&apb_engine::supervisor_token_fingerprint(token)) {
                let cap = capability_for_tool(tool_name);
                if !session.capabilities.iter().any(|c| c == cap) {
                    return Err(ToolError::Engine(format!("capability `{cap}` not granted")));
                }
                return Ok(session.run_id.clone());
            }
        }
        if let Ok(Some((run_id, caps))) = apb_engine::find_session_by_token(&self.root, token) {
            let cap = capability_for_tool(tool_name);
            if !caps.iter().any(|c| c == cap) {
                return Err(ToolError::Engine(format!("capability `{cap}` not granted")));
            }
            return Ok(run_id);
        }
        Err(ToolError::Engine(
            "invalid or unknown supervisor token".to_string(),
        ))
    }

    /// Resolves the root for a read operation (spec 7): None - the current
    /// workspace, Some(id) - via the project registry. On unavailability
    /// returns a structured (JSON) error for the agent - the registry's
    /// 404 machinery kicks in.
    fn effective_root(&self, workspace: Option<&str>) -> Result<PathBuf, Value> {
        match workspace {
            None => Ok((*self.root).clone()),
            Some(id) => match apb_core::projects::resolve_root(id) {
                Ok(p) => Ok(p),
                Err(apb_core::projects::ProjectAccessError::Unreachable { workspace_id, path }) => {
                    Err(
                        json!({ "error": "workspace_unreachable", "workspace": workspace_id, "path": path }),
                    )
                }
                Err(apb_core::projects::ProjectAccessError::Unknown(w)) => {
                    Err(json!({ "error": "workspace_unknown", "workspace": w }))
                }
            },
        }
    }

    /// The `playbook_run` branch for `supervise: "self"`: starts the
    /// playbook in the background, computes capabilities from the
    /// supervisor policy, and mints a token for the calling session.
    #[allow(clippy::too_many_arguments)]
    fn run_supervised_self(
        &self,
        id: String,
        version: Option<String>,
        params: BTreeMap<String, String>,
        instruction: Option<String>,
        expected_digest: String,
        expected_bundles: BTreeMap<String, String>,
        expected_children: BTreeMap<String, apb_engine::run_config::ChildExpectation>,
        expected_connectors: BTreeMap<String, String>,
        expected_connector_accounts: BTreeMap<String, String>,
        continued_from: Option<String>,
        worktree: Option<String>,
        warnings: Vec<String>,
    ) -> CallToolResult {
        let capabilities = match tools::supervisor_capabilities(&self.root, &id, version.as_deref())
        {
            Ok(caps) => caps,
            Err(e) => return to_call_tool_result(Err(e)),
        };
        let started = tools::playbook_run_supervised(
            &self.root,
            &id,
            version.as_deref(),
            params,
            instruction,
            Some(expected_digest),
            Some(expected_bundles),
            Some(expected_children),
            expected_connectors,
            expected_connector_accounts,
            continued_from,
            worktree,
        );
        let value = match started {
            Ok(v) => v,
            Err(e) => return to_call_tool_result(Err(e)),
        };
        let run_id = match value["run_id"].as_str() {
            Some(r) => r.to_string(),
            None => {
                return to_call_tool_result(Err(ToolError::Engine(
                    "playbook_run_supervised did not return a run_id".to_string(),
                )));
            }
        };
        let token = match self.mint_token(run_id.clone(), capabilities.clone()) {
            Ok(t) => t,
            Err(e) => return to_call_tool_result(Err(e)),
        };
        to_call_tool_result(with_warnings(
            Ok(json!({
                "run_id": run_id,
                "supervisor_token": token,
                "capabilities": capabilities,
            })),
            &warnings,
        ))
    }

    /// Combined tool router: the per-domain routers (defined in the
    /// `playbook` / `run` / `profile` / `supervisor` submodules) merged into
    /// one. `#[tool_handler]` on the `ServerHandler` impl calls this.
    fn tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        Self::playbook_router()
            + Self::run_router()
            + Self::profile_router()
            + Self::supervisor_router()
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for WfMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "agentic-playbooks",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(crate::instructions::TIER0)
    }
}

/// Bring up the stdio MCP server and serve until the client closes stdin.
pub async fn serve_stdio(root: PathBuf) -> anyhow::Result<()> {
    let supervisor = std::env::var(apb_engine::adapter::MCP_ROLE_ENV).as_deref()
        == Ok(apb_engine::adapter::MCP_ROLE_SUPERVISOR);
    let service = if supervisor {
        WfMcp::for_supervisor(root)
    } else {
        WfMcp::new(root)
    };
    let server = service.serve(rmcp::transport::stdio()).await?;
    server.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests;
