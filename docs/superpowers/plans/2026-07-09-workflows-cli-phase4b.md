# Workflows CLI, Phase 4b (supervisor MCP tools + supervise:self) - implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** give the coding agent (Claude Code) the ability to supervise an in-progress run directly from its own session. `workflow_run` with `supervise: "self"` starts the workflow in the background in supervised mode and issues a supervisor token; the agent waits on `supervisor_wait_event` (long-poll), on wake investigates via `run_inspect` and intervenes with the tools `node_retry` / `run_continue_from` / `run_pause` / `run_abort` / `context_append`, and at the end submits `supervisor_report`. All of this sits on top of the engine primitives from Phase 4a.

**Architecture:** Supervisor tools are thin wrappers over `wf-engine` (Phase 4a provided background runs, the control channel, wait_wake/run_inspect). The single-writer invariant for `events.jsonl` (the drive stream) is preserved: intervention tools write ONLY to `control.jsonl` (via `post_supervisor_command`), and the drive stream itself applies the commands and logs `SupervisorAction`/context notes to `events.jsonl`. `context_append` also goes through the control channel (a new `Control::ContextAppend` command) rather than writing events directly. The token model and capability model live in the MCP server (`WfMcp` gets shared supervisor-session state): `workflow_run(supervise:self)` mints a token bound to the `run_id` and a set of capabilities (from `supervisor.policy.capabilities`, default is all of them); supervisor tools accept a `token`, resolve the `run_id` from it, and check the capability. In the boot-core stdio setup one `wf mcp` process serves one agent session, so the token is a binding and gating mechanism, not protection against a remote attacker (this is stated plainly in the doc; a cryptographically strong token is a later hardening step).

**Tech Stack:** Rust (edition 2024), rmcp 2.2.0, std::sync::Mutex for session state; wf-core, wf-engine, wf-mcp. No new external dependencies.

**Spec:** `docs/superpowers/specs/2026-07-08-workflows-cli-design.md` (9.2 supervise:self, 9.4 agent tools, 9.5 capability model and safeguards, 9.6 report). Builds on Phases 1-3 and 4a.

## Global Constraints

- Error messages are in English; comments/documentation are in Russian. No em-dashes (U+2014) and no exclamation marks in code/docs.
- The project version stays at `0.1.0`.
- TDD: write a failing test first, then the implementation. Commit at the end of each task.
- Event-sourcing invariant: `events.jsonl` is written ONLY by the drive stream. Supervisor tools write to `control.jsonl` (and to `supervisor/report.md`). Do not violate this.
- Any client-supplied `run_id`/`token` that ends up in a path goes through `wf_core::registry::is_safe_segment`.
- Autonomous runs and all existing tests stay green; the behavior of Phase 3's non-supervisor tools does not change (except for the new optional `supervise` field on `workflow_run`).
- `workflow_patch` and patch versions are NOT part of 4b (Phase 6). Of the capabilities, 4b applies `observe` and `retry`; there is no `patch_workflow` tool, `edit_workspace` is declarative only.
- Run code-ranker before marking a task done; navigate via codegraph. Do not invoke a real `claude` in tests: verify the supervisor logic on a workflow with an agent_task using a `WF_AGENT_CMD` stub (failing, or failing-then-succeeding) or with no agent at all.

---

### Task 1: Engine - ContextAppend command, top-of-loop fix, note rendering, post_supervisor_command

**Files:**
- Modify: `crates/wf-engine/src/control.rs` (the `ContextAppend` variant)
- Modify: `crates/wf-engine/src/scheduler.rs` (top-of-loop, await_control, post_supervisor_command)
- Modify: `crates/wf-engine/src/context.rs` (rendering context notes)
- Modify: `crates/wf-engine/src/lib.rs` (re-export)
- Test: `crates/wf-engine/tests/supervisor_commands_test.rs`

