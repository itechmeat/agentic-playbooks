# Playbooks CLI - task status

Quick progress assessment. Spec: `docs/superpowers/specs/2026-07-08-workflows-cli-design.md`.
Phase plans: `docs/superpowers/plans/`.

Legend: `[x]` done, `[~]` partial, `[ ]` not started.

## Workflow (for self, mandatory)

Run code-ranker NOT on every change, but once before marking a task done
(`[x]`) in this file (offline, already installed):

1. `cargo metadata --format-version 1 >/dev/null` - warm the cargo cache (needed by the rust plugin, otherwise an offline error).
2. `code-ranker check .` - gate: exit != 0 on a violation, auto-detects languages.
3. If there are violations: `code-ranker report . --output.scorecard --focus ADP --top 1` (worst-first), read `code-ranker docs base <ID>` BEFORE fixing, fix, repeat from step 2.

Check the report, draw conclusions, fix as needed. Goal: correct code structure: dependency cycles (ADP), cohesion (HK), complexity, SOLID/DRY/KISS. Artifacts in `.code-ranker/` (baseline for `--baseline` diffs).

Navigate the code through **codegraph** (symbol/edge index) - use it actively instead of manual grep/reading.

## Phase 1 - core and viewer (done, tag v0.1.0)

- [x] **Cargo workspace** - three crates apb-core / apb-server / apb-cli, edition 2024.
- [x] **playbook.yaml schema** - domain model on serde, `Playbook::from_yaml`.
- [x] **Validator V01-V08** - graph structure: id uniqueness, single start, finish has no outgoing edges, edges point to existing nodes, reachability.
- [x] **Validator V09-V15** - semantics: condition coverage, reference order, cycles via max_loops (Tarjan), script path escapes, templates, executor/profile references, duplicate agents in fallbacks.
- [x] **Atomic write** - temp + fsync + rename for control files.
- [x] **init `.apb/`** - creates the playbooks/profiles/runs/config structure, idempotently.
- [x] **Playbook registry** - versions, current pointer, layouts, profiles, version-mismatch control.
- [x] **CLI init/list/validate** - exit codes 0/1/2.
- [x] **HTTP API** - health, playbook list, detail with validation.
- [x] **serve.lock** - root fingerprint + instance id, released on graceful shutdown (SIGINT/SIGTERM).
- [x] **Watcher + WebSocket** - live pickup of disk changes without a restart.
- [x] **Frontend: scaffold** - Bun + Vite 8 + Svelte 5 + TypeScript, proxy to :7321.
- [x] **Frontend: model -> graph** - conversion to svelte-flow, dagre auto-layout (vitest).
- [x] **Frontend: list and graph** - read-only viewer, live reload over WS.
- [x] **Static baking** - rust-embed, `apb serve` opens the browser.
- [x] **Node connection points** - start has no incoming, finish has no outgoing (visual).
- [x] **Full-screen layout** - 40px header, graph fills the area, no side panels or footer.

## Phase 2 - execution engine

Phase 2a (engine, headless/CLI) implemented in the `apb-engine` crate; plan `docs/superpowers/plans/2026-07-09-workflows-cli-phase2a.md`. Phase 2b (web run monitor) implemented; plan `docs/superpowers/plans/2026-07-09-workflows-cli-phase2b.md`.

- [x] **Event-sourced engine** - append-only `events.jsonl`, state = fold, `apb run/runs/resume`. Replaced the minimal runner.
- [x] **Replay/resume** - `apb resume <run-id> [--from-node]`, detects `interrupted` from a broken-off attempt.
- [x] **AgentAdapter + ClaudeAdapter** - headless adapter (claude-code only for now); ACP/stream-json and other agents later.
- [x] **Retry** - node retry via `max_retries`.
- [x] **Fallbacks** - switch to the next agent in the executor's chain.
- [x] **Named executors** - node/defaults resolution, primary + fallback chain.
- [x] **Shared context** - `context.md` as a materialized view from events, available to all nodes.
- [x] **Template substitution at run start** - `{{params.*}}`, `{{run.instruction/context}}`, `{{nodes.*.output/report}}`.
- [x] **One-off instruction** - `apb run --instruction`, stored in `run.yaml`.
- [x] **Version snapshot per run** - immutable `runs/<id>/playbook.yaml` (groundwork for the "effective playbook"/overrides).
- [x] **Strict single-writer workdir lock** - `--allow-shared-workdir` to bypass.
- [x] **enforced max_loops** - a condition node that exhausts the loop limit takes the fallback edge or fails the run (instead of spinning up to the step ceiling).
- [x] **Nodes start / prompt / condition / finish / agent_task / script(sh)** - executed and covered by tests.
- [x] **Path safety** - id/version go through a containment check (protection against path traversal in the registry and server MCP handlers).
- [x] **Web run monitor** - `/api/runs` + `/api/runs/{id}`, watcher on `.apb/runs` with a `runs_changed` WS event, run-list and monitor pages with live node status highlighting and an event feed (Phase 2b).

