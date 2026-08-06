# Run resilience and systemic parallelism design

Design for GitHub issues #75 (parallel fan-out and join barriers in every run
mode, P0), #71 (survive agent session interruptions and self-recover), and #74
(supervised-run findings: tail-failure discards finished work,
identical-executor fallback, host-hook leakage, verdict-blind edge routing,
config-only finish report). One PR implements all three; this document records
the design decisions and their rationale. Code anchors reference `main` at
`b0cc97e` (they were verified drift-free against the issue texts).

Both #71 and #75 carried open design questions. The decisions below were made
by the implementing agent under an explicit owner instruction to proceed; each
decision states its alternative and why it lost, so the owner can overturn any
of them at review time.

## Part 1: Parallelism and joins (#75)

### 1.1 Decision: join readiness gains a liveness notion, implicit wait-for-all for acyclic fan-in

A node with more than one incoming edge behaves as follows:

- If at least one incoming edge carries `join:` the node is an explicit join,
  exactly as today (`parallel.rs:157-160`), with `all` and `any` modes.
- Otherwise, if every incoming edge originates outside the node's own strongly
  connected component (the fan-in is acyclic), the node becomes an implicit
  `all` join: it waits for its inputs.
- Otherwise (the node is a cycle merge point, e.g. `check -> tick -> check`),
  first-arrival semantics stay exactly as today, preserving the loop-deadlock
  rationale documented at `parallel.rs:153-156` and the behavior pinned by
  `loop_edges_test.rs`.

The SCC partition needed for "acyclic fan-in" already exists: `check_cycles`
(`validate/graph.rs:245-346`) runs an iterative Tarjan and discards the result.
The SCC helper moves into a reusable location so both the validator and the
engine share one implementation.

To make wait-for-all safe under conditional routing, join readiness (for both
implicit and explicit `all` joins) treats an input source as satisfied when it
is either terminal (as today) or dead: no longer reachable from the set of
active nodes (current, frontier, running batch members) in the residual graph.
Today a source that will never run stays `Pending` forever, so an `all` join
after a conditional fork would deadlock; the deadness rule makes the intuitive
semantics ("wait for everything that can still arrive") hold for both explicit
and implicit joins. Alternative considered and rejected: mandatory explicit
`join:` plus a validator error for barrier-less fan-in. Rejected because it
breaks every existing playbook with a diamond topology and pushes the burden
onto authors for what has one intuitive meaning.

### 1.2 Decision: `join` values are validated, not migrated

`Edge.join` stays `Option<String>` on the wire (no schema 3, no migration, run
snapshots keep loading). A new validation rule (next free code, V36, in
`validate/graph.rs`) rejects values other than `all` or `any` as an Error, so a
typo can no longer silently mean `all` (`parallel.rs:19-24`). Additionally V36
warns when incoming edges of one node mix `all` and `any` (today the first in
YAML order wins silently, `parallel.rs:163-169`). Engine-side parsing stays
lenient as a last resort for pre-existing stored snapshots, now documented.

Alternative rejected: strict serde enum. It would turn every legacy stored
playbook or run snapshot with a malformed value into a parse failure at load
time, which is a worse failure mode than a validation error at save/run time.

### 1.3 Decision: the concurrent batch runs in every mode, with a cap

The `mode == RunMode::Autonomous` gate at `scheduler.rs:572` is removed:
supervised runs batch ready non-interactive `agent_task`/`script` nodes exactly
like autonomous runs. Supervision semantics at batch boundaries:

- All batch members run to completion; failures are then handled sequentially
  by the existing batch-order failure scan (`scheduler.rs:729-738`) and, in
  supervised mode, `park_for_supervisor` fires after the batch, per failed
  node, exactly as it fires after a sequential node today.
- Per-branch live parking (a supervisor intervening in branch A while branch B
  keeps running) is out of scope for this PR; the drive loop still owns the
  run between batches. This keeps `pending_question`/`pending_review`/
  `pending_supervisor` truthfully singular (`progress.rs:259-278`): at most one
  wake is presented at a time, in deterministic batch order. Documented as the
  supervised concurrency contract.

