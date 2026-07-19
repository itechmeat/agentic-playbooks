# Supervised run reliability (field report fixes)

Date: 2026-07-20
Status: approved design, pending implementation plan
Depends on: 2026-07-08-workflows-cli-design.md, 2026-07-12-agent-profiles-design.md

## 1. Purpose and scope

A long supervised production run on apb 0.3.0 (a real two-repo feature shipped
through a 17-node playbook on a memory-constrained VPS, surviving several host
crashes) produced a field report with 13 findings. All were verified against
the current 0.6.0 code and every one is still present. This story fixes them
in one release, 0.7.0.

The findings fall into four groups:

- Authoring friction: validation errors reach MCP as bare codes; the howto
  documents a template namespace the engine rejects; the human_review schema
  and edge-condition types are undocumented; playbook_trial cannot carry a run
  instruction.
- Journal and resume correctness: attempt_started is journaled only at attempt
  completion, so a crash mid-attempt leaves no trace and resume rewinds to the
  last finished node and re-runs completed work; resume marks the run paused
  for its whole remaining life; supervisor notes are replayed and duplicated on
  every resume.
- Process model: MCP-started runs execute on threads inside the `apb mcp`
  process and die with it; run_resume blocks until the run ends; abort cannot
  interrupt an in-flight node; there is no stop command and no way to append a
  note without a live MCP session.
- Expressiveness and diagnostics: cycles are forbidden outright so fix loops
  must be unrolled by hand; run_status has no timestamps and no liveness;
  fallback retries the identical agent+model binding; doctor cannot examine a
  run.

Verified root causes (all anchors current as of 0.6.0):

- `execute_node` returns its events and the drive loop writes them only after
  the node completes (`scheduler/node.rs:230-236`, `scheduler.rs:1133-1147`),
  so `attempt_started`/`attempt_finished` carry the same timestamp and nothing
  is journaled at spawn.
- Resume rewinds to `RunState.last_node` (last `node_finished`) and re-executes
  it (`scheduler.rs:1477-1554`); a crash after `node_finished` re-runs a
  completed node, which is how one node ran three times in the field.
- `resume_inner` writes `RunPaused { reason: "resume from X" }` as a marker
  (`scheduler.rs:1526-1528`) and no later event folds the run back to running,
  so a resumed run reads `paused` forever while executing.
- `drive` resets `control_cursor` to `None` on entry (`scheduler.rs:553`) and
  re-applies every historical `ContextAppend` from `control.jsonl`, emitting
  duplicate `supervisor_action` events on each resume.
- MCP background and supervised runs are `std::thread::spawn` inside the MCP
  process (`scheduler.rs:389-419`); only the CLI `apb run --supervise` re-execs
  a separate driver process (`cli/run.rs:355-408`).
- Abort is consumed only at the between-nodes boundary; the cancel flag handed
  to a single-node execution is a fresh `AtomicBool` wired to nothing
  (`scheduler.rs:594-602, 1142`).
- The full structured validation data (`Issue { code, severity, message, node }`)
  already exists; `versioning.rs:793-809` collapses it to codes and the MCP
  layer formats those codes into the final string (`mcp/tools.rs:53-63`).
- Fallback advances the manifest chain position unconditionally
  (`scheduler/node.rs:197-205`) with no identity guard, so a chain of
  claude -> claude re-runs the identical binding.

Out of scope, with rationale (section 8): in-flight note delivery to a running
one-shot agent process, token/cost capture in events, automatic respawn of
lost attempts, intra-node progress estimation beyond attempt age, dashboard
features beyond rendering tolerance for the new events.

## 2. Decisions and their rationale