**Interfaces:**
- `Control` gains a `ContextAppend { note: String }` variant (serde tag `cmd`=`context_append`).
- `pub fn post_supervisor_command(root: &Path, run_id: &str, cmd: Control) -> Result<u64, EngineError>` - validates via `is_safe_segment` (-> NotFound) and checks the run_dir exists (-> NotFound), then calls `post_control`. Returns the seq.
- Drive behavior:
  - **Top-of-loop fix (mandatory, from the final Phase 4a review):** the control-command scan at the node boundary no longer consumes or advances the shared cursor past non-stop commands. It reacts proactively to `Pause` (-> RunPaused, return Paused) and `Abort` (-> RunAborted, return Aborted), and applies `ContextAppend` (logging `SupervisorAction{action:"context_append", node:None, detail:note}` + rebuilding context.md), but `Retry`/`ContinueFrom` encountered outside a wake are ignored and do NOT move the cursor (they are only valid inside `await_control`). Implementation: top-of-loop uses a separate pass (`read_control_after(stop_cursor)`), advancing `stop_cursor` only for the Pause/Abort/ContextAppend commands it handled; `await_control` remains the consumer that advances the shared cursor on wake. IMPORTANT: make sure the same command is never processed in both places - keep separate cursors so that Retry/ContinueFrom are consumed strictly inside await_control, while Pause/Abort/ContextAppend are handled either at top-of-loop (proactively) OR inside await_control (in response to a wake); a ContextAppend seen inside await_control is applied and waiting continues (it is not a terminal command).
  - **await_control:** on `ContextAppend` - apply it (log SupervisorAction + rebuild context.md) and CONTINUE waiting for the next command; on `Retry`/`ContinueFrom`/`Pause`/`Abort` - return it for application (as in 4a).
- `build_context` additionally renders `EventPayload::SupervisorAction { action, detail, .. }` where `action == "context_append"` as a section `## note (supervisor)\n\n{detail}\n\n`, so the note lands in `context.md` and in `{{run.context}}` for later nodes. Order follows the order of appearance in the events.

- [ ] **Step 1: Failing test** `crates/wf-engine/tests/supervisor_commands_test.rs`
  Scenarios (build on the real helpers, with threads + polling events.jsonl and a 5s timeout, as in supervised_drive_test):
  1. `post_supervisor_command(root, "../../etc", Control::Pause)` -> `Err(NotFound)`; on a nonexistent run -> `Err(NotFound)`.
  2. Supervised run of a failing agent_task: wait for `WakeRaised`; `post_supervisor_command(Control::ContextAppend{note:"lint is failing because of X"})` then `post_supervisor_command(Control::Retry{node, prompt_override:None})`; with the fail-then-success stub the run reaches Succeeded; the events contain `SupervisorAction{action:"context_append"}` before `node_retry`; the resulting `context.md` (or build_context) contains the note text.
  3. Proactive `Pause` in a Supervised run at a node boundary (without a wake): start a multi-step Supervised run in a thread; immediately send `post_supervisor_command(Control::Pause)`; the run ends with `RunStatus::Paused` (top-of-loop honors Pause). Verify that a preceding `Retry`, mistakenly sent outside a wake, was NOT consumed by top-of-loop (did not break the cursor) - i.e. send ContextAppend (non-stop) before Pause and confirm Pause still fires and ContextAppend was applied.
  4. Autonomous is unchanged: existing engine tests stay green; a proactive Pause in Autonomous mode also ends as Paused (top-of-loop is shared between both modes).

- [ ] **Step 2: Confirm it fails.** `cargo test -p wf-engine --test supervisor_commands_test`.

- [ ] **Step 3: Implementation.** Add the variant, the cursor fix, the rendering, `post_supervisor_command`; re-export `post_supervisor_command` from lib.rs.

- [ ] **Step 4: Regression + commit.** `cargo test --workspace`. Commit: `feat(engine): context-append control, proactive pause, supervisor command helper`.

---

### Task 2: Pure supervisor tool functions in wf-mcp

**Files:**
- Modify: `crates/wf-mcp/src/tools.rs`
- Modify: `crates/wf-engine/src/inspect.rs` (+ report read/write) and lib.rs re-export
- Test: `crates/wf-mcp/tests/supervisor_tools_test.rs`