Moved to later phases (not Phase 2): parallel branches and human_review/wait - Phase 7; ts/py runners, agent_task timeout, context compaction, ACP/other agents, worktree isolation - Phase 8.

Order of remaining phases was reshuffled by dependency: the MCP transport comes before the supervisor (the controlling agent rides on it), the editor before the controlling agent's self-patching (needs the version/diff/migration machinery). Nothing was dropped from scope, only reordered.

## Phase 3 - MCP transport + read/run tools (done; plan 2026-07-09-playbooks-cli-phase3.md)

- [x] **`apb mcp` stdio server** - JSON-RPC (rmcp), resolves `.apb/` from cwd, boot-core mode.
- [x] **Read tools** - playbook_list, playbook_get, playbook_validate.
- [x] **Run tools** - playbook_run (params, instruction), run_status, run_events (pagination by seq), run_report, run_resume. Blocking (foreground) run; background (non-blocking) mode and run_cancel - Phase 4 (needed to observe an in-flight run).
- [~] **Claude Code integration** - connecting via `claude mcp add` / `.mcp.json` documented, end-to-end JSON-RPC smoke test (initialize -> tools/call playbook_run) green; manual verification in an interactive Claude Code session in this environment not done.
- [ ] Groundwork for the supervisor token model and thin-client mode (attach to a running `apb serve`) - implementation in Phase 4 / later.

## Phase 4 - Controlling agent (CA): observation + repair (was 3A; on top of MCP)

Phase 4 is fully done: 4a (engine infrastructure) + 4b (supervisor MCP tools + supervise: self) + 4c (background CA, heartbeat/supervisor_lost, report + intervention log in the web UI). node_slow/run_stuck (part of the wake triggers) remain deferred - needs run-duration history (`runs/index.jsonl`), later.

- [~] **Wake triggers** - node_failed/node_timeout/anomaly and the wake-event queue in the engine are ready; node_slow/run_stuck deferred (needs run-duration history), later.
- [x] **Engine foundations 4a** - background (non-blocking) run and run_cancel, control command channel (control.jsonl: retry/continue_from/pause/abort), supervised run mode (RunMode), observation primitives wait_wake/run_inspect. This is the engine infrastructure the supervisor (4b/4c) relies on.
- [x] **Supervisor tools** - supervisor_wait_event (long-poll), run_inspect, node_retry, run_continue_from, run_pause/abort, context_append, supervisor_report (by supervisor token).
- [x] **Background CA** - the engine spawns an agent with an endpoint+token, lifecycle until run_finished; capability model.
- [x] **supervise: self** - the calling coding-agent session becomes the CA (via MCP).
- [x] **supervisor_lost** - heartbeat, a fallback background CA.
- [x] **Report + intervention log** - supervisor/report.md, live log in the web UI.

## Phase 5 - Visual editor + minor-version machinery (was 4)

Done: 5a (version-machinery backend + write tools/API; plan 2026-07-09-workflows-cli-phase5a.md) and 5b (browser visual editor; plan 2026-07-09-workflows-cli-phase5b.md).

- [x] **Version machinery** - minor-version creation (copying scripts/, applying edits, validation), atomic number issuance (temp + rename), immutable version folders, soft delete to trash + restore.
- [x] **Playbook CRUD from the web UI** - backend (MCP write tools + HTTP POST/PUT/DELETE) and UI (create, duplicate, delete to trash in the list).
- [x] **Node and edge editor** - type palette, property forms per node type, edge connect/delete; edits via the YAML AST preserve all fields.
- [x] **Layout persistence** - writes `layouts/<version>.yaml` (mutable), HTTP `PUT .../layout` endpoint, autosave on node drag.
- [x] **Version diffs** - structural (nodes +/-/changed, edges) and text YAML (LCS), HTTP `GET .../diff` endpoint, version switcher + diff panel in the editor.
- [~] **CodeMirror** - deliberately deferred: YAML is edited in a plain text field (`CodeEditor` on a textarea, a stable interface - the CodeMirror return point). No syntax highlighting/script editor yet, not required.
- [x] **MCP write tools** - playbook_create/update/delete on the shared version machinery (plus the same operations over HTTP).

## Phase 6 - CA self-patching, self-improvement (was 3B; on top of the editor + CA)

- [x] **playbook_patch tool (6b)** - the supervisor edits the playbook YAML (MCP supervisor_patch_playbook + capability patch_playbook, default capabilities = all).
- [x] **Patch versions + run migration (6a)** - run_migrated, migration rules 10.3, immutable snapshot of the patched version.
- [x] **Promote on success (6a)** - current only advances on a successful finish of the patched run.
- [x] **Patch classification (6a)** - improvement (promote) vs workaround (no promote); promote_supervisor_patches policy.
- [x] **Per-run patch limit (6a)** - max_patches_per_run, stop with a diagnosis.
- [x] **Version history in the web UI (6b)** - patch provenance, promoted or not, manual promote button; HTTP GET .../versions + POST .../promote.