| Decision | Choice | Why |
|---|---|---|
| Release shape | One release, 0.7.0, all findings | Owner directive; the groups interlock (stop needs the detached driver's pid; liveness needs spawn-time journaling; resume detection needs both) |
| Validation errors | `VersioningError::Validation(Vec<Issue>)` carries the existing structured issues; MCP renders one line per issue: code, severity, node, message | The structure already exists and is already serialized by `playbook_validate`; only the create/update path discards it |
| V13 message | Appends the list of known namespaces to the unresolved-variable message | The field author bisected 18 probe playbooks to learn this; one static suffix removes the whole class |
| Template docs | HOWTO-authoring.md gets a Template variables section; the `{{outputs.plan}}` example is corrected to `{{nodes.plan.output}}`; validator starts scanning the `instruction` field of playbook nodes | The howto taught a namespace the validator rejects; the instruction field is currently a V13 blind spot |
| human_review docs | HOWTO-authoring.md documents `options` (required list of strings) and the three edge-condition types with a worked example | Discovered only via parse errors in the field |
| Spawn-time journaling | `attempt_started` is written at spawn through an event sink available inside node execution; it gains `pid` (`#[serde(default)]`); `attempt_finished` gains `duration_ms` (`#[serde(default)]`) | Restores the crash trace; pid enables liveness; duration enables calibration. The adapter exposes an on-spawn callback carrying the child pid |
| Resume semantics | If the journal shows an interrupted node (node_started without node_finished as its last state), resume restarts that node; otherwise resume continues from the successors of the last finished node instead of re-running it; `--from-node` overrides; several simultaneously interrupted nodes (parallel region) keep today's behavior | Eliminates re-running completed work, the report's most expensive failure; the parallel case is rare and re-running a batch is correct, only wasteful |
| Resume state event | New `run_resumed { from_node }` event replaces the `run_paused` marker; fold maps it to running | A resumed run must not read paused while executing; the old marker abused an unrelated event |
| Terminal runs | Resume of a failed or aborted run is allowed (and now tested); no-argument resume of a succeeded run is refused with a message naming `--from-node` | The field recovery (fix a token, resume from the pr node) becomes first-class; re-running a finished run by accident stays impossible |
| Control cursor | The last-applied control seq persists in the run dir (atomic write via fsutil); drive resumes from it | Kills note duplication and the stale-Pause replay in one change |
| Note without MCP | New CLI `apb note <run_id> <text>` posting `Control::ContextAppend` through the existing `post_supervisor_command` | The field workaround was hand-editing context.md; the engine function already exists, path-based, no session needed |
| Run instruction visibility | context.md gains a leading run-instruction section whenever the run has an instruction, so it reaches every node that renders `{{run.context}}` | A summarizing first node silently dropped explicit requirements downstream; the directive should survive lossy hand-offs by default |
| playbook_trial | Gains `instruction: Option<String>` threaded into the existing `RunOptions.instruction` | One-field change; trial becomes usable for instruction-driven playbooks |
| Fallback guard | Before executing a fallback step, the engine skips it when its agent+model binding is identical to the binding that just failed; if the whole tail is skipped the node fails as exhausted | A claude -> claude fallback with the same profile is a pointless paid retry; skipping is safe because the manifest chain is immutable |
| Bounded loops | Edges gain `max_traversals: Option<u32>` (schema-additive); V11 permits a cycle iff every cycle contains at least one bounded edge; a bounded edge whose count is exhausted is treated as non-matching so the alternative edge fires; traversals of bounded edges are journaled as `edge_traversed` events and folded into counts | The fix-loop shape (review -> fix -> review) had no workaround; counting via the journal keeps resume exact; treating exhaustion as non-match reuses the existing edge-selection semantics |
| Loops vs result cache | A node that already has a node_finished in the current run skips the result cache on re-execution | A cache hit inside a review loop would replay the pre-fix verdict and defeat the loop |
| Detached driver | MCP-started background, supervised, and resumed runs re-exec the `apb` binary as a separate driver process (the pattern `spawn_detached_supervised` already uses), which re-opens the run from `runs/<id>` and drives it; the MCP process only prepares (policy gate, manifest) and observes | Removes the whole crash-cascade class: the field run lost two attempts to MCP-session death, and a later orphaned tree completing by accident proved detached operation already works |
| Driver pid | Every driver (CLI or detached) writes `runs/<id>/driver.pid` atomically at start and removes it on clean exit; status and doctor cross-check `pid_alive` | One file gives run_status liveness, stop-of-dead-driver finalization, and doctor forensics |
| run_resume ack | The MCP `run_resume` returns immediately with `{ run_id, resumed_from, detached: true }` after spawning the driver | The blocking call outlived every MCP client ceiling in the field; the decision record (what resume chose to re-run) is what a supervisor needs |
| Foreground playbook_run | The blocking non-background `playbook_run` stays in-process and synchronous | Short runs and tests rely on it; the report's pain was resume and long runs, which now detach |
| Stop | New MCP tool `run_stop` and CLI `apb stop <run_id>`: post `Control::Abort`; a watcher thread inside every driver polls control and flips the in-flight cancel flag, which kills the agent process tree; a stop with no live driver finalizes the journal with `run_aborted` directly | Abort becomes real: it interrupts in-flight nodes instead of waiting for the boundary, and a dead driver no longer wedges the run in a running state |
| Liveness in status | run_status marks a running node whose journaled attempt pid is dead as `lost`, and carries per-node start timestamps and attempt age; run-level `driver_alive` from driver.pid | The 19-minute ps-forensics episode becomes a status read |
| doctor --run | `apb doctor --run <id>` cross-references journal state, attempt pids, driver.pid, the workdir lock, and unapplied control entries | The report asked for exactly this consistency check |
| supervisor_wait_event | Unchanged; MCP.md documents the `after_seq` cursor pattern and the existing `timeout_ms` argument | Long-poll semantics are fine once documented; clients control slicing already |
| Versioning | Workspace 0.7.0, release notes `apb 0.7.0: supervised run reliability` | The scope is a feature release, schema-additive throughout |

## 3. Authoring surface

### 3.1 Structured validation errors

`VersioningError::Validation` changes from `Vec<String>` to `Vec<Issue>`
(`Issue` moves nothing; versioning already lives in apb-core next to
validate.rs). Both collapse sites are updated. The MCP mapping renders:

```
validation failed:
- V13 error (node `translate`): template `{{outputs.plan}}` cannot be resolved; known namespaces: params.*, nodes.<id>.output, nodes.<id>.report, nodes.<id>.review_note, run.instruction, run.context, run.hooks.*
- V11 error: graph contains a cycle without a bounded edge (max_traversals)
```

One line per issue, in report order: code, severity, node when present,
message. The `playbook_validate` tool shape is unchanged. The CLI renderer in
`cli/run.rs:187-202` is already structured and stays.

The known-namespaces suffix is appended inside `check_templates` so every V13
carries it. `check_templates` additionally scans the `instruction` field of
playbook nodes (currently only agent_task and prompt prompts are scanned).

### 3.2 Howto additions

`docs/HOWTO-authoring.md` (served verbatim by `playbook_howto`):

- The sub-playbook example's `{{outputs.plan}}` becomes
  `{{nodes.plan.output}}`.
- New section Template variables: the exact accepted set with one-line
  descriptions (params.*, nodes.<id>.output / .report / .review_note,
  run.instruction, run.context, run.hooks.*), a note that anything else is
  rejected as V13 at save time.
- New section Human review and conditional edges: `options` is a required list
  of strings; the three condition types with their fields
  (`node_status { node, equals: success|failure }`,
  `review_status { equals: <option string> }`,
  `output_match { node, pattern }`); one worked example wiring a review gate.
- New section Bounded loops (section 6 of this spec): syntax and semantics of
  `max_traversals`, one fix-loop example.

### 3.3 playbook_trial instruction

`PlaybookTrialArgs` gains `instruction: Option<String>`; `playbook_trial` sets
`RunOptions.instruction`. The howto's trial paragraph mentions it.

## 4. Journal and resume correctness

### 4.1 Spawn-time attempt journaling

Node execution gets access to an event sink so lifecycle events are written
when they happen instead of being returned in a batch:

- The drive loop already owns `&mut EventLog`; for the sequential path the log
  (or a thin sink wrapper) is passed into `execute_node`. The parallel batch
  path wraps the log in a mutex; append is a single atomic line write, so
  contention is irrelevant.
- The adapter exposes an on-spawn callback invoked with the child pid
  immediately after a successful spawn. `execute_node` uses it to append
  `attempt_started { node, attempt, agent, soul_delivery, skills_mode, pid }`
  at spawn time. `pid: Option<u32>` is `#[serde(default)]`.
- `attempt_finished` is appended when the attempt returns and gains
  `duration_ms: Option<u64>` (`#[serde(default)]`), measured from spawn.
- All other events keep their current write points. The existing fold logic
  that maps an open attempt to interrupted (`state.rs:184-192`) now actually
  fires for real crashes, which is the point.

The web run views must tolerate the new fields and the two new event types of
this spec (`run_resumed`, `edge_traversed`); unknown-event tolerance is
verified by a test.

### 4.2 Resume semantics

`resume_inner` (no `--from-node`):

1. Fold the journal. If run_status is succeeded, refuse with an error naming
   `--from-node`.
2. Collect interrupted nodes: nodes whose last lifecycle event is
   node_started or attempt_started without a matching node_finished.
3. Exactly one interrupted node: restart from it (today's re-run semantics,
   applied to the node that was actually cut off).
4. Two or more (a parallel region was cut): keep today's behavior, restart
   from `last_node`.
5. None: continue from the successors of `last_node` without re-running it.
   Drive gains a start mode distinguishing re-run-this-node from
   advance-past-this-node; the advance mode seeds the frontier by evaluating
   `last_node`'s edges against the folded state (status and outputs are in
   `RunState` already).
