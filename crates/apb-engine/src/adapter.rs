use std::io::{BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;

use apb_core::config::{InvocationDef, PromptVia, SoulDelivery, Transport};
use serde::Deserialize;

use crate::error::EngineError;
use crate::event::{Event, EventPayload};
// Shared with `run_capture` rather than duplicated: the signal-target
// validation it performs is the difference between killing one process group
// and killing every process the user owns, so it must live in exactly one
// place.
use crate::proc::kill_process_group;
use crate::progress::pending_interval_ms;
use crate::state::NodeStatus;

/// Spawns the agent in its own process group so that cancellation/timeout can
/// tear down the whole tree, not just the direct child (a real agent spawns
/// node, MCP servers, tool subprocesses). On Unix - process_group(0) (pgid ==
/// leader pid); on other platforms - no-op (fallback to child.kill()).
fn spawn_in_group(cmd: &mut Command) -> std::io::Result<Child> {
    #[cfg(unix)]
    cmd.process_group(0);
    // ETXTBSY (errno 26) can transiently occur on Linux right after the
    // executable was written. Retry briefly before surfacing the error.
    for _ in 0..20 {
        match cmd.spawn() {
            Ok(child) => return Ok(child),
            Err(e) if e.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => return Err(e),
        }
    }
    cmd.spawn()
}

/// How long the pipe collection gets once the agent process itself is gone.
///
/// NOT a limit on how long an agent may work. Every use of this budget sits
/// AFTER the agent has already exited, so the data is buffered and the normal
/// cost is microseconds. It bounds only the thing the process wait cannot see:
/// a daemonized grandchild still holding the inherited stdout/stderr write
/// ends, which is what actually decides EOF. A healthy agent - including one
/// that worked for hours - cannot reach it, because by then it is gone and its
/// fds are closed. Tests override it via `APB_AGENT_DRAIN_BUDGET_MS`.
fn drain_budget() -> Duration {
    env_duration_ms("APB_AGENT_DRAIN_BUDGET_MS").unwrap_or(Duration::from_secs(10))
}

/// How long an agent may keep running AFTER it has closed its stdout.
///
/// Also not a limit on working time. An agent's actual work is governed by the
/// node's own `timeout_seconds` through `check_cancel_timeout`, inside the
/// streaming loop; this clock does not start until that loop has already ended
/// because stdout reached EOF. An agent that streams for six hours never comes
/// near it. Reaching it means the agent stopped talking to us and then did not
/// exit for five minutes, which is indistinguishable from a wedge, so the tree
/// is killed and the attempt fails as a timeout instead of blocking the drive
/// forever. Deliberately generous: its job is to make an infinite wait finite,
/// not to enforce promptness. Tests override it via `APB_AGENT_EXIT_GRACE_MS`.
fn exit_after_eof_budget() -> Duration {
    env_duration_ms("APB_AGENT_EXIT_GRACE_MS").unwrap_or(Duration::from_secs(300))
}

fn env_duration_ms(key: &str) -> Option<Duration> {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_millis)
}

/// Terminates the agent's whole process tree and reaps the leader. On Unix
/// this sends SIGKILL to the group (`kill(-pgid, ...)`, pgid == pid because of
/// process_group(0) at spawn time) so children are not orphaned; on other
/// platforms - child.kill().
fn kill_process_tree(child: &mut Child) {
    #[cfg(unix)]
    kill_process_group(child.id());
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    // Bounded: the leader has just been SIGKILLed, which no process can catch
    // or ignore, so this reaps a pid that is already dead or dying.
    let _ = child.wait();
}

/// `child.wait()` with a deadline. `None` means the process was still running
/// when `budget` ran out; the caller decides what to do about it.
fn wait_bounded(child: &mut Child, budget: Duration) -> Option<std::io::Result<ExitStatus>> {
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(Ok(status)),
            Ok(None) => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Some(Err(e)),
        }
    }
}