**Interfaces (in `tools.rs`, all `-> Result<Value, ToolError>` except wait):**
- `workflow_run_supervised(root, id, version, params, instruction) -> Result<Value, ToolError>` - calls `wf_engine::run_background` with `RunOptions{ mode: RunMode::Supervised, .. }`; returns `{ run_id }` (the token is minted by the server layer, not here).
- `supervisor_wait_event(root, run_id, after_seq: Option<u64>, timeout_ms: Option<u64>) -> Result<Value, ToolError>` - a wrapper over `wf_engine::wait_wake` (default timeout, e.g. 25s); returns `{ wake: <WakeEvent or null>, run_status }` (null means the run finished or timed out; the agent decides whether to keep polling).
- `sv_run_inspect(root, run_id) -> Result<Value, ToolError>` - a wrapper over `wf_engine::run_inspect`.
- `node_retry(root, run_id, node, prompt_override: Option<String>) -> Result<Value, ToolError>` - `post_supervisor_command(Control::Retry{..})`; `{ posted_seq }`.
- `run_continue_from(root, run_id, node) -> Result<Value, ToolError>` - `Control::ContinueFrom`.
- `run_pause(root, run_id) -> Result<Value, ToolError>` - `Control::Pause`.
- `run_abort(root, run_id) -> Result<Value, ToolError>` - `wf_engine::run_cancel` (Abort); `{ ok: true }`.
- `context_append(root, run_id, note) -> Result<Value, ToolError>` - `Control::ContextAppend{note}`.
- `supervisor_report(root, run_id, text) -> Result<Value, ToolError>` - writes `runs/<id>/supervisor/report.md` (via the engine's `write_supervisor_report`), `{ ok: true }`.
- Engine (`inspect.rs`): `pub fn write_supervisor_report(root, run_id, text) -> Result<(), EngineError>` (is_safe_segment; creates `supervisor/` inside run_dir; atomic_write report.md) and `pub fn read_supervisor_report(root, run_id) -> Result<Option<String>, EngineError>`.

- [ ] **Step 1: Failing test** `crates/wf-mcp/tests/supervisor_tools_test.rs`
  Scenarios: (1) e2e without a real agent - `workflow_run_supervised` on a workflow with no agent_task (start->prompt->finish): since supervised mode only affects the failure path, this run reaches Succeeded; poll `run_status` until succeeded. (2) On a fail-then-success agent_task stub: `workflow_run_supervised` -> in a separate thread wait for a wake via `supervisor_wait_event`, call `context_append` + `node_retry`, wait for succeeded, verify `sv_run_inspect` contains the wake+actions. (3) a traversal run_id in every tool -> `NotFound`. (4) `supervisor_report` writes report.md, `read_supervisor_report` reads it back.

- [ ] **Step 2-3: fail -> implementation.** Write the functions; re-export the report helpers.

- [ ] **Step 4: commit.** `cargo test --workspace`. `feat(mcp): supervisor tool functions (wait_event, inspect, retry, continue, pause, abort, context_append, report)`.

---

### Task 3: Token and capability model + registering supervisor tools in the server

**Files:**
- Modify: `crates/wf-mcp/src/server.rs`
- Modify: `crates/wf-mcp/src/tools.rs` (capabilities parser from supervisor.policy)
- Test: in `server.rs` (`#[cfg(test)]`) + `crates/wf-mcp/tests/supervisor_tools_test.rs`

**Interfaces:**
- In `tools.rs`: `pub fn supervisor_capabilities(root, id, version) -> Result<Vec<String>, ToolError>` - reads the workflow, extracts `supervisor.policy.capabilities` (a list of strings) or defaults to `["observe","retry"]` (in 4b we do not gate patch_workflow/edit_workspace with tools, but we include them in the list if set). Helper `capability_for_tool(name) -> &str` (observe: wait_event/run_inspect; retry: node_retry/run_continue_from/run_pause/run_abort/context_append).
- In `server.rs`:
  - `WfMcp` state is extended: `sessions: Arc<Mutex<HashMap<String, SupervisorSession>>>`, `SupervisorSession { run_id: String, capabilities: Vec<String> }`. `#[derive(Clone)]` is kept (Arc). Token: `format!("sv-{}-{}", now_millis, counter)` (counter behind a Mutex); document that this is not a crypto token (boot-core setup, trusted local client).
  - `workflow_run` gains an optional `supervise: Option<String>` argument (value `"self"`). If `Some("self")`: call `tools::workflow_run_supervised`, compute the capabilities via `tools::supervisor_capabilities`, mint a token, store the session, return `{ run_id, supervisor_token, capabilities }`. Otherwise - the previous synchronous Phase 3 behavior.
  - Eight supervisor tools are registered with `#[tool]`, each taking `token: String` (+ its own arguments, WITHOUT `run_id` - that comes from the token). Common gate: a `resolve_session(&self, token, tool_name) -> Result<String /*run_id*/, ToolError>` helper - looks up the session by token (otherwise `ToolError::Engine("invalid or unknown supervisor token")`), checks that `capability_for_tool(tool_name)` is in `session.capabilities` (otherwise `ToolError::Engine("capability <c> not granted")`), returns the `run_id`. The tool then calls the matching `tools::*` function with that `run_id`.
  - Tool names: `supervisor_wait_event`, `supervisor_run_inspect`, `supervisor_node_retry`, `supervisor_run_continue_from`, `supervisor_run_pause`, `supervisor_run_abort`, `supervisor_context_append`, `supervisor_report`. (The `supervisor_` prefix sets them apart from the Phase 3 read/run tools.)

- [ ] **Step 1: Failing test.** In the `server.rs` tests: (1) `tool_router` registers the previous 9 + 8 supervisor tools = 17 tools (update the existing counter test). (2) calling a supervisor tool with an unknown token -> is_error. (3) supervise:self via `workflow_run` returns success with a `supervisor_token`. (4) capability gate: a session with `capabilities:["observe"]` -> `supervisor_node_retry` (retry) yields is_error, while `supervisor_run_inspect` (observe) succeeds.

- [ ] **Step 2-3: fail -> implementation.**

- [ ] **Step 4: commit.** `cargo test --workspace`. `feat(mcp): supervisor token and capability model, supervise:self, register supervisor tools`.

---

### Task 4: End-to-end check (supervise:self over stdio), code-ranker, docs

**Files:**
- Create: `crates/wf-cli/tests/mcp_supervise_test.rs`
- Modify: `README.md`, `docs/tasks.md`, `CHANGELOG.md`

- [ ] **Step 1: e2e stdio test** `crates/wf-cli/tests/mcp_supervise_test.rs` - like `mcp_cli_test.rs`, but: initialize -> initialized -> `tools/call workflow_run {id, supervise:"self"}` on a workflow with no agent_task (deterministically reaches succeeded), obtain the `supervisor_token`; `tools/call supervisor_run_inspect {token}` -> success with the run_status. This proves that supervise:self starts a background run and the supervisor tools work by token over real JSON-RPC. Timeouts, `child.kill()` at the end. If catching a live wake deterministically is too hard, limit the test to inspect/report by token (this already proves the token gate and the background run).
- [ ] **Step 2: code-ranker.** `cargo metadata --format-version 1 >/dev/null` then `code-ranker check .`; on violations, fix worst-first + `cargo test --workspace`.
- [ ] **Step 3: README.** Extend the MCP section: list the supervisor tools and briefly describe the `supervise:self` flow (start -> wait_event -> inspect -> node_retry/continue/pause/abort/context_append -> report). Use plain hyphens.
- [ ] **Step 4: docs/tasks.md + CHANGELOG.** Under "Phase 4": mark `[x]` "Supervisor tools ..." and `[x]` "supervise: self ..." (as part of 4b); update the split note (4b done; 4c - background agent + heartbeat + report-in-web). CHANGELOG `### Added`: a line about the supervisor MCP tools and supervise:self.
- [ ] **Step 5: commit.** `docs: mark phase 4b (supervisor mcp tools + supervise:self) done`.

---

## What is deliberately NOT part of Phase 4b (for the reviewer)

- The background supervisor agent that the engine spawns for CLI/web runs; passing the endpoint+token to the agent via the adapter; heartbeat and `supervisor_lost` with a fallback background agent - Phase 4c.
- Auto-summarizing the report when the agent never submitted `supervisor_report` (session closed) - Phase 4c.
- The intervention journal and the report in the web UI - Phase 4c.
- `workflow_patch`/`run_migrated`/patch versions/promote-on-success - Phase 6.
- `node_slow`/`run_stuck` triggers (needs duration history) - later.
- A cryptographically strong supervisor token and a thin client (attaching to a running `wf serve`) - later hardening; in 4b it is boot-core stdio, and the token is binding+gating only.
