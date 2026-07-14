# Phase 8 - agent/runner breadth + robustness Implementation Plan (decomposition)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans per sub-phase. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Expand execution breadth and robustness: agent_task timeout, context compaction, a global config with an agent/runner registry, extensible runners; ACP/multi-agent are deferred by the spec until after real-world use.

**Architecture:** Layered on top of the stable execution core (Phases 1-7); invariants (event sourcing, single-writer, resume) must not be broken. Decomposed into independent increments, each its own commit with gates.

## Global Constraints (for all sub-phases)

- Chat language is English; code/tests as in the rest of the project (comments/docstrings in English, error text/code in English).
- NO em dashes (U+2014) and no exclamation marks in code/comments/docs/UI.
- Project stays at 0.1.0; version is not bumped per sub-phase.
- Single writer for events (only drive writes events.jsonl); resume/replay determinism must be preserved (LLM artifacts go into separate files, not into the primary log/context.md).
- Sanitize paths coming from the client/config; the global config lives outside the project (`~/.config/wf/`), the project config references it by id.
- Gates before a sub-phase is considered done: cargo build/test/clippy (0 new warnings), web build+vitest (if the web was touched), code-ranker, an em-dash scan.

---

## 8a - agent_task timeout  [DONE, awaiting commit]

Gap: `agent_task` ignored `timeout_seconds` (it could hang forever). Implemented on top of the killable adapter from 7c-3.
- `AgentTask.timeout: Option<Duration>`; `ErrorClass::Timeout`; `run_cancellable` kills the process at the deadline and returns Timeout.
- `execute_node`: passes through `timeout_seconds`, on Timeout writes an AttemptFinished `timed_out`, aborts the retry on the executor (as a transport error) and proceeds to fallback; the final node status is `TimedOut` if the last attempt timed out.
- Test `agent_timeout_test`: the agent sleeps 5s, timeout 1s -> killed (~1s), node TimedOut, the run goes to fallback into failure.
- Gates green (clippy 36).

Remaining at commit time: a line in CHANGELOG/tasks.md.

---

## 8b - context compaction (context_compact.md)  [DONE, awaiting commit]

Implemented: event `ContextCompacted{compact_file, model, up_to_seq}` (fold no-op, the summary is NOT written to the log). context.rs: `sections`/`build_context_tail`/`sections_between`/`compaction_boundary`/`latest_compaction`/`build_context_for_render`. RunConfig+RunOptions: `context_max_bytes`, `context_compact_model`. drive triggers `maybe_compact_context` before nodes that render the context (the only writer of the event is drive; context_compact.md is a materialized artifact). execute_node renders through `build_context_for_render` (summary + uncompacted tail). The primary context.md is left untouched. Test `context_compaction_test`. Gates green (clippy 36).

### 8b - original sketch

Spec 8.5: when the configurable size limit for `{{run.context}}` is exceeded, the engine asks a cheap model to compact the OLD sections; the result is written to a separate `context_compact.md` (an LLM artifact, non-deterministic), while the primary `context.md` is NOT replaced (replay determinism). The compaction event references the compact file and the model. Nodes are given the compacted content plus the uncompacted tail of the full context.

**Files:** `wf-engine/src/context.rs` (threshold, split into old/tail), `scheduler.rs` (trigger compaction before rendering the prompt; event `ContextCompacted{file, model}`; `event.rs`), `run_config.rs` or the global config (size limit, cheap model), adapter (calling the cheap model for compaction).

**Tasks (sketch):**
1. Event `ContextCompacted{compact_file, model, up_to_seq}` (fold no-op); size threshold (config, default e.g. 32KB).
2. Logic: before assembling the context for the prompt, if `context.md` exceeds the limit and there is no up-to-date compact for the current seq - compact the sections up to `up_to_seq` with the cheap model (executor from config), write `context_compact.md`, emit the event.
3. Rendering: `{{run.context}}` = compact (if present) + the uncompacted tail (sections after up_to_seq).
4. Tests: a large context -> ContextCompacted recorded, context.md untouched, rendering = compact + tail; replay produces the same context.md.

**Note:** compaction is non-deterministic (LLM) - NOT written to the primary log; this is a key replay invariant.