/// `child.wait_with_output()` with a deadline, so a grandchild holding the
/// pipes open cannot stall the drive. The collecting thread is abandoned on a
/// timeout rather than joined: it owns nothing the caller needs, and joining
/// it is the very wait being bounded.
fn wait_with_output_bounded(
    child: Child,
    budget: Duration,
    program: &str,
) -> Result<std::process::Output, (ErrorClass, String)> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(budget) {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err((
            ErrorClass::ProcessExit,
            format!("collect `{program}` output failed: {e}"),
        )),
        Err(_) => Err((
            ErrorClass::Timeout,
            format!(
                "`{program}` exited but its stdout/stderr were still held open {budget:?} later, \
                 so its output could not be collected: a descendant that outlived it inherited \
                 the pipes"
            ),
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    Transport,
    ProcessExit,
    StructuredOutputMissing,
    AgentReportedFailure,
    Timeout,
}

/// Connector isolation applied to every spawned agent process (spec 4.3).
/// `scrub` is the union of env var names referenced by ANY installed connector
/// config (both scopes), removed from the child so a connector token can never
/// be inherited by an agent. `run_dir`/`node_id`, when set, become the
/// `APB_RUN_DIR`/`APB_NODE_ID` context env that `apb connector call` (a child
/// of the agent) reads to locate the run manifest and check its grants. The
/// default is empty: no scrub and no context env, so non-connector spawn paths
/// are untouched.
#[derive(Debug, Clone, Default)]
pub struct ConnectorEnvPolicy {
    pub scrub: Vec<String>,
    pub run_dir: Option<PathBuf>,
    pub node_id: Option<String>,
}

impl ConnectorEnvPolicy {
    /// Removes every scrubbed var from `cmd`'s environment and, when set,
    /// injects the run-context env. Called at every agent spawn site.
    fn apply(&self, cmd: &mut Command) {
        for name in &self.scrub {
            cmd.env_remove(name);
        }
        if let Some(dir) = &self.run_dir {
            cmd.env("APB_RUN_DIR", dir);
        }
        if let Some(node) = &self.node_id {
            cmd.env("APB_NODE_ID", node);
        }
    }
}

pub struct AgentTask<'a> {
    pub prompt: &'a str,
    pub model: &'a str,
    pub workdir: &'a Path,
    /// Maximum agent run time; once it elapses the process is killed and the
    /// attempt is considered timed out. `None` - no limit.
    pub timeout: Option<Duration>,
    /// File for streaming the agent's NDJSON events (acp transport). Each
    /// event is written as a separate line as it arrives - the basis for the
    /// live stream in the web UI and logs. `None` - do not stream (headless
    /// ignores this field).
    pub stream_log: Option<&'a Path>,
    /// System role prompt (profile SOUL). Delivered according to the agent's
    /// capability (native flag or prompt prefix, see `build_command`).
    /// `None`/empty - no role set.
    pub soul: Option<&'a str>,
    /// Whether the engine grants this attempt the agent's non-interactive
    /// permission flags (`InvocationDef::autonomous_args`). Set for agent-task
    /// nodes of an authorized effectful run so the headless agent can perform
    /// the file-writes and network access the run already declared; kept false
    /// for internal, side-effect-free calls (e.g. context compaction).
    pub grant_autonomy: bool,
    /// Connector env isolation (spec 4.3): scrubbed var names + optional run
    /// context env, applied at spawn. Defaults to empty for spawn paths that
    /// carry no connector exposure.
    pub connector_policy: &'a ConnectorEnvPolicy,
    /// Whether this attempt runs an interactive node (spec 2026-07-20). Only
    /// then does the adapter scan stdout for the question marker; on a
    /// non-interactive node the marker line is ordinary output. Internal,
    /// side-effect-free calls (context compaction, finish answers) set `false`.
    pub interactive: bool,
    /// The node id this attempt executes, used to name the node in a
    /// malformed-marker error. Internal calls pass an internal placeholder.
    pub node: &'a str,
    /// The agent id of the executor running this attempt (spec 2026-07-20,
    /// Task 7). Selects the per-agent session-id parser in [`capture_session`]
    /// so the finished attempt's `session` can feed the `resume` transport.
    /// Internal calls pass the agent they invoke (e.g. `claude-code`).
    pub agent: &'a str,
}

/// The marker a `resume`/`reprompt` agent prints on its own stdout line to ask
/// the user a question mid-run (spec 2026-07-20-interactive-nodes). The very
/// next non-empty line carries the question as JSON (`AskedQuestion`). The
/// marker is honored only for interactive nodes (the gate lives in `node.rs`);
/// on a non-interactive node the literal text has no effect.
pub const QUESTION_MARKER: &str = "<<<apb:question>>>";

/// A question an agent asked via the stdout marker protocol. Parsed from the
/// JSON line following [`QUESTION_MARKER`]. `options` is an optional list of
/// suggested answers; free-text answers are always allowed.
#[derive(Debug, Clone, Deserialize)]
pub struct AskedQuestion {
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
}

/// Default per-server tool timeout injected into claude's `--mcp-config` for a
/// live interactive node when the node sets no `question_timeout_seconds` (spec
/// 2026-07-20, Transport: live): about 28 h in ms, so claude's ~30-minute stdio
/// idle timer never cuts off a blocking `ask_user` call while a human takes
/// their time.
pub const LIVE_TOOL_TIMEOUT_MS_DEFAULT: u64 = 28 * 60 * 60 * 1000;

/// Slack added to the injected per-server timeout ON TOP of a node's
/// `question_timeout_seconds` (spec 2026-07-20, Task 11 fix). The sidecar
/// timeout is a BACKSTOP only, never the enforcement mechanism: the engine
/// enforces `question_timeout_seconds` on the drive thread (posting the
/// `default_answer` or failing the attempt), and this slack keeps the sidecar's
/// own timeout strictly larger so drive always wins the race and the timeout
/// semantics stay engine-owned.
pub const LIVE_TOOL_TIMEOUT_BACKSTOP_MS: u64 = 60 * 1000;

/// The per-server `timeout` (ms) injected into claude's `--mcp-config` for a
/// live interactive node. With a `question_timeout_seconds` it is that budget
/// plus [`LIVE_TOOL_TIMEOUT_BACKSTOP_MS`] (strictly larger, so the engine-side
/// enforcement fires first); without one, the large default. Never the
/// enforcement mechanism - a backstop that keeps claude's stdio connection
/// alive if the engine ever missed a tick.
pub fn live_tool_timeout_ms(question_timeout_seconds: Option<u64>) -> u64 {
    match question_timeout_seconds {
        Some(s) => s
            .saturating_mul(1000)
            .saturating_add(LIVE_TOOL_TIMEOUT_BACKSTOP_MS),
        None => LIVE_TOOL_TIMEOUT_MS_DEFAULT,
    }
}

/// The paragraph appended to a live interactive node's prompt (spec 2026-07-20,
/// Transport: live): the `ask_user` tool exists, when to use it, and that
/// free-form questions to the user go through it rather than being answered by
/// assumption. Kept beside the injection so the two never drift.
pub const LIVE_PROMPT_PARAGRAPH: &str = "You have an `ask_user` tool available through the apb MCP server. When you need input from the user before you can proceed, call `ask_user` with your question (and optional `options` listing suggested answers) and wait for the reply. Route any free-form question for the user through `ask_user` rather than assuming an answer.";

/// Live-injection inputs for one attempt (spec 2026-07-20, Transport: live).
/// Built by the drive layer only for an interactive node whose resolved
/// `interaction` is `Live` on claude/claude-code with a resolvable current exe;
/// a resolution failure or a non-claude agent downgrades before this is built.
/// The adapter appends `--mcp-config <json>` pointing claude at the hidden
/// `apb __ask-server` sidecar.
pub struct LiveInject {
    /// Current apb executable (the `apb __ask-server` sidecar host), resolved
    /// by the drive layer the same way `apb __drive-run` resolves it.
    pub exe: PathBuf,
    pub run_id: String,
    pub attempt: u32,
    /// Per-server tool timeout in ms: `question_timeout_seconds*1000` when the
    /// node sets one, else [`LIVE_TOOL_TIMEOUT_MS_DEFAULT`].
    pub timeout_ms: u64,
}

/// Live-attempt hooks handed to [`AgentAdapter::run_cancellable`] (spec
/// 2026-07-20, Transport: live): the sidecar injection plus a per-poll
/// `on_tick` the adapter calls on the DRIVE thread each wait iteration.
/// `on_tick` is where drive observes the question/answer channels and journals
/// `QuestionAsked`/`QuestionAnswered` while the agent blocks in `ask_user`.
/// Because the poll loop runs on the drive thread, this keeps the single writer
/// (drive) as the only thread that ever touches the event log.
pub struct LiveHooks<'a> {
    pub inject: LiveInject,
    pub on_tick: &'a dyn Fn(),
    /// Set by `on_tick` when the open question hit its
    /// `question_timeout_seconds` with no `default_answer` (spec 2026-07-20,
    /// Task 11 fix). The adapter's poll loop then tears down the agent's process
    /// group and returns, so the drive layer can fail the attempt with the
    /// node-named timeout message. Attempt-local (NOT the run-level cancel), so
    /// only THIS node fails; the blocked agent would otherwise wait forever for
    /// an answer that will never come.
    pub abort: &'a AtomicBool,
}

/// Builds the `--mcp-config` JSON injected into claude for a live interactive
/// node (spec 2026-07-20, Transport: live). Points claude at the hidden
/// `apb __ask-server --run <id> --node <node> --attempt <n>` sidecar with a
/// large per-server `timeout` so the blocking `ask_user` call is never cut off
/// by claude's stdio idle timer. No `--strict-mcp-config`: the agent keeps its
/// own configured servers.
pub fn ask_server_mcp_config(
    exe: &Path,
    run_id: &str,
    node: &str,
    attempt: u32,
    timeout_ms: u64,
) -> String {
    serde_json::json!({
        "mcpServers": {
            "apb": {
                "command": exe.to_string_lossy(),
                "args": [
                    "__ask-server",
                    "--run",
                    run_id,
                    "--node",
                    node,
                    "--attempt",
                    attempt.to_string(),
                ],
                "timeout": timeout_ms,
            }
        }
    })
    .to_string()
}

