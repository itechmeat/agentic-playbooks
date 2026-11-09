# Proposal: DeepSeek Harness (`dsh`) as a built-in agent

## Why

DeepSeek Harness (`dsh`, package `@deepseek-ai/dsh`) is a coding agent CLI
with a headless one-shot app, a machine-readable event stream, persisted
sessions and a multi-provider model layer (the official DeepSeek route plus
any provider of its `pi-ai` adapter). apb cannot run it today: a profile
naming `dsh` falls back to the claude form and fails. The owner runs it on
this machine for delegated work, so a playbook node should be able to run on
it like on codex or opencode: launched headless, its reply, session and token
usage read from its own output, resumed by session id, checked by `apb
doctor`, and listed in the dashboard.

## What changes

1. A new agent module `apb_core::agents::dsh` (directory: `mod.rs`,
   `output.rs`, `model.rs`, `mcp.rs`, `actions.rs`) registered last in the
   agent registry, verified against `dsh` 0.2.2-rc.1.
2. Launch form `dsh --profile headless --json --permission-mode read-only
   --approval-policy never -` with the prompt on stdin; autonomy switches to
   `--permission-mode danger-full-access`. Resume form adds `--session-id
   <id> -- <prompt>`. A fresh session is printed (the first event).
3. Model selection through a run-scoped dsh patch file passed as the
   launcher's `--patch` in front of the form: dsh has no model flag, the
   model is the `agent-default-model` row of the composed profile. This
   needs one new hook on `HostSetup` (`scoped_args`) that the single spawn
   path applies, the argv twin of `scoped_env`.
4. An output reader of the `--json` event stream (reply, session id, token
   usage, closing record, error flag, failure wording the classifier reads),
   a tool-fact decoder for the grader's transcript view, and the agent
   contract fixture from real recorded streams.
5. Detection, doctor check, environment set, cost basis, billing account per
   provider route, closed model list (the two models the official DeepSeek
   API serves), and `apb setup` registration of apb's MCP server in dsh's
   home patch layer.
6. Docs: `docs/AGENT-SETUP.md`, the agent lists, CLAUDE.md and AGENTS.md,
   and decision record 0013 for the leading argv hook.

## What does not change

No other agent's forms, outputs or environment. No new journal event. The
dashboard gets its lists from the server as before; only a hint naming the
session-continuing agents gains `dsh`.

## Impact

- `crates/apb-core/src/agents/` (new module, `setup.rs` hook, registry).
- `crates/apb-engine/src/adapter/` (the spawn path applies the hook),
  `crates/apb-engine/src/transcript/` (dsh tool facts).
- Tests: module unit tests, `agents_test`, the agent contract suite, detect
  and doctor tests.