A concurrency cap `max_parallel` is added following the existing precedence
convention (`node.rs:240`): `Playbook.defaults.max_parallel` (authoring) ->
`RunOptions.max_parallel` (invocation) -> `RunConfig.max_parallel` (persisted,
survives detached re-drive) -> engine default of 4. The batch executes at most
`max_parallel` members concurrently (chunked admission over the existing
`thread::scope`); N ready nodes no longer spawn N unbounded OS threads
(`scheduler.rs:627`).

### 1.4 Decision: batch path reaches parity with the sequential path

The cache flow (`cache::prepare`/`lookup`/`restore_artifacts`/`admit`,
currently sequential-only at `scheduler.rs:1393-1543`) and declared-artifact
capture (currently hardcoded `artifacts: Vec::new()` at `scheduler.rs:671-679`)
are factored into a per-node unit used by both arms. A batch member that hits
the cache is not spawned at all. `context.md` keeps its one-rebuild-per-batch
cadence (`scheduler.rs:707`), which is already atomic and race-free because
only the drive thread calls it.

### 1.5 Decision: missing template inputs become observable, not fatal

`{{nodes.<id>.output}}` for a node with no successful `NodeFinished` record
still renders as an empty string (changing the rendered text would shift
agent-task cache keys, `scheduler.rs:1408-1416`), but the engine journals an
explicit `missing_input` warning event naming the reading node and the missing
reference at render time. The dashboard and `run_events` surface it. Statically,
a new template rule (warning) flags a `nodes.<id>.*` read where `<id>` is not
guaranteed to have executed before the reading node, reusing the existing
happens-before machinery from V10 (`reachable_from`, `graph.rs:178-192`) plus
join-barrier awareness. With implicit joins from 1.1, the common diamond case
actually waits, so the warning marks genuinely racy reads.

Alternative rejected: failing the node on a missing input. Playbooks with
either-or conditional merges legitimately reference both branches; hard
failure would break them.

### 1.6 Decision: targeted interrupts

`Control::Interrupt` gains `node: Option<String>` with `#[serde(default)]` (a
compatible `control.jsonl` wire change). `observe_control`
(`scheduler/node.rs:122-151`) skips entries naming a different node. The MCP
tool `supervisor_interrupt_attempt` gains an optional `node` argument; absent
means broadcast, preserving today's documented behavior
(`server/supervisor.rs:143`).

### 1.7 Explicitly out of scope for this PR (documented, not silently dropped)

- Per-branch supervisor parking and plural `pending_*` arrays (engine
  `ProgressSummary` shape change rippling into MCP, axum routes, and the
  dashboard). The batch-boundary contract in 1.3 makes the singular fields
  truthful.
- Per-branch workdir isolation. Concurrent branches already share the repo
  root on `main`'s autonomous path; the existing opt-in `isolation:` field is
  the tool for branches that mutate files. A docs note is added.
- Visualizer rendering of join semantics (greenfield, tracked separately).

## Part 2: Resilience (#71) and supervised-run findings (#74)

### 2.1 Decision: the status-file verdict outlives the process exit (#74 finding 1)

Today the engine reads `APB_STATUS_FILE` only on the adapter's `Ok` branch
(`scheduler/node.rs:817-832`), so a non-zero exit, signal, or timeout discards
a verdict the agent already wrote. Change: on an adapter `Err` the engine also
reads the status file; a valid `{"status":"success"}` (with its `outputs`)
makes the attempt succeed, and the engine journals the tail failure as an
anomaly note (new `WakeTrigger::Anomaly` detail naming the exit) instead of
throwing the deliverable away. A `{"status":"failure"}` file on the `Err`
branch keeps the attempt failed but preserves the agent's own outputs as the
attempt output instead of the raw CLI error text. Rationale: the status file
is the explicit completion signal; the process exit code is transport-level
noise once that signal exists.