/// The `ts` of the currently-open (asked-but-unanswered) question for `node`,
/// i.e. the last `QuestionAsked` when the node carries more asked than answered
/// events (spec 2026-07-20, Transport: live). `None` when no question is open.
/// Used to exclude the OPEN pending window from a live attempt's node-timeout
/// clock: [`pending_interval_ms`] only sums CLOSED asked->answered pairs, so a
/// still-unanswered question would otherwise let the node time out mid-question.
fn open_question_asked_ts(events: &[Event], node: &str) -> Option<u128> {
    let mut asked: Vec<u128> = Vec::new();
    let mut answered = 0usize;
    for e in events {
        match &e.payload {
            EventPayload::QuestionAsked { node: n, .. } if n == node => asked.push(e.ts),
            EventPayload::QuestionAnswered { node: n, .. } if n == node => answered += 1,
            _ => {}
        }
    }
    if asked.len() > answered {
        asked.last().copied()
    } else {
        None
    }
}

/// Scans agent output for a line equal to [`QUESTION_MARKER`] and parses the
/// next non-empty line as an [`AskedQuestion`] (spec 2026-07-20, Transport:
/// resume/reprompt). The scan runs ONLY for an interactive task: on a
/// non-interactive node the marker text is ordinary output and this returns
/// `Ok(None)`. On an interactive node a marker whose following line is not a
/// valid question object fails the attempt with a Transport error naming the
/// node and the marker, never a silent skip - a half-parsed question would park
/// the run on something nobody can read. A marker with no following payload
/// line is treated as no question (`Ok(None)`).
fn scan_question(
    output: &str,
    task: &AgentTask,
) -> Result<Option<AskedQuestion>, (ErrorClass, String)> {
    if !task.interactive {
        return Ok(None);
    }
    let mut lines = output.lines();
    while let Some(line) = lines.next() {
        if line.trim() == QUESTION_MARKER {
            for next in lines.by_ref() {
                if next.trim().is_empty() {
                    continue;
                }
                return serde_json::from_str::<AskedQuestion>(next.trim())
                    .map(Some)
                    .map_err(|e| {
                        (
                            ErrorClass::Transport,
                            format!(
                                "interactive node `{}` printed a malformed question after the {QUESTION_MARKER} marker: {e}",
                                task.node
                            ),
                        )
                    });
            }
        }
    }
    Ok(None)
}

#[derive(Debug)]
pub struct AgentReport {
    pub status: NodeStatus,
    pub summary: String,
    pub raw: String,
    /// Set when the agent asked a question via the stdout marker protocol
    /// (spec 2026-07-20). `None` on a normal finish. The drive loop parks an
    /// interactive node whose attempt returns `Some(..)`; a non-interactive
    /// node ignores it (`node.rs` gates on the node's `interactive` flag).
    pub question: Option<AskedQuestion>,
    /// Agent session id captured from this attempt's output, for the `resume`
    /// transport (spec 2026-07-20, Task 7). `None` when the agent surfaced no
    /// session id (e.g. a plain-text one-shot run). Set from
    /// [`capture_session`] using the task's `agent`; the drive loop writes it
    /// into `AttemptFinished.session` and reads it back to re-enter the agent's
    /// session on the answer round.
    pub session: Option<String>,
}

/// Captures an agent session id from a finished attempt's raw output, for the
/// `resume` transport (spec 2026-07-20, Task 7). Dispatches on `agent_id`;
/// returns `None` when the output carries no session id, which forces the
/// runtime downgrade from `resume` to `reprompt`.
///
/// Reality per agent under the CURRENT one-shot invocation forms: only claude's
/// stream-json output (`--output-format stream-json`, the `acp` transport)
/// emits a `session_id` field, so claude is the one agent that yields a session
/// id today; codex/opencode/hermes/grok/cursor one-shot output is plain
/// final-answer text
/// with no session id, so they yield `None` here and rely on the downgrade
/// path. The per-agent field lists below are wired so that when those agents'
/// resumable one-shot output lands, only the field name changes here (spec
/// Transport: resume). No parser is invented for an output shape we do not
/// produce today: a plain-text line simply never matches.
pub fn capture_session(agent_id: &str, raw: &str) -> Option<String> {
    match agent_id {
        "claude" | "claude-code" => capture_json_string_field(raw, &["session_id"]),
        "codex" => capture_json_string_field(raw, &["session_id", "conversation_id"]),
        "opencode" => capture_json_string_field(raw, &["session_id", "sessionID"]),
        "hermes" => capture_json_string_field(raw, &["session", "session_id"]),
        "grok" => capture_json_string_field(raw, &["session_id", "sessionId"]),
        "cursor" => capture_json_string_field(raw, &["chatId", "chat_id", "session_id"]),
        _ => None,
    }
}

/// Scans each line of `raw` as a top-level JSON object and returns the LAST
/// non-empty string value found under any name in `fields` (the terminal event
/// wins, matching how the stream's final `result` event carries the id).
/// `None` when no line parses to such an object with such a field - the plain-
/// text one-shot case.
fn capture_json_string_field(raw: &str, fields: &[&str]) -> Option<String> {
    let mut found: Option<String> = None;
    for line in raw.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        for f in fields {
            if let Some(s) = val
                .get(*f)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                found = Some(s.to_string());
            }
        }
    }
    found
}

pub trait AgentAdapter {
    fn run(&self, task: &AgentTask) -> Result<AgentReport, (ErrorClass, String)>;

    /// Like `run`, but with cooperative cancellation: while the agent is
    /// running, the implementation periodically checks `cancel` and, if set,
    /// kills the process and returns Err(Transport, "cancelled"). Needed by
    /// parallel branches so join:any can cancel the losing branch (spec 8.4).
    ///
    /// `on_spawn`, when set, is invoked exactly once immediately after the agent
    /// process is successfully spawned, carrying the child pid (`child.id()`).
    /// The attempt-journaling path uses it to append `attempt_started` at spawn
    /// time so a crash mid-attempt leaves an open attempt on disk. It never
    /// fires if the spawn itself fails.
    ///
    /// `live`, when set (spec 2026-07-20, Transport: live), injects the
    /// `apb __ask-server` sidecar into the agent's argv and hands the adapter a
    /// per-poll `on_tick` the drive thread runs to observe the question/answer
    /// channels while the agent blocks in `ask_user`. `None` for every
    /// non-live attempt.
    ///
    /// The default ignores `cancel`/`on_spawn`/`live` and just calls `run` (for
    /// adapters without kill or spawn-hook support).
    fn run_cancellable(
        &self,
        task: &AgentTask,
        cancel: &AtomicBool,
        on_spawn: Option<&dyn Fn(u32)>,
        live: Option<&LiveHooks>,
    ) -> Result<AgentReport, (ErrorClass, String)> {
        let _ = (cancel, on_spawn, live);
        self.run(task)
    }