## Phase 7 - Node types and branching

- [x] **human_review (7a)** - web UI review, `apb review`, review_decide; plus full branching (review_status, output_match) in next_node.
- [x] **wait (7b)** - timer + webhook with hook_secret (uuid v4), HTTP signal endpoint, {{run.hooks.<key>}}, timeout = node failure.
- [x] **Parallel branches (7c)** - fork/join (all|any), frontend model (7c-1); true concurrency for agent_task/script across threads, drive stays the single writer (7c-2); kill the losing branch's process on join:any (7c-3). Per-branch workdir isolation (worktree) - future work.

## Phase 8 - Agent and runner breadth

Decomposition plan: docs/superpowers/plans/2026-07-10-workflows-cli-phase8.md (8a..8e).
- [x] **agent_task timeout (8a)** - kill the process on timeout_seconds (killable adapter), TimedOut status, fallback.
- [x] **Context compaction (8b)** - context_compact.md via a cheap model, the primary context.md is left untouched (replay). ContextCompacted event (file+model+up_to_seq, no summary in the log), trigger in drive, context_compaction_test.
- [x] **Global config (8c)** - `~/.config/apb/config.yaml` (agents by id, named executors, defaults, port); local overrides it. apb-core/config.rs, merged in prepare_run, adapter_for from config, port/validation wired up. Tests config_test, global_config_test.
- [x] **Extensible runners (8d)** - registry ts:[bun,deno]/py:[python3]/sh from config, first one available on PATH; clear error when no runtime is present. Test runner_registry_test.
- [~] **AgentAdapter extension (8e)** - partial: 8e-1 pluggable transport (headless|acp), streaming acp over stream-json (per-attempt NDJSON log, error classes, kill on cancel/timeout), transport selection from config. Tests acp_adapter_test, acp_config_test. Marked provisional in spec 7.2.
  - [~] **8e-2** - honest isolation level (full|best_effort|none): schema field on agent_task, node form in the web UI (select), validator warning V16 for unenforced isolation. Real enforcement (per-node worktree) - future work (spec 8.3). Test v16 in validate_semantics_test.
  - [ ] **8e-3** - full Agent Client Protocol (JSON-RPC sessions, permissions, agents other than claude-code); separate plan.

## Phase 9 - Operations and distribution

- [x] **`apb doctor`** - environment diagnostics (agents, executors, profiles, runners). apb-core/doctor.rs (diagnose -> DoctorReport, Ok/Warn/Fail checks: config, registry, agent-binary PATH, runtime availability, playbook validity), CLI `apb doctor` with a return code. Runner registry and PATH helper moved into apb-core. Test doctor_test.
- [x] **`apb dev`** - dev mode: API server on 7321 (Vite's proxy target) in the background + Vite HMR (`bun run dev` in web/) as a child process; clear error without web/ or bun.
- [x] **Distribution** - GitHub Actions: CI (build web + clippy/test) and release (binaries for macOS arm64/x64 and Linux x64 on tag v*, web built before cargo because of rust-embed); docs/INSTALL.md (binaries / brew / cargo install, web-build requirement); brew formula template packaging/apb.rb.
- [x] **Playbook export/import** - apb-core/bundle.rs (PlaybookBundle: raw playbook.yaml + layout as JSON), CLI `apb export`/`apb import` (import creates a new version per the project's scheme, validates, restores layout). Tests bundle_test, phase9_cli_test.

## Follow-up on previously deferred items (after Phase 9)

- [x] **Agent report contract (spec 6.2)** - agent-node status comes from the final ```yaml block (status: success|failure) in the response, not from the return code; includes branching on node_status. Backward compatible: no block -> success (the strict "no block -> unknown+anomaly" was deliberately not included). adapter interpret_report + prompt instruction; adapter unit tests + agent_report_test.
- [x] **Run overrides / effective playbook (spec 11)** - RunOverrides{executors,nodes} in apb-core, applied in prepare_run, effective-playbook snapshot, `apb run --overrides <file>`; overrides in run.yaml. Tests overrides_test, overrides_run_test. Not done: under --supervise (explicit error), matrix runs.
- [x] **success_check (spec 6.2)** - a sh-script check of an agent-node's result; a nonzero exit code fails the node regardless of self-assessment. Schema + execute_node + V12 + node form in the web UI. Test success_check_test.
- [ ] **node_slow / run_stuck** CA wake triggers + `runs/index.jsonl` (duration history) - deferred, medium value.
- [ ] **Isolation enforcement** (per-node/branch worktree) on top of the 8e-2 declaration - deferred (spec 8.3).