6. Journal `run_resumed { from_node }` (new event; fold maps it to running)
   instead of the `run_paused` marker. Old journals with the marker fold as
   before; no migration.

The MCP `run_resume` response reports the decision: the chosen node and the
reason (interrupted restart, advance past finished, parallel fallback, or
explicit from_node).

### 4.3 Control cursor persistence

The last-applied control seq is persisted to `runs/<id>/control.cursor`
(atomic fsutil write) every time drive applies control entries; drive
initializes its cursor from the file. Applied ContextAppend notes are
therefore never re-applied on resume and a stale Pause or Abort consumed in a
previous drive does not fire again.

## 5. Process model

### 5.1 Detached driver

A hidden CLI entrypoint (same binary, following `__drive-supervised`)
re-opens a prepared run from `runs/<id>` (manifest, journal, control) and
drives it to completion. The MCP layer changes:

- `playbook_run` with `background: true`, `supervise: "self"` runs, and
  `run_resume` spawn the detached driver via the current executable, stdio
  nulled, and return immediately. The policy gate, permit verification, and
  manifest snapshot all still happen inside the MCP process before the spawn;
  the child only executes the already-prepared run (anti-TOCTOU posture
  unchanged: the child reads the manifest, never re-resolves live files).
- Foreground `playbook_run` (no background flag) stays synchronous in-process.
- The engine's in-process `run_background*` thread spawns remain for the
  library callers that want them (apb-server), but the MCP tools stop using
  them for anything that must survive the session.