    /// Spawns a background supervisor agent as a separate, non-awaited
    /// (detached) process. The default implementation is a no-op, so adapters
    /// for which spawning a supervisor makes no sense do not need to override
    /// it.
    fn spawn_supervisor(
        &self,
        _brief: &str,
        _model: &str,
        _workdir: &Path,
        _soul: Option<&str>,
        _policy: &ConnectorEnvPolicy,
    ) -> Result<(), (ErrorClass, String)> {
        Ok(())
    }
}

pub struct ClaudeAdapter {
    pub program: String,
    /// Declarative invocation form (argv template, prompt_via, SOUL delivery,
    /// transport). The type name is historical: the adapter is now
    /// parameterized by the form and serves any headless/acp-compatible CLI,
    /// not just claude.
    pub spec: InvocationDef,
}

/// Builds argv (without the program name) and an optional stdin payload from
/// the invocation form. The `{prompt}`/`{model}` placeholders are substituted
/// as whole elements. SOUL: with `prefix` it is prepended before the prompt,
/// with `native` it goes out as a separate `soul_flag <soul>`. An empty SOUL
/// is not delivered.
fn build_command(
    spec: &InvocationDef,
    prompt: &str,
    model: &str,
    soul: Option<&str>,
    grant_autonomy: bool,
) -> (Vec<String>, Option<String>) {
    let soul = soul.filter(|s| !s.is_empty());
    let effective_prompt = match (spec.soul, soul) {
        (SoulDelivery::Prefix, Some(s)) => format!("{s}\n\n---\n\n{prompt}"),
        _ => prompt.to_string(),
    };
    let mut argv: Vec<String> = Vec::with_capacity(spec.argv.len() + 2);
    for a in &spec.argv {
        match a.as_str() {
            "{prompt}" => argv.push(effective_prompt.clone()),
            "{model}" => argv.push(model.to_string()),
            other => argv.push(other.to_string()),
        }
    }
    if spec.soul == SoulDelivery::Native
        && let Some(s) = soul
        && let Some(flag) = &spec.soul_flag
    {
        argv.push(flag.clone());
        argv.push(s.to_string());
    }
    // Autonomy is granted only for an authorized effectful run (decided by the
    // caller, see AgentTask::grant_autonomy). The flags themselves are the
    // agent's own non-interactive permission mechanism, carried as data on the
    // invocation form so codex/opencode/custom agents stay unaffected.
    if grant_autonomy {
        argv.extend(spec.autonomous_args.iter().cloned());
    }
    let stdin_payload = match spec.prompt_via {
        PromptVia::Stdin => Some(effective_prompt),
        PromptVia::Argv => None,
    };
    (argv, stdin_payload)
}

/// Appends the live `--mcp-config` sidecar injection to `argv` when this is a
/// live interactive attempt on claude/claude-code (spec 2026-07-20, Transport:
/// live). The agent guard is belt-and-braces: the drive layer only builds a
/// `LiveHooks` for claude in the first place, but a non-claude argv must never
/// gain a claude-only flag. No `--strict-mcp-config`: the agent keeps its own
/// configured servers.
fn inject_ask_server(argv: &mut Vec<String>, task: &AgentTask, live: Option<&LiveHooks>) {
    if let Some(lh) = live
        && (task.agent == "claude" || task.agent == "claude-code")
    {
        argv.push("--mcp-config".to_string());
        argv.push(ask_server_mcp_config(
            &lh.inject.exe,
            &lh.inject.run_id,
            task.node,
            lh.inject.attempt,
            lh.inject.timeout_ms,
        ));
    }
}

/// Tail appended to the prompt: asks the agent to end its reply with a
/// structured report block (spec 6.2 contract). Agents that follow the
/// contract get their self-assessed status reflected in node_status;
/// stubs/agents without the block get backward-compatible handling (see
/// `interpret_report`).
const REPORT_INSTRUCTION: &str = "When you are done, end your reply with a fenced yaml block reporting the outcome:\n```yaml\nstatus: success | failure\nsummary: one-line summary of what you did\n```";

fn with_report_instruction(prompt: &str) -> String {
    format!("{prompt}\n\n{REPORT_INSTRUCTION}")
}

/// Extracts status and summary from the agent's reply per the report contract
/// (spec 6.2): the last fenced ```yaml block with a `status` field.
/// `status: failure` is the agent's self-assessment (agent_reported_failure),
/// and it drives the node_status branching. If there is no block, or it has
/// no valid status, the default is Succeeded, and the summary is the whole
/// text (backward compatibility with agents and stubs that have no
/// structured block). NOTE: the strict variant of the spec (no block ->
/// unknown + anomaly) is deliberately NOT included so as not to break agents
/// without the contract; this is a possible future tightening.
fn interpret_report(text: &str) -> (NodeStatus, String) {
    if let Some(block) = last_yaml_block(text)
        && let Ok(val) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&block)
    {
        let status = match val.get("status").and_then(|s| s.as_str()) {
            Some("failure") => Some(NodeStatus::Failed),
            Some("success") => Some(NodeStatus::Succeeded),
            _ => None,
        };
        if let Some(status) = status {
            let summary = val
                .get("summary")
                .and_then(|s| s.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| text.trim().to_string());
            return (status, summary);
        }
    }
    (NodeStatus::Succeeded, text.trim().to_string())
}

/// Body of the last completed fenced ```yaml (or ```yml) block. The scan runs
/// forward, each opening fence pairs with its OWN nearest closing fence, and
/// after closing the opening state is reset - so a closing fence can never be
/// mistakenly paired with an unrelated opening. None if there is no completed
/// block.
fn last_yaml_block(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut open: Option<usize> = None;
    let mut last: Option<String> = None;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        match open {
            None if t == "```yaml" || t == "```yml" => open = Some(i),
            Some(start) if t == "```" => {
                last = Some(lines[(start + 1)..i].join("\n"));
                open = None;
            }
            _ => {}
        }
    }
    last
}

impl ClaudeAdapter {
    pub fn from_env() -> Self {
        let program = std::env::var("APB_AGENT_CMD").unwrap_or_else(|_| "claude".to_string());
        Self {
            program,
            spec: crate::invocation::builtin("claude").expect("builtin claude spec"),
        }
    }

