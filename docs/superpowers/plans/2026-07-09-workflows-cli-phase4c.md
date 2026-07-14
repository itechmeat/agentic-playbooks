# Workflows CLI, Phase 4c (background agent, heartbeat, report and journal in the web UI) - implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** finish the supervisor. A run started from the CLI/web (without an agent session) gets a background agent that the engine spawns as a separate agent process; heartbeat and `supervisor_lost` guard against a silent agent; a final report always exists (either submitted via the `supervisor_report` tool, or an engine auto-summary built from the `supervisor_*` events); the web UI shows a live intervention journal and the agent's report.

**Architecture:** Task order goes from a testable foundation to agent-dependent infrastructure.
- **A report always exists (9.6):** the engine's `supervisor_report_or_summary(root, run_id)` returns `supervisor/report.md` if it was submitted, otherwise builds a markdown auto-summary from the events (the run outcome + the list of `WakeRaised` + the list of `SupervisorAction`). Pure, testable without an agent.
- **Visibility in the web UI (9.6, 12):** a new endpoint `GET /api/runs/{id}/report`; the run page shows an intervention journal (a separate feed of `WakeRaised`/`SupervisorAction` events) and a report panel.
- **Persistent supervisor session:** the 4b token model was in-memory per MCP process. The background agent is a separate agent process connecting to a separate `wf mcp`, so the `{token, capabilities}` session is persisted to `.wf/runs/<id>/supervisor/session.json` when minted; the server's `resolve_session` first checks in-memory, then falls back to disk (by token, scanning `.wf/runs/*/supervisor/session.json`). This bridges the process boundary.
- **Heartbeat + supervisor_lost (9.2):** `supervisor_wait_event` touches `.wf/runs/<id>/supervisor/heartbeat` (mtime). The engine (the drive stream, at a node boundary) checks the heartbeat age while a supervised run is live; if it is older than the threshold and an agent is still expected to be on duty, it logs `SupervisorLost` (written by the drive stream - the invariant holds) and, if a supervisor executor chain is configured, spawns a fallback background agent. There is no handoff back (9.2).
- **Background agent (9.1, 9.2):** for a supervised run WITHOUT a `supervise:self` session, the engine spawns the supervisor agent through `AgentAdapter`, passing it a minimal brief (id, version, params, run_id, token) and the MCP connection config in whatever way is native (for claude-code - via environment/brief). The spawn mechanics are tested with a stub (`WF_AGENT_CMD`); the agent's live loop against a real `claude` is verified manually (like the Phase 2 ping run), and this is stated plainly.

**Tech Stack:** Rust (edition 2024), rmcp 2.2.0; wf-core, wf-engine, wf-mcp, wf-server; web - Svelte 5 + vitest. No new external dependencies.

**Spec:** `docs/superpowers/specs/2026-07-08-workflows-cli-design.md` (9.1 lifecycle, 9.2 binding/heartbeat/supervisor_lost, 9.6 report; 12 monitor screen). Builds on Phases 1-3, 4a, 4b.

## Global Constraints

- Error text is in English; comments/documentation are in Russian. No em-dashes (U+2014) and no exclamation marks in code/docs/UI.
- Version stays at `0.1.0`. TDD. Commit at the end of each task.
- Event-sourcing invariant: `events.jsonl` is written ONLY by the drive stream (including `SupervisorLost`). The supervisor side writes `control.jsonl`, `supervisor/report.md`, `supervisor/session.json`, `supervisor/heartbeat`. Do not violate this.
- Any client-supplied `run_id`/`token` that ends up in a path goes through `is_safe_segment`.
- Do not break what already works: autonomous runs, supervise:self (4b), Phase 3 tools, the whole workspace stays green.
- Do not invoke a real `claude` in tests: verify spawn/heartbeat/lost with a stub; verify the web logic with vitest + an API test.
- Run code-ranker before marking a task done; navigate via codegraph.

---

### Task 1: Engine - report auto-summary

**Files:** Modify `crates/wf-engine/src/inspect.rs`, `lib.rs`; Test `crates/wf-engine/tests/report_summary_test.rs`.

**Interfaces:**
- `pub fn supervisor_report_or_summary(root: &Path, run_id: &str) -> Result<String, EngineError>` - is_safe_segment + run-dir NotFound; if `supervisor/report.md` exists - return its contents; otherwise build markdown: a heading with the `run_status` (from fold), then a `## Wakes` section with one line per `WakeRaised` (trigger, node, detail), then a `## Interventions` section per `SupervisorAction` (action, node, detail). Omit empty sections. Re-export from lib.rs.