Every driver, CLI or detached, writes `runs/<id>/driver.pid` (atomic, 0600)
when drive starts and removes it on clean exit. A dead pid in that file is the
signal for status, stop, and doctor.

### 5.2 Stop

- Engine: each drive spawns a watcher thread that polls `control.jsonl` about
  every 200 ms; on `Control::Abort` it sets the cancel flag that
  `run_cancellable` already honors, killing the in-flight agent process tree.
  The drive loop then observes the abort at the boundary as today and
  finalizes with `run_aborted`. The watcher stops with the drive.
- MCP: new tool `run_stop { run_id }`; CLI: `apb stop <run_id>`. Both post
  `Control::Abort`. When `driver.pid` is missing or dead and the folded state
  is non-terminal, stop finalizes the journal directly by appending
  `run_aborted` (taking the run's event lock), so a crashed driver cannot
  leave a permanently running run.
- `supervisor_run_abort` keeps its contract and now actually interrupts
  in-flight nodes through the same watcher.

### 5.3 Liveness in run_status

run_status adds, without breaking existing fields:

- per node: `started_ms` (last node_started timestamp) and, for running
  nodes, `attempt_age_ms` and `attempt_pid`;
- node status `lost` when the node is running, an attempt pid is journaled,
  and the pid is dead;
- run level: `driver_alive: bool|null` from driver.pid (null when no file,
  e.g. terminal runs).

`run_report`'s duration table is unchanged; it now also benefits from
`duration_ms` on attempts.

### 5.4 doctor --run

`apb doctor --run <id>` prints a per-run consistency report: folded run and
node statuses, open attempts with pid liveness, driver.pid liveness, workdir
lock holder and its liveness, control entries beyond the persisted cursor
(posted but unapplied), and duplicate supervisor_action counts. Read-only,
no repair actions in this release.

## 6. Bounded loops

Schema (additive, stays schema 2): edges gain `max_traversals: Option<u32>`.

Validation:

- V11 now rejects only cycles in which no edge carries `max_traversals`.
  A cycle fully covered by at least one bounded edge is legal. The V11
  message names the remedy.
- New V30 error: `max_traversals: 0` (must be >= 1). Bounded edges on
  non-cycle edges are legal and simply act as traversal caps.

Engine:

- Traversing a bounded edge journals `edge_traversed { from, to }` (only
  bounded edges, keeping the journal lean). `RunState` folds these into
  per-edge counts, so resume restores loop progress exactly.
- During edge selection, a bounded edge whose count has reached its cap is
  treated as non-matching: the alternative edge (typically the success path
  or an escalation edge to human_review) fires instead; if nothing matches
  the existing no-edge behavior applies unchanged.
- A node re-executed in the same run (it already has a node_finished) skips
  the node result cache, so loop iterations never replay a cached verdict.
  Outputs remain last-write-wins in run_status; the journal keeps the full
  history as today.

The howto documents the pattern with the canonical fix loop:

```yaml
edges:
  - { from: review, to: fix,    condition: { type: node_status, node: review, equals: failure }, max_traversals: 3 }
  - { from: fix,    to: review }
  - { from: review, to: qa,     condition: { type: node_status, node: review, equals: success } }
```

After three review failures the bounded edge stops matching and the run
takes whatever else is wired from review (or fails there, visibly).

The web visualizer must render cyclic graphs without breaking layout; a
rendering-tolerance test covers a playbook with a bounded loop.

## 7. Fallback sameness guard

In the executor-chain loop, before executing step `idx > 0`, the engine
compares the step's resolved agent+model binding with the binding of the step
that just failed; identical bindings are skipped: no attempt runs and no
fallback_triggered event is emitted for the skipped step, so the journal
simply shows the chain moving past it. If skipping exhausts the chain the
node fails as exhausted, as when the chain ends today. Distinguishing environment-caused failures (like the field
run's token-permission error) from agent failures stays out of scope; the
sameness guard removes the observed waste without new failure taxonomy.

## 8. Out of scope, follow-up candidates

- Delivery of supervisor notes to an in-flight attempt: one-shot CLI agents
  have no input channel mid-run. The practical replacement shipped here is
  stop + note + resume, which interrupts the burn instead of watching it.
  A true channel needs agent-side support (ACP transport story).
- Token/cost fields on attempts: adapters do not parse usage today; the
  hermes usage-file follow-up is the natural vehicle.
- Automatic respawn of lost attempts: surfacing (`lost`, doctor) ships now;
  auto-respawn policy belongs to the supervisor, not the engine.
- Intra-node progress estimation: attempt age plus expected_duration in
  run_status is deliberately the stopping point.
- run_events history views in the dashboard beyond tolerance for the new
  events.
- Environment-vs-agent failure taxonomy for fallback decisions.

## 9. Testing

- Unit, apb-core: structured Validation error carries issues; V13 suffix;
  playbook-node instruction scanning; V11 cycle acceptance with bounded
  edge and rejection without; V30; schema round-trip of max_traversals.
- Integration, apb-engine (stub-agent scripts, tempdirs, existing suite
  binaries): spawn-time attempt_started with pid and same-run crash
  simulation (kill the stub, fold shows interrupted); duration_ms present;
  resume case matrix (interrupted node restart, advance-past-finished,
  parallel fallback to last_node, succeeded-run refusal, from_node
  override); run_resumed folds to running; control cursor prevents note
  replay across two drives; bounded loop executes exactly max_traversals
  iterations then takes the alternative edge, counts survive resume, cache
  is bypassed on re-execution; fallback sameness skip; stop kills an
  in-flight stub attempt (watcher wiring) and finalizes a dead-driver run.
- Integration, apb-mcp: create/update error text contains code, node, and
  message lines; run_resume returns the ack shape with the decision;
  run_stop tool; trial instruction reaches {{run.instruction}}.
- Integration, apb-cli: apb note posts a note a subsequent drive applies
  exactly once; apb stop; doctor --run output on a healthy and a wedged
  fixture run; detached driver survives parent exit (spawn via the hidden
  subcommand, parent exits, run completes - PID-file based assertion).
- Web: run view renders a journal containing run_resumed, edge_traversed,
  pid, duration_ms; graph view lays out a cyclic playbook.
- Live smoke: none needed; no external service is involved.

## 10. Docs and release

- docs/HOWTO-authoring.md: sections per 3.2 and 6.
- docs/MCP.md: run_stop, run_resume ack shape, detached lifecycle,
  wait_event cursor pattern, trial instruction.
- README.md command table: apb note, apb stop, doctor --run.
- Workspace version 0.7.0; docs/release-notes/v0.7.0.md titled
  `apb 0.7.0: supervised run reliability`.