    /// `pending_ms` (spec 2026-07-20, Task 5): milliseconds of pending-
    /// question time excluded from the elapsed clock, from
    /// `pending_question_ms` below. A node's `timeout_seconds` budgets the
    /// agent's own work, not a human/supervisor's answer time - a node that
    /// budgets 300s of agent work must not be killed because a question sat
    /// unanswered for an hour. `0` for the reprompt transport (a park always
    /// spans a fresh attempt boundary, so no completed question interval ever
    /// falls inside one attempt's own `started..now` window). For the live
    /// `ask_user` transport (Task 11), whose single long-lived attempt DOES
    /// span the pending question, it is recomputed each poll tick and includes
    /// the still-open window.
    fn check_cancel_timeout(
        child: &mut Child,
        cancel: &AtomicBool,
        started: Instant,
        timeout: Option<Duration>,
        pending_ms: u128,
    ) -> Option<(ErrorClass, String)> {
        if cancel.load(Ordering::Relaxed) {
            kill_process_tree(child);
            return Some((ErrorClass::Transport, "cancelled".to_string()));
        }
        if let Some(limit) = timeout {
            let pending = Duration::from_millis(u64::try_from(pending_ms).unwrap_or(u64::MAX));
            if started.elapsed().saturating_sub(pending) >= limit {
                kill_process_tree(child);
                return Some((
                    ErrorClass::Timeout,
                    format!("agent timed out after {}s", limit.as_secs()),
                ));
            }
        }
        None
    }

    /// Sums `pending_interval_ms` (spec 2026-07-20, Task 5) over the run's
    /// event log for `task`'s node, restricted to events at or after
    /// `since_ms` (the wall-clock instant this attempt began) so a
    /// historical, already-closed question round from a PREVIOUS attempt is
    /// never double-counted against a freshly started one. `0` when the task
    /// carries no timeout (nothing to exclude from), no run dir/node id (an
    /// internal, connector-less call), or the event log cannot be read.
    fn pending_question_ms(task: &AgentTask, since_ms: u128, include_open: bool) -> u128 {
        if task.timeout.is_none() {
            return 0;
        }
        let (Some(run_dir), Some(node_id)) = (
            task.connector_policy.run_dir.as_deref(),
            task.connector_policy.node_id.as_deref(),
        ) else {
            return 0;
        };
        let Ok(events) = crate::event::read_all(run_dir) else {
            return 0;
        };
        let scoped: Vec<_> = events.into_iter().filter(|e| e.ts >= since_ms).collect();
        let mut total = pending_interval_ms(&scoped, node_id);
        // Live path (spec 2026-07-20, Task 11): a single long-lived attempt
        // spans the OPEN (asked-but-unanswered) question window, which
        // `pending_interval_ms` (closed pairs only) does not cover. Add the
        // elapsed since the currently-open question so a human still thinking
        // does not push the node past its `timeout_seconds`.
        if include_open && let Some(asked_ts) = open_question_asked_ts(&scoped, node_id) {
            total += crate::event::now_millis().saturating_sub(asked_ts);
        }
        total
    }

    /// Headless transport: a one-shot buffered run of `claude -p ...`, stdout
    /// is collected on completion. Cancellation/timeout - via a poll loop.
    fn run_headless(
        &self,
        task: &AgentTask,
        cancel: &AtomicBool,
        on_spawn: Option<&dyn Fn(u32)>,
        live: Option<&LiveHooks>,
    ) -> Result<AgentReport, (ErrorClass, String)> {
        let prompt = with_report_instruction(task.prompt);
        let (mut argv, stdin_payload) = build_command(
            &self.spec,
            &prompt,
            task.model,
            task.soul,
            task.grant_autonomy,
        );
        inject_ask_server(&mut argv, task, live);
        let mut cmd = Command::new(&self.program);
        cmd.args(&argv)
            .current_dir(task.workdir)
            .stdin(if stdin_payload.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Connector env isolation (spec 4.3): scrub inherited connector tokens
        // and inject the run-context env before spawning the agent.
        task.connector_policy.apply(&mut cmd);
        let mut child = spawn_in_group(&mut cmd).map_err(|e| {
            (
                ErrorClass::ProcessExit,
                format!("spawn `{}` failed: {e}", self.program),
            )
        })?;
        // Attempt journaling (spawn-time): the process exists, so record it now
        // (pid = child.id()) before any of its work runs.
        if let Some(cb) = on_spawn {
            cb(child.id());
        }
        if let Some(payload) = &stdin_payload
            && let Some(mut si) = child.stdin.take()
        {
            let _ = si.write_all(payload.as_bytes());
            // si is dropped here - stdin closes, the agent sees EOF.
        }
        let started = Instant::now();
        // Pending-question exclusion. For a non-live attempt it is computed once
        // (the reprompt park always spans an attempt boundary, so no completed
        // interval falls inside one attempt's own window). For a LIVE attempt
        // (spec 2026-07-20, Task 11) the single long-lived attempt DOES span the
        // pending window, so it is recomputed each poll tick - AFTER `on_tick`
        // journals the round - and includes the still-open question window.
        let since = crate::event::now_millis();
        let mut pending_ms = Self::pending_question_ms(task, since, false);
        loop {
            if let Some(lh) = live {
                (lh.on_tick)();
                // Question timed out with no default answer (spec 2026-07-20,
                // Task 11 fix): drive-thread `on_tick` set the abort flag. Tear
                // down the blocked agent's process group so drive can fail the
                // attempt with the node-named timeout message.
                if lh.abort.load(Ordering::Relaxed) {
                    kill_process_tree(&mut child);
                    return Err((
                        ErrorClass::Timeout,
                        "live interactive question timed out with no default answer".to_string(),
                    ));
                }
                pending_ms = Self::pending_question_ms(task, since, true);
            }
            if let Some(err) =
                Self::check_cancel_timeout(&mut child, cancel, started, task.timeout, pending_ms)
            {
                return Err(err);
            }
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => {
                    return Err((
                        ErrorClass::ProcessExit,
                        format!("wait `{}` failed: {e}", self.program),
                    ));
                }
            }
        }
        // The agent process itself is gone, but `wait_with_output` reads both
        // pipes to EOF - and EOF is decided by whoever still holds the write
        // ends, not by the process we just waited for. A real agent spawns MCP
        // servers and tool subprocesses; any one of them that daemonizes and
        // outlives its parent keeps those fds open, and this read would then
        // block for the lifetime of that daemon. Tearing the group down first
        // is what makes EOF actually arrive.
        kill_process_group(child.id());
        let output = wait_with_output_bounded(child, drain_budget(), &self.program)?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if !output.status.success() {
            return Err((
                ErrorClass::ProcessExit,
                format!("agent exited with {:?}: {stderr}", output.status.code()),
            ));
        }
        // Status comes from the structured report block (spec 6.2); raw is the full stdout.
        let (status, summary) = interpret_report(&stdout);
        // Marker scan (spec 2026-07-20): an interactive node's agent may ask a
        // question instead of finishing. The scan is gated on `task.interactive`
        // and hard-fails on malformed JSON naming the node; a non-interactive
        // node's literal marker text is simply ignored.
        let question = scan_question(&stdout, task)?;
        // Session capture (spec 2026-07-20, Task 7): pull the agent's session id
        // from its output so the answer round can resume the same session. The
        // plain headless `-p` form carries no session id, so this is normally
        // `None`; the stream path below is where claude surfaces one.
        let session = capture_session(task.agent, &stdout);
        Ok(AgentReport {
            status,
            summary,
            raw: stdout,
            question,
            session,
        })
    }

