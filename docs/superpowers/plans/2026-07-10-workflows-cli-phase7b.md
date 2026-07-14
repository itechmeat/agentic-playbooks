# Phase 7b - wait nodes (timer + webhook) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Implement execution of the `wait` node: waiting on a timer or an external webhook signal with a per-run unpredictable hook_secret, an HTTP signal endpoint, and a mandatory timeout (timeout = node failure).

**Architecture:** The schema already parses `Wait{wait_for: Timer{seconds}|Webhook{key}, timeout_seconds, scope}`. drive executes wait with the same poll pattern as human_review (7a): it writes `WaitStarted`, spins a loop (the top-of-loop control scan keeps catching Pause/Abort, and no step budget is spent), and checks the completion condition; on a signal/timer expiry it writes `WaitSignalled` + NodeFinished(succeeded); on `timeout_seconds` expiry it writes `WaitTimeout` + NodeFinished(failed, which goes into the normal branching/wake path). Webhook signals arrive on the `signals.jsonl` channel (mirroring reviews.jsonl); the HTTP handler writes to it after checking the secret. The hook_secret is generated at run start (uuid v4) and stored in `hooks.json` inside the run folder. Single-writer holds: only drive writes events; HTTP writes to the signals channel.

**Tech Stack:** Rust (wf-core schema, wf-engine, wf-server axum, wf-cli), Bun+Vite+Svelte 5, vitest. New wf-engine dependency: `uuid` v4 (already in the workspace via wf-server) for the unpredictable hook_secret.

## Global Constraints

- Chat language is Russian; code/tests follow the project conventions (comments/docstrings in Russian, errors/code in English).
- NO em-dashes (U+2014) and no exclamation marks in code/comments/docs/UI.
- The project is at 0.1.0.
- Sanitize client-supplied run_id/secret/key (`is_safe_segment`/`is_safe_id`).
- Single-writer: only drive writes events; the HTTP hook handler writes `signals.jsonl`.
- The timeout is mandatory (schema: `timeout_seconds` is not an Option); timeout = node failure.
- hook_secret is unpredictable (uuid v4), bound to the run; a signal for a different run/secret must not be accepted.
- In tests, use small timer values (fractions of a second), polling as in review_test.
- Gates before this is ready: cargo build/test/clippy (0 new warnings), web build+vitest, code-ranker.

## File Structure

- `crates/wf-engine/Cargo.toml` - add `uuid` (feature v4).
- `crates/wf-engine/src/event.rs` - `WaitStarted{node, kind}`, `WaitSignalled{node}`, `WaitTimeout{node}`.
- `crates/wf-engine/src/signals.rs` (create) - the `signals.jsonl` channel: `SignalCommand{key}`, `post_signal`, `read_signals_after` (mirrors review.rs).
- `crates/wf-engine/src/hooks.rs` (create) - `generate_hooks(run_dir, wf)` (creates a key->secret entry in hooks.json for every Webhook wait), `read_hooks(run_dir) -> BTreeMap<String,String>`, `hook_path(run_id, secret)`.
- `crates/wf-engine/src/scheduler.rs` - drive: a `Wait` branch (timer/webhook poll, timeout); generating hooks at prepare time; export.
- `crates/wf-engine/src/context.rs` - `{{run.hooks.<key>}}` -> the relative path `/api/hooks/<run-id>/<secret>` (the monitor prepends the host; MVP).
- `crates/wf-engine/src/lib.rs` - re-exports for signals/hooks.
- `crates/wf-server/src/lib.rs` - `POST /api/hooks/{run_id}/{secret}` (validates the secret against hooks.json, writes the signal); run detail exposes `hooks` (key->path).
- `web/src/lib/{api.ts,types.ts}`, `web/src/pages/RunView.svelte` - display the hook URL(s) and the waiting status.
- Tests: `crates/wf-engine/tests/wait_test.rs`, a server hook test, web tests (if a pure helper can be factored out).
- Docs: CHANGELOG, tasks.md.

## Tasks

### Task 1: events + signals channel + hooks store
- Events WaitStarted{node, kind: "timer"|"webhook"}, WaitSignalled{node}, WaitTimeout{node}; fold is a no-op (like wake). Update all exhaustive matches (state.rs fold, context/inspect already have a `_` fallback).
- `signals.rs`: `SignalCommand{key}`, `post_signal(run_dir, cmd)->seq`, `read_signals_after(run_dir, after)->Vec<SignalEntry>` (a copy of review.rs with a key field).
- `hooks.rs`: `generate_hooks(run_dir, &Workflow)` - for every Wait{Webhook{key}} node, generate a uuid v4 secret, write hooks.json (a key->secret map) atomically; idempotent (create the file if it does not exist). `read_hooks(run_dir)->BTreeMap`. `hook_path(run_id, secret)->String` = format!("/api/hooks/{run_id}/{secret}").
- lib.rs re-export.
- Tests: signals round-trip; generate_hooks creates a secret for a webhook node and not for a timer one; read_hooks reads it back.