- [ ] **Step 1: test** `report_summary_test.rs`: (1) traversal/missing -> NotFound; (2) a run with a written report.md -> it is returned; (3) a run with no report.md but with WakeRaised+SupervisorAction events -> the summary contains trigger/node/action; (4) a run with nothing at all -> a summary with the run_status and no sections.
- [ ] **Step 2:** confirm it fails.
- [ ] **Step 3:** implementation.
- [ ] **Step 4:** `cargo test --workspace`; commit `feat(engine): supervisor report auto-summary from events`.

---

### Task 2: Web - intervention journal and report panel

**Files:** Modify `crates/wf-server/src/lib.rs` (the `/api/runs/{id}/report` endpoint), do not touch `crates/wf-mcp`; `web/src/lib/api.ts`, `web/src/lib/types.ts`, `web/src/pages/RunView.svelte`; Test: `crates/wf-server/tests/*` (API) + web vitest, if a pure journal-formatting function can be factored out.

**Interfaces:**
- Server: `GET /api/runs/{id}/report` -> `{ report: string }` via `wf_engine::supervisor_report_or_summary` (is_safe_id gate, as in the existing endpoints). If the run is missing -> 404.
- web `api.ts`: `fetchRunReport(id) -> Promise<{report:string}>`.
- `RunView.svelte`: extract `wake_raised`/`supervisor_action` from `detail.events` into a separate "intervention journal" (a feed with trigger/action + node + detail); add a report panel/tab that loads via `fetchRunReport`. Live updates over WS as before.
- If it is convenient to factor out a pure `interventionJournal(events) -> Entry[]` function in `web/src/lib/graph.ts` or a separate module, cover it with vitest.

- [ ] **Step 1: tests** - API test: seed a run with supervisor/report.md, `GET /api/runs/{id}/report` -> 200 with the text; a traversal id -> 404/400. vitest on `interventionJournal` (filters wake/action, preserves order).
- [ ] **Step 2:** it fails.
- [ ] **Step 3:** implementation (server + frontend). Keep the full-screen layout (40px header, no extra panels - fit the journal/report into the existing aside or tabs).
- [ ] **Step 4:** `cargo test --workspace` + `cd web && bun run test` (vitest); commit `feat(web): intervention journal and supervisor report panel on run page`.

---

### Task 3: Engine - persistent session + heartbeat + supervisor_lost

**Files:** Modify `crates/wf-engine/src/{inspect.rs,scheduler.rs,event.rs,lib.rs}`; `crates/wf-mcp/src/{tools.rs,server.rs}` (touching the heartbeat inside wait, disk-fallback token resolution); Tests: `crates/wf-engine/tests/heartbeat_lost_test.rs`, additions to the mcp tests.

**Interfaces:**
- Event `EventPayload::SupervisorLost { detail: String }` (log-only; fold is a no-op on statuses). build_context does not touch it.
- Persistent session: `pub fn write_supervisor_session(root, run_id, token, capabilities: &[String]) -> Result<(), EngineError>` writes `.wf/runs/<id>/supervisor/session.json`; `pub fn find_session_by_token(root, token) -> Result<Option<(String /*run_id*/, Vec<String> /*caps*/)>, EngineError>` scans `.wf/runs/*/supervisor/session.json`. The server's `mint_token` additionally calls `write_supervisor_session`; when `resolve_session` misses in-memory it calls `find_session_by_token` (disk fallback), so that a separate `wf mcp` process (for the background agent) can validate the token.
- Heartbeat: `pub fn touch_heartbeat(root, run_id) -> Result<(), EngineError>` writes the current ts to `.wf/runs/<id>/supervisor/heartbeat`; `pub fn heartbeat_age_ms(root, run_id) -> Result<Option<u128>, EngineError>`. `supervisor_wait_event` (tools.rs) calls `touch_heartbeat` at the start.
- supervisor_lost: inside `drive` (at a node boundary, only when the run expects an external agent - see the Task 4 flag in run.yaml/RunConfig, e.g. `supervisor_expected: bool`) check `heartbeat_age_ms`; if it is older than the threshold (e.g. 60s) and `SupervisorLost` has not been logged yet - log `SupervisorLost`; spawning a fallback background agent happens through the Task 4 hook (in 4c-Task3 it is enough to log the event; the spawn is Task 4). The threshold comes from a constant (testable by lowering it via env/parameter, or by testing `heartbeat_age_ms` + the decision logic as a separate pure function).

- [ ] **Step 1: tests** `heartbeat_lost_test.rs`: (1) write/read session + find_by_token (including the traversal guard); (2) touch_heartbeat -> heartbeat_age_ms is small; missing -> None; (3) a pure decision function `should_declare_lost(age, threshold, already_logged) -> bool`; (4) SupervisorLost round-trip + fold no-op. MCP: disk-fallback resolution - `mint` writes the session, a fresh `WfMcp` (empty in-memory) resolves the token from disk.
- [ ] **Step 2-3:** it fails -> implementation.
- [ ] **Step 4:** `cargo test --workspace`; commit `feat(engine): persistent supervisor session, heartbeat, supervisor_lost`.