    /// acp transport (currently based on Claude Code's stream-json): runs the
    /// agent in streaming mode, reads stdout line by line on a separate
    /// thread, writes each NDJSON event to `stream_log` as it arrives, and on
    /// EOF extracts the final result from the terminal `type: result` event.
    /// Cancellation/timeout kill the process. Error classification per spec
    /// 7.2: a broken/invalid stream with no result -> Transport; non-zero
    /// exit code -> ProcessExit; a successful exit code with no result event
    /// -> StructuredOutputMissing; a result with is_error -> a report with
    /// status Failed (agent_reported_failure).
    ///
    /// NOTE (provisional): the exact Claude Code stream-json schema is not
    /// rigidly pinned down here - parsing is lenient (unrecognized lines are
    /// skipped). The full Agent Client Protocol (JSON-RPC initialize/
    /// session.new/session.prompt/session.update, permissions, multi-agent)
    /// is a follow-up refinement on top of this same transport.
    fn run_acp(
        &self,
        task: &AgentTask,
        cancel: &AtomicBool,
        on_spawn: Option<&dyn Fn(u32)>,
        live: Option<&LiveHooks>,
    ) -> Result<AgentReport, (ErrorClass, String)> {
        let prompt = with_report_instruction(task.prompt);
        // Base argv comes from the invocation form; claude-specific streaming
        // flags (stream-json) are layered on top. In the first iteration,
        // acp = claude.
        let (mut argv, _stdin) = build_command(
            &self.spec,
            &prompt,
            task.model,
            task.soul,
            task.grant_autonomy,
        );
        inject_ask_server(&mut argv, task, live);
        argv.push("--output-format".to_string());
        argv.push("stream-json".to_string());
        argv.push("--verbose".to_string());
        let mut cmd = Command::new(&self.program);
        cmd.args(&argv)
            .current_dir(task.workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Connector env isolation (spec 4.3): scrub inherited connector tokens
        // and inject the run-context env before spawning the agent.
        task.connector_policy.apply(&mut cmd);
        let mut child = spawn_in_group(&mut cmd).map_err(|e| {
            (
                ErrorClass::ProcessExit,
                format!("spawn `{}` failed: {e}", self.program),
            )
        })?;
        // Attempt journaling (spawn-time): record the process (pid = child.id())
        // now, before its streaming work runs.
        if let Some(cb) = on_spawn {
            cb(child.id());
        }

        // Read stdout on a background thread: BufReader::lines() blocks
        // line by line, but we also need to poll cancel/timeout concurrently.
        // Lines go into a channel.
        let stdout = child.stdout.take().expect("stdout piped");
        let (tx, rx) = mpsc::channel::<String>();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // Drain stderr on a separate thread, otherwise heavy stderr output
        // would fill its pipe and block the agent on write (we only read
        // stdout).
        // Delivered over a channel rather than through `JoinHandle::join`:
        // there is no timed join in std, and stderr's EOF is decided by
        // whatever still holds the write end - possibly a daemonized
        // grandchild rather than the agent. See the bounded receive below.
        let stderr_pipe = child.stderr.take();
        let (err_tx, err_rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            use std::io::Read as _;
            let mut s = String::new();
            if let Some(mut e) = stderr_pipe {
                let _ = e.read_to_string(&mut s);
            }
            let _ = err_tx.send(s);
        });

        let mut sink = task.stream_log.and_then(|p| {
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .ok()
        });

        let started = Instant::now();
        // Pending-question exclusion: once for a non-live attempt, recomputed
        // each tick (with the open-question window) for a LIVE attempt - see
        // `run_headless` for the rationale.
        let since = crate::event::now_millis();
        let mut pending_ms = Self::pending_question_ms(task, since, false);
        let pid = child.id();
        let mut raw_lines: Vec<String> = Vec::new();
        // Set once the agent process itself has exited: the deadline for
        // draining whatever is still buffered in the pipe afterwards.
        let mut drain_deadline: Option<Instant> = None;
        loop {
            // Only while the agent is still running. Once `drain_deadline` is
            // set the agent has ALREADY exited, and what remains is reading
            // bytes it left in the pipe.
            //
            // For the timeout half: charging that read against the node's
            // timeout would report `TimedOut` for an agent that finished
            // inside its budget, purely because a leftover descendant made the
            // final read slow.
            //
            // For the cancel half: a cancellation arriving during the drain is
            // ignored for at most `drain_budget()`, which is deliberate and
            // harmless. Cancellation exists to stop WORK and reclaim the
            // machine, and there is no work left to stop - the agent is gone
            // and its group was killed on the line below. All that remains is
            // copying bytes out of a pipe. Honouring it here would abandon
            // output the run has already paid for, in exchange for ending a
            // bounded, idle wait slightly sooner.
            //
            // Live observation (spec 2026-07-20, Task 11): while the agent is
            // still running, run the drive-thread `on_tick` (journals the
            // question/answer round as it lands) and refresh the pending-window
            // exclusion so a blocked-on-question agent is never killed as hung.
            if drain_deadline.is_none()
                && let Some(lh) = live
            {
                (lh.on_tick)();
                // Question timed out with no default answer (spec 2026-07-20,
                // Task 11 fix): tear down the blocked agent so drive can fail the
                // attempt with the node-named timeout message.
                if lh.abort.load(Ordering::Relaxed) {
                    kill_process_tree(&mut child);
                    return Err((
                        ErrorClass::Timeout,
                        "live interactive question timed out with no default answer".to_string(),
                    ));
                }
                pending_ms = Self::pending_question_ms(task, since, true);
            }
            if drain_deadline.is_none()
                && let Some(err) = Self::check_cancel_timeout(
                    &mut child,
                    cancel,
                    started,
                    task.timeout,
                    pending_ms,
                )
            {
                return Err(err);
            }
            // This loop used to end ONLY on stdout EOF - but EOF is not the
            // agent's to give. A grandchild that inherited the pipe (a real
            // agent spawns MCP servers and tool subprocesses) holds it open
            // after the agent is gone, and a node with no `timeout_seconds`
            // then spun here forever, waiting on output from a process that no
            // longer existed. So notice the agent's own exit, release the pipes
            // (whatever still holds them is by definition not the agent), and
            // let the reader hand over what it has within a bounded window.
            if drain_deadline.is_none() && matches!(child.try_wait(), Ok(Some(_))) {
                kill_process_group(pid);
                drain_deadline = Some(Instant::now() + drain_budget());
            }
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(line) => {
                    if let Some(f) = sink.as_mut() {
                        let _ = writeln!(f, "{line}");
                    }
                    raw_lines.push(line);
                }
                // The reader thread closed the channel: stdout reached EOF
                // (the process finished its output) - exit and assemble the result.
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            // Backstop for the one case the group kill above cannot reach: a
            // descendant that left the process group while holding the pipe.
            if drain_deadline.is_some_and(|d| Instant::now() >= d) {
                break;
            }
        }
        // Bounded because the loop above exited on stdout EOF, and stdout EOF
        // is NOT proof the agent exited: it can close stdout (or hand it to a
        // grandchild that then closes it) and keep running. `child.wait()`
        // here would block for as long as it does, with the drive's own
        // timeout already behind us. So: wait with a deadline, and if the
        // agent overstays it, tear down the tree and report a timeout rather
        // than hang.
        //
        // `reader` is deliberately NOT joined. On the normal path it has
        // already finished (the loop broke because it dropped its sender), so
        // a join would be a no-op; on the backstop path it is still blocked on
        // a pipe held by a descendant that escaped the group, and joining it
        // is precisely the unbounded wait being avoided. It owns nothing the
        // caller needs - every line reached us through the channel - so it is
        // abandoned and dies with the process.
        drop(reader);
        let grace = exit_after_eof_budget();
        let status = match wait_bounded(&mut child, grace) {
            Some(Ok(status)) => status,
            Some(Err(e)) => {
                return Err((
                    ErrorClass::ProcessExit,
                    format!("wait `{}` failed: {e}", self.program),
                ));
            }
            None => {
                // The agent will not exit. Tear the tree down either way, but
                // do not throw away a run that actually finished: if the
                // stream already carried its terminal `result` event, the
                // agent said everything it had to say and only failed to exit
                // afterwards. Reporting that as a timeout would discard
                // completed work over a process-lifecycle detail, which is the
                // same call this wave made for an agent that exits leaving a
                // grandchild on its pipes - the work counts, the leftover
                // process is noise. `Timeout` is reserved for the case where
                // no result was ever seen, which is a genuinely unfinished
                // node.
                kill_process_tree(&mut child);
                return match parse_stream_result(&raw_lines, task) {
                    Ok(report) => Ok(report),
                    // A malformed-marker Transport error names the node and must
                    // survive the grace teardown (spec 2026-07-20, Task 6): the
                    // terminal result WAS seen, and the question after the marker
                    // was invalid JSON - rewriting that into a generic "no result
                    // event" Timeout would hide the real, node-named cause. Only a
                    // genuinely-missing result becomes the grace Timeout.
                    Err((ErrorClass::Transport, msg)) => Err((ErrorClass::Transport, msg)),
                    Err(_) => Err((
                        ErrorClass::Timeout,
                        format!(
                            "`{}` closed its stdout without a terminal result event and was still \
                             running {grace:?} later; killed its process group",
                            self.program
                        ),
                    )),
                };
            }
        };
        // The leader is reaped; anything left in its group would still be
        // holding stderr, so clear the group before collecting.
        kill_process_group(pid);
        let stderr = err_rx.recv_timeout(drain_budget()).unwrap_or_default();
        if !status.success() {
            return Err((
                ErrorClass::ProcessExit,
                format!("agent exited with {:?}: {}", status.code(), stderr.trim()),
            ));
        }

        parse_stream_result(&raw_lines, task)
    }
}