---

## 8c - global config `~/.config/wf/config.yaml`  [DONE, awaiting commit]

Implemented: `wf-core/src/config.rs` (GlobalConfig{port, default_executor, agents, executors, runners}, AgentDef{program}, `config_dir` via WF_CONFIG_DIR/XDG_CONFIG_HOME/HOME, `load` errors out on malformed YAML, `agent_program`/`executor_names`). prepare_run: `merge_global_config` merges in the global executors (local ones override by name) and the default_executor. `adapter_for`: WF_AGENT_CMD > agents.<id>.program > "claude". The serve port and validation (cli/mcp/server) read the config. Tests `config_test` (wf-core), `global_config_test` (wf-engine). Note: standalone `wf validate` does not apply the default_executor merge (it only resolves executor names), so a workflow relying on the config's default_executor will run fine, but `wf validate` may complain - this asymmetry is noted, to be fixed if needed.

### 8c - original sketch

Spec 4.2 / 7.1: descriptions of coding agents (id -> transport, headless_args, model defaults), global named executors, and the runner registry live in the CLI's root config. The project's `config.yaml` references agents by id and overrides defaults, but does NOT describe launch commands. Local executors override global ones by name.

**Files:** `wf-core/src/config.rs` (create; structures GlobalConfig{agents, executors, runners, defaults, port}, loading from `~/.config/wf/config.yaml` with XDG resolution, missing file = empty default), integration into `executor.rs` (executor resolution: workflow-local -> global), `adapter.rs`/`script.rs` (commands/runners from the config, not hardcoded), CLI/serve (port from config).

**Tasks (sketch):**
1. `GlobalConfig` + loading (XDG `~/.config/wf/`, override via env e.g. `WF_CONFIG_DIR` for tests), default when absent.
2. Executor merging: local (workflow) overrides global by name; resolution in `executor.rs` considers both sources.
3. Agents by id: `adapter_for` reads the agent description (program/transport/headless_args) from the config instead of hardcoding `claude`. Default (no config) = the current ClaudeAdapter from WF_AGENT_CMD.
4. serve port and default executor - from config (project overrides).
5. Tests: loading/default, executor merging (local overrides), agent by id.

**Caution:** do not break the current no-config path (WF_AGENT_CMD, port 7321).

---

## 8d - extensible runner registry  [DONE, awaiting commit]

Implemented in `script.rs`: `runner_candidates` (config `runners` extends/overrides the default ts:[bun,deno]/py:[python3]/sh:[sh]; an unknown key with no entry is an error), `is_in_path` (a manual PATH scan without external crates), `command_for_runtime` (bun/uv run, deno run -A, otherwise program+script). run_script takes the first available runtime, and if none is available, returns a clear error "no runtime available (tried: ...)". Test `runner_registry_test` (unknown key, no-runtime, config-extended key picks the first available, ts via bun if present). The default sh path and old script tests are untouched.

### 8d - original sketch

Spec 7.4: `runner: ts` -> bun (default) or deno; `py` -> python3 / uv run; `sh` -> sh/zsh. The registry is extensible via the global config (`runners: { ts: [bun, deno], ... }`), and the first available runtime on the machine is used. Currently `script.rs` hardcodes bun/python3/sh.

**Files:** `script.rs` (resolve the runner command from the registry, pick the first available via a `which`-like check), `config.rs` (runners section), runner tests (ts/py) - marked `#[ignore]` or gated on the presence of bun/python3, so they do not flake in CI without those runtimes.

**Tasks (sketch):**
1. Runner registry (default: ts:[bun,deno], py:[python3], sh:[sh]) from config (8c) or the default.
2. Picking the first available runtime (checked in PATH); a clear error if none is found.
3. Tests for sh (always), ts/py - conditional (skipped if the runtime is missing).

Depends on 8c (registry in config). Can be done after 8c, or with a default registry without config.

---

## 8e - ACP transport + agent breadth  [PARTIALLY DONE (8e-1), awaiting commit]

At the operator's request, done "to a reasonable extent for now, ACP is available in principle, we will finish it after real-world use."