### 2.2 Decision: opt-in required verdict classifies cut-off attempts as interrupted (#71 item 1)

`AgentTask` gains `require_verdict: bool` (`#[serde(default)]`, mirrored in
`Defaults` following the `max_retries` pattern, `node.rs:240`). When it is in
force for a node:

- the status-file prompt contract (`STATUS_FILE_NOTE`) is always appended, not
  only under `success_check` (`node.rs:367-374`);
- an attempt whose process exits (any code) without writing a valid status
  file is classified interrupted: `AttemptFinished { status: "interrupted" }`,
  the partial output preserved on the event, a retry consumed, and the next
  attempt's prompt carries an interruption note ("a previous attempt was cut
  off mid-work; check for work already done - commits, worktrees, files -
  before redoing it"), which also covers #71 item 3 (self-recovery with
  context) and item 5 (the fresh attempt is told where to look).

Default stays off: turning every exit-0-without-verdict into a failure would
break every existing playbook that relies on the documented text-report
default (`interpret_report`, `adapter.rs:889-918`). Playbooks that orchestrate
long work opt in per node or via `defaults`.

Two boundary rulings recorded after the Task 5 review:

- A supervisor-issued interrupt is a control decision, not transport noise: a
  written success verdict does not override it. The interrupted attempt stays
  failed (the pre-existing `supervisor_interrupt_attempt` contract), and the
  journaled anomaly carries the verdict so the supervisor can accept the work
  explicitly if it chooses.
  Shipped nuance, recorded after the whole-branch review: "stays failed" is the
  label when there WAS a verdict to overrule. On a `require_verdict` node with no
  status file written, the no-verdict arm runs regardless of the interrupt, so the
  attempt is journaled `interrupted` and the next attempt carries the interruption
  note. The control decision itself is unchanged either way (no `failure_kind`,
  ordinary retry/fallback/patch proceeds); only the label differs, and it differs
  because the node's own contract was not met.
- For `ErrorClass::Timeout | Transport` with `require_verdict` in force, the
  attempt is labeled interrupted but keeps the pre-existing break-to-fallback
  behavior (no same-executor retry consumed in Task 5). The retry semantics
  for infrastructure failures belong to the classification work in 2.3, which
  must give this shape the bounded Transient retry treatment.

### 2.3 Decision: infrastructure failure classification with bounded backoff (#71 item 2, #74 finding 2)