/// Extracts the result from the stream-json stream: looks for the terminal
/// `type: "result"` event and takes its text (`result`) and `is_error` flag.
/// The absence of such an event on a normal exit is StructuredOutputMissing.
fn parse_stream_result(
    lines: &[String],
    task: &AgentTask,
) -> Result<AgentReport, (ErrorClass, String)> {
    let raw = lines.join("\n");
    for line in lines.iter().rev() {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if val.get("type").and_then(|t| t.as_str()) != Some("result") {
            continue;
        }
        let is_error = val
            .get("is_error")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        let text = val
            .get("result")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_string();
        // Status: the stream's is_error takes priority; otherwise, the
        // agent's self-assessment from the report block in the result text
        // (spec 6.2), defaulting to success.
        let (block_status, summary) = interpret_report(&text);
        let status = if is_error {
            NodeStatus::Failed
        } else {
            block_status
        };
        // Marker scan over the agent's message text (spec 2026-07-20): in the
        // stream transport the agent's prose is the `result` event's text, so
        // that is where the marker appears (not the surrounding NDJSON). Gated
        // on `task.interactive`; malformed JSON fails naming the node.
        let question = scan_question(&text, task)?;
        // Session capture over the whole NDJSON stream (spec 2026-07-20, Task
        // 7): claude's stream-json events carry a `session_id`, so the parser
        // scans `raw` (every event line), not just the terminal result text.
        let session = capture_session(task.agent, &raw);
        return Ok(AgentReport {
            status,
            summary,
            raw,
            question,
            session,
        });
    }
    Err((
        ErrorClass::StructuredOutputMissing,
        "no `result` event in stream-json output".to_string(),
    ))
}

impl AgentAdapter for ClaudeAdapter {
    fn run(&self, task: &AgentTask) -> Result<AgentReport, (ErrorClass, String)> {
        // A non-cancellable run is a cancellable run with an always-false flag,
        // no spawn hook, and no live sidecar.
        self.run_cancellable(task, &AtomicBool::new(false), None, None)
    }

    fn run_cancellable(
        &self,
        task: &AgentTask,
        cancel: &AtomicBool,
        on_spawn: Option<&dyn Fn(u32)>,
        live: Option<&LiveHooks>,
    ) -> Result<AgentReport, (ErrorClass, String)> {
        match self.spec.transport {
            Transport::Headless => self.run_headless(task, cancel, on_spawn, live),
            Transport::Acp => self.run_acp(task, cancel, on_spawn, live),
        }
    }