### Task 2: drive wait execution
- In drive, a branch `if let NodeKind::Wait { wait_for, timeout_seconds, .. } = &node_kind` (before the generic execute, next to human_review):
  - Idempotently (once per visit) write `WaitStarted`. Determine the start time as the timestamp of the WaitStarted event (the last one with no matching WaitSignalled/WaitTimeout for the node) via read_all; count WaitStarted vs. (Signalled|Timeout) the same way human_review does, for ordering/replay robustness.
  - Timeout: if now_millis - start_ts >= timeout_seconds*1000 -> WaitTimeout + NodeFinished(failed) -> proceed as an ordinary failed node (in autonomous mode - branch via fallback/next; in supervised mode - wake). Do NOT continue: let it go through the generic status handling? Simpler: write NodeFinished(failed) and go into the common failure-handling block. For simplicity: inside the branch, after WaitTimeout, set `status=Failed, output="wait timeout"` and do NOT build a separate path, but reuse the common failure tail. Implementation note: factoring out a common "tail after NodeFinished" is not trivial, so the Wait branch handles this terminally: timeout -> WaitTimeout + NodeFinished(failed) + (supervised? wake+await : next_node based on statuses) - the code cannot be cleanly reused, so we treat timeout as: log WaitTimeout, NodeFinished(failed), then `current = next_node(...)` in autonomous mode, and in supervised mode raise a wake and await (as a generic failure). To avoid duplication, we scope 7b down: timeout goes through next_node/fallback (autonomous) and wake (supervised) as a minimal repeat of the existing block.
  - timer: if now - start >= seconds*1000 -> WaitSignalled + NodeFinished(succeeded) -> next_node.
  - webhook: if signals.jsonl has a signal for the key (after the cursor/by count) -> WaitSignalled + NodeFinished(succeeded) -> next_node.
  - otherwise sleep(AWAIT_CONTROL_POLL), continue (no step budget spent).
- Tests (wait_test.rs, E2E): a timer completes successfully and ends in the success finish; a webhook signal (post_signal) completes successfully; a timeout (tiny timeout_seconds, webhook with no signal) -> a failure finish via the fallback.

### Task 3: prepare generates hooks + template
- In prepare_run/prepare (where RunStarted and the snapshot happen), call generate_hooks(run_dir, &wf) after run_dir is created.
- context.rs render: `["run","hooks",key]` -> hook_path(run_id, secret) from read_hooks. render/resolve needs to know the run_id + run_dir (or an already-built hooks map). Simpler: pass a `hooks: &BTreeMap<String,String>` map into render, already as key->path. Build it in drive/execute before rendering (read_hooks + hook_path). Update the render signature (+ its call sites, + context_test).
- Test: a prompt with {{run.hooks.<key>}} renders the path /api/hooks/<run-id>/<secret>.

### Task 4: HTTP hook endpoint + run detail hooks
- `POST /api/hooks/{run_id}/{secret}` -> is_safe_id both; read_hooks(run_dir); find the key by secret; if there is no match -> 404; otherwise post_signal(run_dir, {key}) -> {signalled: key}.
- get_run_handler: add `hooks` to the response: a key->path map (from read_hooks + hook_path).
- Tests: a valid secret -> 200 + signals.jsonl contains the key; a wrong secret -> 404; traversal -> 404; run detail contains hooks.

### Task 5: web
- types: RunDetail.hooks?: Record<string,string>. api: (nothing new, hooks arrive in run detail; signaling from the web UI is not required - this is for CI/external systems).
- RunView: if there are hooks and/or a node is waiting (wait_started with no signalled/timeout) - show a block with the key + the full URL (origin + path) and a copy button; show the waiting status. A pure `pendingWaits(events)` helper (+vitest), following the pendingReviews pattern.

### Task 6: docs + gates
- CHANGELOG + tasks.md (mark wait 7b as done).
- Gates: cargo build/test/clippy, web build+vitest, code-ranker, an em-dash scan.

## Self-Review
- Single-writer: WaitStarted/Signalled/Timeout is written only by drive; signals.jsonl - by HTTP.
- Timeout = node failure -> normal branching/wake (compatible with the 7a conditions).
- hook_secret uuid v4 is unpredictable, bound to the run (the endpoint validates the secret against that run's hooks.json).
- Waiting on wait does not spend the step budget (like human_review in 7a).
- Deferred to 7c: parallel branches + join. Broadcast scope: workflow is out of scope for 7b (per-run hooks only).
- MVP simplification (flag it): {{run.hooks.<key>}} = a relative path, the monitor shows the full URL (origin). The spec allows an MVP webhook running locally.