---

### Task 4: Engine/CLI - spawning the background agent and wiring it up

**Files:** Modify `crates/wf-engine/src/{scheduler.rs,adapter.rs,run_config.rs}`, `lib.rs`; `crates/wf-cli/src/main.rs` (a `--supervise` flag on `wf run`); Test: `crates/wf-engine/tests/background_supervisor_test.rs`.

**Interfaces:**
- `RunConfig`/`RunOptions` gain a field marking that the run expects an external agent (`supervisor_expected: bool`), so Task 3 knows when to enable heartbeat monitoring.
- `AgentAdapter` gains a way to spawn the supervisor: a method `spawn_supervisor(&self, brief: &SupervisorBrief) -> Result<(), (ErrorClass, String)>` (or a standalone function), where `SupervisorBrief { run_id, token, workflow_id, version, mcp_hint }`. For `ClaudeAdapter` - build the command `claude -p <brief> --model <model>` with the MCP config (env/flag) so the agent sees `wf mcp` in this folder; the connection details follow whatever is native for claude-code. In tests, a stub (`WF_AGENT_CMD`) checks that the spawn is invoked with the expected brief (the stub writes the received arguments to a file), without starting a real agent.
- `run_background`, when `supervisor_expected && supervise != self`: after starting the drive thread, mint+persist a token (through the server? no - the engine does it itself: `write_supervisor_session` + generate the token in the engine) and spawn the supervisor via the adapter, passing the brief. The engine generates the token the same way (`sv-<millis>-<n>`); capabilities come from `supervisor.policy` (either factor the parser out into wf-core or duplicate it minimally).
- CLI: `wf run <id> --supervise` -> `RunOptions{ mode: Supervised, supervisor_expected: true, .. }` via `run_background` (non-blocking), printing the run_id; without `--supervise` - as now (synchronous).
- Spawn + heartbeat monitoring + supervisor_lost are wired together: when the heartbeat is lost, the engine respawns the background agent (using the supervisor executor chain; in 4c a single retry + event is enough).

- [ ] **Step 1: tests** `background_supervisor_test.rs` (a stub agent writes the brief to a file): (1) supervised+supervisor_expected -> the adapter receives a brief with the correct run_id/token/workflow; the session is persisted to disk and validated via `find_session_by_token`. (2) with a stale heartbeat, `SupervisorLost` is logged and a respawn happens (the stub is invoked twice). All of this deterministic, with timeouts, without a real agent.
- [ ] **Step 2-3:** it fails -> implementation.
- [ ] **Step 4:** `cargo test --workspace`; commit `feat(engine): spawn background supervisor agent, wire heartbeat-lost respawn; wf run --supervise`.

---

### Task 5: Manual verification with a real agent, code-ranker, status

**Files:** Modify `docs/tasks.md`, `CHANGELOG.md`, `README.md`.

- [ ] **Step 1: manual verification** (documented like the Phase 2 ping run). In a real project with `.wf` and a workflow whose agent_task stumbles: `wf run <id> --supervise`; confirm the engine started the background agent (claude-code), that when the node failed the agent woke up, inspected via the tools and intervened, and that the report shows up in `supervisor/report.md` and is visible in the web UI. If the environment is headless and a real agent is unavailable - rely on Task 4's stub tests and the Phase 4b e2e test, and document that.
- [ ] **Step 2: code-ranker.** `cargo metadata --format-version 1 >/dev/null && code-ranker check .`; fix worst-first on violations.
- [ ] **Step 3: README** - a section on the background agent: `wf run --supervise`, how the engine starts the supervisor itself, heartbeat/supervisor_lost, the report in the web UI.
- [ ] **Step 4: tasks.md + CHANGELOG.** Mark `[x]` "Background agent", "supervisor_lost", "Report + intervention journal"; close the split note (Phase 4 fully done). CHANGELOG `### Added`: a line about the background agent, heartbeat/supervisor_lost, the auto-summary, and the journal/report in the web UI.
- [ ] **Step 5: commit** `docs: mark phase 4c (background supervisor + web report) done; phase 4 complete`.

---

## What is deliberately NOT part of Phase 4c (for the reviewer)

- `workflow_patch`/`run_migrated`/patch versions/promote-on-success - Phase 6 (needs the Phase 5 versioning machinery). The background agent in 4c fixes things via retry/continue/context_append, but does NOT patch the workflow.
- `node_slow`/`run_stuck` triggers (needs duration history in `runs/index.jsonl`) - later.
- A cryptographically strong token; a thin client (attaching to a running `wf serve`) - later hardening.
- A full visual editor/version history - Phase 5.
- The background agent's live loop against a real agent is covered by manual verification (Task 5), not automated tests (agent-dependent).