    /// Spawns a background supervisor agent and does not wait for it to
    /// finish (fire-and-forget): dropping the `Child` will orphan the
    /// process, which is intentional for Phase 4c - the supervisor lives out
    /// its own cycle (supervisor_wait_event -> ... -> supervisor_report) for
    /// the whole run, and the engine holds no explicit handle to it. The live
    /// cycle against a real `claude` is verified manually (Task 5); here we
    /// only cover the fact of spawning.
    fn spawn_supervisor(
        &self,
        brief: &str,
        model: &str,
        workdir: &Path,
        soul: Option<&str>,
        policy: &ConnectorEnvPolicy,
    ) -> Result<(), (ErrorClass, String)> {
        // Goes through build_command using the invocation form (not hardcoded
        // -p/--model), otherwise codex/opencode/custom argv and stdin
        // profiles would be spawned incorrectly. The supervisor profile's
        // SOUL is delivered according to the form (native flag / prefix).
        // The supervisor keeps the default permission posture for now; its
        // intervention path is the supervisor_* MCP tools, not autonomous
        // file/network actions in the run workdir.
        let (argv, stdin_payload) = build_command(&self.spec, brief, model, soul, false);
        let mut cmd = Command::new(&self.program);
        cmd.args(&argv)
            .current_dir(workdir)
            .stdin(if stdin_payload.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Connector env isolation (spec 4.3): the supervisor is a spawned agent
        // too, so its inherited connector tokens are scrubbed before spawn.
        policy.apply(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| {
            (
                ErrorClass::ProcessExit,
                format!("spawn supervisor `{}` failed: {e}", self.program),
            )
        })?;
        if let Some(payload) = &stdin_payload
            && let Some(mut si) = child.stdin.take()
        {
            let _ = si.write_all(payload.as_bytes());
            // si is dropped here - stdin closes, the agent sees EOF.
        }
        Ok(())
    }
}

pub fn adapter_for(agent: &str) -> Result<Box<dyn AgentAdapter>, EngineError> {
    // APB_AGENT_CMD is an explicit program override (tests, local runs) with
    // the highest priority and the headless claude form: the config-free path
    // stays unchanged.
    if let Ok(program) = std::env::var("APB_AGENT_CMD") {
        let spec = crate::invocation::builtin("claude").expect("builtin claude spec");
        return Ok(Box::new(ClaudeAdapter { program, spec }));
    }
    // Agent invocation form: config overrides the built-in default. An
    // unknown agent with no form -> error (same as the former "unsupported
    // agent").
    let global = apb_core::config::GlobalConfig::load().unwrap_or_default();
    let spec = crate::invocation::spec_for(agent, &global)?;
    let program = global
        .agent_program(agent)
        .unwrap_or_else(|| default_program(agent));
    Ok(Box::new(ClaudeAdapter { program, spec }))
}

/// Default binary name for built-in agents when not set in config:
/// claude/claude-code -> "claude", others - the id itself (codex, opencode, agy).
fn default_program(agent: &str) -> String {
    match agent {
        "claude" | "claude-code" => "claude".to_string(),
        // cursor is installed as `cursor-agent`; the bare `cursor` binary is
        // the GUI editor CLI, not the headless agent.
        "cursor" => "cursor-agent".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// cursor is installed as `cursor-agent`; the bare `cursor` binary is the
    /// GUI editor CLI. `default_program` must resolve the agent id to the
    /// detected binary so `adapter_for` spawns the right executable.
    #[test]
    fn default_program_maps_cursor_to_its_binary() {
        assert_eq!(default_program("cursor"), "cursor-agent");
        assert_eq!(default_program("grok"), "grok");
        assert_eq!(default_program("codex"), "codex");
        assert_eq!(default_program("claude"), "claude");
        assert_eq!(default_program("claude-code"), "claude");
    }

    #[test]
    fn interpret_report_reads_failure_block() {
        let text = "did work\n```yaml\nstatus: failure\nsummary: could not finish\n```";
        let (status, summary) = interpret_report(text);
        assert_eq!(status, NodeStatus::Failed);
        assert_eq!(summary, "could not finish");
    }

    #[test]
    fn build_command_appends_autonomous_args_when_granted() {
        let spec = crate::invocation::builtin("claude").expect("builtin claude spec");
        let (argv, _) = build_command(&spec, "hello", "claude-opus-4-8", None, true);
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--permission-mode" && w[1] == "bypassPermissions"),
            "expected the autonomous permission flag when granted, got {argv:?}"
        );
    }

    #[test]
    fn build_command_omits_autonomous_args_when_not_granted() {
        let spec = crate::invocation::builtin("claude").expect("builtin claude spec");
        let (argv, _) = build_command(&spec, "hello", "claude-opus-4-8", None, false);
        assert!(
            !argv.iter().any(|a| a == "bypassPermissions"),
            "must not grant permissions without autonomy, got {argv:?}"
        );
    }

    #[test]
    fn interpret_report_reads_success_block() {
        let text = "```yaml\nstatus: success\nsummary: done\n```";
        assert_eq!(interpret_report(text).0, NodeStatus::Succeeded);
    }

    #[test]
    fn interpret_report_defaults_to_success_without_block() {
        let text = "just plain output, no block";
        let (status, summary) = interpret_report(text);
        assert_eq!(status, NodeStatus::Succeeded);
        assert_eq!(summary, text);
    }

    #[test]
    fn ask_server_mcp_config_injects_run_node_attempt_and_timeout() {
        // Injection JSON (spec 2026-07-20, Task 11): the `--mcp-config` argument
        // points claude at `apb __ask-server` with the run/node/attempt and the
        // per-server timeout. `question_timeout_seconds`-derived ms here.
        let json =
            ask_server_mcp_config(Path::new("/opt/apb/bin/apb"), "run-xyz", "ask", 3, 120_000);
        let val: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        let server = &val["mcpServers"]["apb"];
        assert_eq!(server["command"], "/opt/apb/bin/apb");
        assert_eq!(
            server["args"],
            serde_json::json!([
                "__ask-server",
                "--run",
                "run-xyz",
                "--node",
                "ask",
                "--attempt",
                "3"
            ]),
        );
        assert_eq!(server["timeout"], 120_000);
    }

    #[test]
    fn ask_server_mcp_config_uses_large_default_timeout() {
        // With no `question_timeout_seconds` the drive layer hands the large
        // default, so the blocking `ask_user` call outlives claude's idle timer.
        let json = ask_server_mcp_config(
            Path::new("/usr/bin/apb"),
            "r1",
            "q",
            1,
            LIVE_TOOL_TIMEOUT_MS_DEFAULT,
        );
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        // The default comfortably exceeds claude's ~30-minute stdio idle timer
        // (about 28 h in ms) so a blocking `ask_user` call is never cut off.
        assert_eq!(
            val["mcpServers"]["apb"]["timeout"],
            LIVE_TOOL_TIMEOUT_MS_DEFAULT
        );
        assert_eq!(val["mcpServers"]["apb"]["timeout"], 100_800_000);
    }

    #[test]
    fn live_tool_timeout_is_a_backstop_strictly_larger_than_the_question_timeout() {
        // With a question timeout the injected sidecar timeout is that budget
        // plus the backstop slack, so drive's own enforcement always fires
        // first and the timeout semantics stay engine-owned.
        assert_eq!(
            live_tool_timeout_ms(Some(120)),
            120_000 + LIVE_TOOL_TIMEOUT_BACKSTOP_MS
        );
        assert!(
            live_tool_timeout_ms(Some(120)) > 120_000,
            "the sidecar timeout must be strictly larger than the question timeout"
        );
        // Without one, the large default.
        assert_eq!(live_tool_timeout_ms(None), LIVE_TOOL_TIMEOUT_MS_DEFAULT);
    }

    #[test]
    fn last_yaml_block_returns_last_completed_pairing_forward() {
        // A yaml block first, then an unrelated (json) block: it must not
        // pair with the json closing fence; the last COMPLETED yaml block is taken.
        let text = "```yaml\nstatus: success\nsummary: first\n```\nmid\n```json\n{\"x\":1}\n```";
        let block = last_yaml_block(text).expect("a yaml block");
        assert!(block.contains("summary: first"));
        assert!(!block.contains("json"));

        // Two yaml blocks: the last one is returned.
        let two = "```yaml\nstatus: failure\n```\n```yaml\nstatus: success\nsummary: latest\n```";
        assert_eq!(interpret_report(two).0, NodeStatus::Succeeded);
    }
}