A new curated classifier (a pattern table module in `apb-engine`, same spirit
as `apb-core`'s `models_table`) inspects the failure detail of a process-exit
or transport error and yields a `FailureKind`:

- `Transient` (network drop/reset/DNS, HTTP 5xx, 429/overloaded, stream
  aborted): the attempt is retried on the same executor with bounded backoff
  (default two extra infrastructure retries, 5 s then 30 s), consuming an
  infrastructure budget, not the node's `max_retries`. The backoff sleep is a
  short-tick poll loop so cancel/abort/interrupt still land promptly (there is
  no backoff at all today, `node.rs:489-992`).
- `Auth` (401/unauthorized/expired token/re-login) and `Budget` (spend limit,
  quota, billing): non-transient. The attempt fails immediately, remaining
  retries on this step are skipped, and fallback steps resolving to the same
  agent are suppressed (a different agent may still work, so cross-agent
  fallback stays allowed). This is the real gap behind #74 finding 2: an
  `(agent, model)` sameness guard already exists (`node.rs:421-443`, since
  0.8.0), but nothing classifies non-transient exits.
- `Agent` (everything else): today's behavior, unchanged.

`AttemptFinished` gains `failure_kind: Option<String>` and `FallbackTriggered`
gains `from_model`/`to_model` (all `#[serde(default)]`), so the journal can
finally show that a claude-to-claude fallback changed the model (#74 finding
2's observability gap, `event.rs:123-130`).

### 2.4 Decision: dead open attempts are reaped when a drive starts (#71 item 4)

`lost_nodes` (`liveness.rs:562-579`) stays report-only while a drive is
running (a live drive waits on its child and cannot miss its exit). The gap is
a run whose driver died: on the next drive entry (fresh drive or resume), open
attempts whose pid is dead are journaled closed as interrupted and the node is
scheduled for a fresh attempt, instead of requiring a manual salvage rerun.
Fully autonomous reaping of a driverless run (an external watchdog) is out of
scope and documented as such.

### 2.5 Decision: structured verdict routing without new event plumbing (#74 finding 5)

The status file's `outputs` object already overwrites the node output as
compact JSON and flows into `state.outputs` (`node.rs:827-832`,
`status_file_test.rs:158-186`). The missing piece is a condition that can read
it: a new `EdgeCondition::OutputField { node, field, equals }` parses the
source node's output as JSON and compares one top-level field as a string.
Authors route on a verdict by having the agent write
`{"status":"success","outputs":{"verdict":"failed", ...}}` and the edge
declare, in the internally tagged form the sibling conditions already use,
`condition: { type: output_field, node: verify, field: verdict, equals: failed }`
(the nested `output_field: { ... }` form written in the first draft of this
decision was never shipped).
A validator extension covers the new variant (source-node existence and
happens-before, beside V09/V10). Alternative rejected: a parallel
`verdicts` map folded from new event fields - more wire surface for the same
capability the status file already provides.

### 2.6 Decision: declared deliverables are checked, capture decouples from cache (#74 finding 4)

`capture_artifacts` (`cache.rs:334-383`) runs today only for cache-enabled
succeeded nodes. It moves to a per-node step that runs on every successful
attempt of a node declaring `outputs.files`; a declaration whose globs match
zero files journals an explicit `deliverable_missing` warning event (not a
failure: prompt-driven drift should be visible, but hard-failing on a glob is
too brittle). This also gives the batch path artifact parity (1.4). Full
cross-node deliverable ownership enforcement is out of scope.

### 2.7 Decision: finish-with-prompt always sees the run context (#74 finding 6)

Root cause confirmed: the terminal context reaches the finish composer only
through the `{{run.context}}` placeholder (`node.rs:1064-1097`); a finish
prompt without it gets zero upstream output while `FINISH_ANSWER_DELIVERABLE`
still says "summarize the recorded run context above". Fix: when the finish
prompt references neither `{{run.context}}` nor any `{{nodes.*}}` field,
`assemble_finish_answer_prompt` appends the terminal context section before
the deliverable instruction. Prompts that do reference context explicitly keep
today's exact assembly.

### 2.8 Decision: host-hook leakage stays a documentation and authoring concern (#74 finding 3)

The hermetic profile field from PR #69 already suppresses user-scope hooks and
plugins for claude executors (settings-flags mechanism,
`adapter.rs:347-375`); `outputs.extract` already protects the node output from
appended hook turns. No new engine mechanism in this PR. The deliverable is
documentation: PROFILES.md gains explicit guidance that production profiles
should set `hermetic: true`, and HOWTO-authoring gains the `outputs.extract`
recommendation for output hygiene, plus the #71 item 5 guidance for
orchestration-heavy nodes (commit early and often in worktrees).

## Part 3: Compatibility and gates

- All new event payload fields are `#[serde(default)]` (CLAUDE.md rule); the
  `control.jsonl` change (1.6) is additive; no schema migration is required
  (1.2); response shapes of `run_status` stay backward compatible.
- Development is TDD per `docs/TESTING-GUIDELINES.md` (single suite binary,
  shared env lock, bounded waits, stub agents via `APB_AGENT_CMD`); build and
  test scoping per `docs/BUILD-OPTIMIZATION.md`; gates per CLAUDE.md
  (fmt, clippy `-D warnings`, code-ranker, `cargo clippy --release`, full
  workspace test + nextest before PR).