### 8e-1 - pluggable transport + streaming acp  [DONE]
- Config: `AgentDef.transport` (`headless` default | `acp`), `Transport` enum in wf-core/config.rs, `agent_transport`.
- adapter.rs: `ClaudeAdapter{program, transport}`, `run_cancellable` branches into `run_headless` (previous behavior) and `run_acp`. `AgentTask.stream_log`. `check_cancel_timeout` shared.
- `run_acp`: `claude -p ... --output-format stream-json --verbose`, reading stdout line by line on a background thread, streamed into a per-attempt NDJSON file `runs/<id>/agent-stream/<node>-<attempt>.jsonl`, extracting `type:result` (`is_error` -> Failed), stderr drained on a separate thread (anti-deadlock). Error classes: Transport/ProcessExit/StructuredOutputMissing + cancel/timeout kill.
- execute_node passes the per-attempt stream_log; adapter_for resolves the transport from the config (WF_AGENT_CMD -> headless).
- Tests: `acp_adapter_test` (6: success+stream log, is_error->Failed, no-result->StructuredOutputMissing, nonzero->ProcessExit, timeout kill, cancel), `acp_config_test` (transport acp chosen from config, stream log written).
- Provisional markers left in spec 7.2.

### 8e-2 - honest isolation level  [DONE declaratively]
Implemented: enum `Isolation{full|best_effort|none}` (default none) and an `isolation` field on `NodeKind::AgentTask` (schema.rs); validator V16 warns that full/best_effort are declared but not enforced by the engine (check_isolation in validate.rs); an isolation select field on the node form in the web UI (NodePanel.svelte, edits go through updateNode/YAML AST). Test v16 in validate_semantics_test, markers in spec 7.2/8.3. Actual enforcement (per-node worktree) is deliberately left for the future (spec 8.3).

### 8e-3 - full Agent Client Protocol  [planned, not done]
JSON-RPC over the `acp` value (initialize/session.new/session.prompt/session.update, permissions, client callbacks), adapters for agents other than Claude Code, tightening the stream-json schema based on real output. Separate plan when this is picked up.

### 8e - original formulation (deferred by the spec)

Spec 7.2 (line 461): "For the first implementation - a single headless adapter (Claude Code) and a single output format; generality is designed AFTER real-world use." Plus the honest isolation level (`isolation: full|best_effort|none`, per the spec, visible in the web UI on the node form.

Not planned in detail right now: the ACP connection (persistent, streaming, permissions), adapters for agents other than claude-code, parsing stream-json - these are designed after the headless path sees real-world use. When we get to it - a separate plan (design the transport layer on top of AgentAdapter, add `ErrorClass::Transport` handling for ACP disconnects -> fallback, an isolation field in the schema/web UI).

---

## Fixes from external review (2026-07-10, before the Phase 8 commit)

Checked against the code, all four are valid and applied:
- adapter.rs: cancellation/timeout kill the agent's entire process group (Unix `process_group(0)` + `kill(-pgid, SIGKILL)`, non-Unix fallback `child.kill()`), so as not to orphan the process tree. libc is already a transitive dependency. Test `process_group_test` (grandchild is dead after the timeout).
- scheduler.rs `maybe_compact_context`: the synchronous call to the compaction model is bounded by a timeout (COMPACTION_TIMEOUT 120s), so a hung model does not stall drive; on timeout - falls back to the previous Ok(None).
- script.rs `runner_candidates`: a `GlobalConfig::load()` error is propagated as `EngineError::Script` rather than swallowed by `unwrap_or_default` (a malformed config now produces a clear error).
- script.rs `command_for_runtime`: runtime classification is by basename, so an absolute path (`/opt/homebrew/bin/bun`) still gets the `run` arguments; the full path is kept in the Command. Test 3b in `runner_registry_test`.

## Order and dependencies

- 8a - done.
- 8b (compaction) - independent, can go next.
- 8c (global config) - foundation for 8d; introduce carefully (do not break the no-config path).
- 8d (runners) - after/with 8c.
- 8e (ACP/multi-agent) - deferred by the spec, a separate plan when needed.

Recommended commit order: 8a (now) -> 8b -> 8c -> 8d. 8e - on explicit request.
