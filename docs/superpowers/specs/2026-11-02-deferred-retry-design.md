# Deferred retry of one node

Date: 2026-11-02. Status: accepted, built. Author-facing usage:
`docs/HOWTO-authoring.md` ("retry_delay"), operations: `docs/DEPLOYMENT.md`,
`docs/CI.md`, `docs/MCP.md`, `docs/NOTIFY.md`.

## 1. Goal

An `agent_task` that fails retries at once, up to `max_retries`. When the
failure is a dependency that needs minutes to come back (a deploy still
rolling out, a service restarting, a rate window), every retry fails the
same way and the budget is gone in seconds. Waiting by hand meant pausing
the whole run.

This change lets an author space the ordinary retries of one node. The node
waits on the run's active clock while the rest of the run goes on; a run
with nothing else to do frees its driver and is woken when the retry is
due; a person, an operator or a supervisor can start it at once.

The same change records the chain step and the binding of every agent
attempt, the apb version of every drive, and fixes two defects: a list item
no longer falls back to a host task, and the last notifications of a run
are retried after its driver ends.

## 2. Schema and validator

`retry_delay` is an optional block on an `agent_task` and on `defaults`;
the node's own block wins (`Playbook::effective_retry_delay`). Additive to
schema 2, no migration.

```yaml
defaults:
  retry_delay: { initial: 30s }
nodes:
  - id: deploy_check
    type: agent_task
    max_retries: 4
    retry_delay: { initial: 1m, factor: 2, max: 15m }
```

1. `initial`: a duration (`30s`, `5m`, `1h30m`, or plain seconds).
2. `factor`: the growth per further retry, default 2.
3. `max`: the cap of every delay; absent means the 7-day ceiling.

The block never applies to a list node (item retries are not delayed), and
`defaults.retry_delay` skips list nodes. `max_retries` stays the only
count: the delay only spaces gaps that `max_retries` already allows.

| Code | Severity | Rule |
|---|---|---|
| V126 | error | `initial` does not parse, is below 1 s or above 7 days |
| V127 | error | `factor` is not a finite number in `[1, 10]` |
| V128 | error | `max` does not parse, is above 7 days or below `initial` |
| V129 | error | a list node carries `retry_delay` |
| V130 | warning | the delay is never used: the node's effective `max_retries` is 0, or no node `defaults.retry_delay` applies to (an `agent_task` without its own `retry_delay`, not a list node) has `max_retries` above 0 |

## 3. What is delayed

Only the ordinary retry of an `agent_task` (the one that spends
`max_retries`). Unchanged and never delayed: infrastructure backoff (its own
budget, in process), the quota pause (run-level), the move to a profile
fallback or to the host step (the first attempt of a step never waits),
timeout continuation, a supervisor's `node_retry` and `continue_from`, a
re-visit through a loop edge, list item retries, the finish composer and
script nodes.

## 4. The delay and the clock

Owner: `apb_engine::scheduler::deferred`.

1. Retry `n` (1-based within its chain step) waits
   `min(initial * factor^(n-1), max)`. When the failure carries a
   `Retry-After`, the delay is the larger of the two, still capped by `max`.
   No jitter, so the delay is exact in tests.
2. The delay counts active run time from the `retry_deferred` event, on the
   paused-interval clock of `apb_engine::pause`: an operator, budget or quota
   pause freezes it; a released driver (section 6) does not, because the run
   is not paused.
3. `deferred::due` answers `Now` (delay passed or expedited), `At(ms)`
   (while the run stays active) or `Frozen { remaining_ms }` (the run is
   paused), read by every surface through `apb_engine::retry_wait`.

## 5. Deferral in the attempt loop and the drive

1. Where the attempt loop would start an ordinary retry of a node with an
   effective `retry_delay`, it returns `AttemptOutcome::Deferred` with the
   failed attempt's events followed by `retry_deferred { node,
   after_attempt, retry, step, binding, delay_ms, failure_kind? }`. The node
   reads `retry_waiting`.
2. The open deferrals are folded from the journal (`RunState::deferred`), so
   the live drive, a resume and a wake read the same state. A deferral
   closes on the node's next `attempt_started`, its `node_finished`, a
   revival of the node or `run_aborted`.
3. The drive never admits a node whose deferral is not due. The node stays
   in the frontier, so a downstream join counts it as in flight and a fork
   policy or a won race cancels it like any waiting branch. When the held
   node is current, the first runnable frontier node takes its place. With
   nothing else to run the drive sleeps in short ticks up to the earliest
   due time, the control scan running between them. Inside a parallel batch
   a waiting member is not in flight; the batch waits for the next result
   with a timeout and puts the member back in its queue once it is due.
4. A due deferral continues its execution: no new `node_started`, no cache
   lookup, the chain walk starts at the recorded step, the retry counter at
   the recorded retry, the display attempt number after the failed one, and
   the binding is the snapshot named by `retry_deferred.binding`, never the
   current rebind or tier overlay. A missing or damaged snapshot fails the
   node with that reason.
5. A resume with no interrupted work but an open deferral starts from the
   oldest one (`ResumeReason::DeferredRetry`) and holds it until it is due;
   a resume never skips the delay. `max_retries` is not exceeded across a
   crash and resume during a delay.
6. A run never finishes with a deferral open: before the run ends (the first
   finish reached, or a failure that ends it) every open deferral is written
   off as a cancelled `node_finished`.
7. A crash after the failed attempt's events and before `retry_deferred`
   resumes as it did before this change: the node runs again.

## 6. Releasing the driver and waking up

1. When the only work left is deferrals and the earliest is due more than
   60 s away (`APB_RELEASE_AFTER_MS` shortens it in a debug build, for
   tests; a release build ignores it), the drive journals `drive_released { wake_at_ms, nodes }` and returns with the run
   still `running`. It journals the release under the run's event lock, and
   only while nothing came in since its last look: a control its scan
   applies (a pause, a stop, a retry-now, a patch) or an expedited deferral
   keeps the drive going instead. It then frees the busy lock, spawns
   `apb __pause-waker --release <seq>` and runs the final notify sweep last.
   A shorter wait sleeps in process.
2. Only a top-level autonomous run releases. A child run (its parent drives
   it), an eval run and a supervised run (the supervisor is its waker) sleep
   in process.
3. `RunState.released` holds the latest `drive_released` until any later
   drive event (a node or attempt start, a pause, a resume, a stop, the end,
   a new deferral). Surfaces say "waiting to retry `<node>` at T; a waker
   resumes the run then" instead of "driver dead".
4. The release waker sleeps until `wake_at_ms` (at once when one of the
   released retries was expedited) and resumes the run only while that
   exact release still holds it, through the same launch check as the pause
   waker (`EngineResume::PauseWaker`); a busy tree makes it wait, not give
   up. It probes nothing. A pause, resume, stop or new release supersedes
   it. The driver it starts gets the release's seq (`--release-seq`) and
   journals `run_resumed` only while that release still holds the run,
   checked under the run's event lock, as a pause waker's driver does with
   `--pause-seq`: a pause a person journaled after the waker looked stands.
5. One waker per wait. Every waker (pause, release, notify) holds an OS
   lock on `runs/<id>/wakers/<pause-N | release-N | notify>` for its life;
   a second waker of the same wait ends at once, and a waker keeps its
   lease until the driver it started journaled `run_resumed` (or died), so
   a quota pause is probed once and no second driver races the first.
   Spawning lives in `crate::waker` with the lease.
6. A pause requested while the run is released is journaled at once, as for
   a run nothing drives, never posted to the driver that is on its way
   out.
7. `apb resume <run>` of a released run before its time starts a driver
   that recomputes the remaining delay and sleeps or releases again.
8. `apb resume --all` resumes a released run whose wake time passed (cause
   `retry_due`) and gives a new waker (`Rearmed { wake_at_ms }`) to a
   released run whose time is ahead and to a paused run whose `until` is
   ahead, since the waker they had may have died with the machine; it
   spawns none while a live waker holds the wait's lease, and leaves a due
   wait whose lease is held to that waker (`Busy`). Before this change such
   a pause was left as it was.
9. A foreground `apb run` or `apb resume` whose drive released the run
   prints the release line and exits 4, the code `apb wait` gives a
   released run.

## 7. Retry now

One check for every surface: `retry_wait::plan_retry_now` refuses a run
that ended (`run_finished`) or one with nothing waiting
(`no_deferred_retry`); otherwise it says whether a live driver holds the
run.

| Surface | Request |
|---|---|
| CLI | `apb resume <run> --retry-now [--node <id>]` (not with `--all`) |
| MCP | `run_resume { retry_now: true, node? }` (not with `from_node`; `node` needs `retry_now`) |
| HTTP | `POST /api/runs/{id}/resume { retry_now, node? }` (400 with `from_node`, or `node` without `retry_now`) |
| Dashboard | the "Retry now" button of the `retry_now` next action |
| Supervisor | `supervisor_node_retry` on a waiting node (a prompt override is refused) |

MCP and HTTP refuse in one order, with the same codes: the arguments first
(`retry_now_with_from_node`, `node_without_retry_now`), then the run gate
(the origin and trust check of a resume), then `plan_retry_now`, so a run
directory this machine did not create learns nothing about its retries.
The CLI makes the checks `apb resume` makes. On a run a live driver holds, `retry_expedited { node, by }` is journaled under
the run's event lock, the lock the drive journals its release under; that
is the whole action, and the driver starts the attempt at its next look;
the other resume arguments a caller sent are not applied and are named
(`not_applied` on MCP and HTTP, a note on the CLI's stderr). A
released, paused or driverless run (or one whose driver let go meanwhile)
is resumed through the ordinary resume path, which journals the
`retry_expedited` right before its `run_resumed`, under the same lock and
after every resume check passed, so a refused resume waives no delay.
`by` is `operator` (a person) only for the CLI at a terminal outside a run
step and for a dashboard with keys; it is `host` for MCP (a host may relay a
person or act on its own), for a run step (`APB_RUN_ID` or `APB_HOST_TASK`
set: a run step is never a person), for the CLI without a terminal and for
a dashboard without keys, the callers the launch policy also does not take
for a person; `supervisor` for
`supervisor_node_retry`, which journals directly (a read error is its
answer; when nothing was left to expedite it posts a plain retry); an
unknown value reads as absent. The detached driver refuses a
`--retry-now-by` that names nobody or is missing, and a retry-now or a budget change
next to the engine's hold of a pause or a release.

Next actions: `ActionCode::RetryNow` (principals person, operator,
supervisor) and `AutoOutcome::RetryAttempt`. Under a live driver the
deferral progresses by itself (`At`, assured). Paused, it is `Frozen` with
what is left. Released, the run is stopped with a not-assured resume at
`wake_at_ms`, like a pause with `until`: `apb wait` returns 4, and
`--follow-auto` keeps waiting until the wake time plus its grace for the
same release.

Run surfaces: `apb runs <id>` prints the release line and each waiting
node's line ("retry 2 at T", "frozen while paused, 1m 30s left", "due
now"); MCP `run_status` carries `retry_waiting`, `released` and
`released_note`; the HTTP run detail carries `retry_waits` and `released`;
the run page shows a badge on a waiting node (dashed while frozen) and a
"released" badge that carries the release line.

## 8. Journal events and older binaries

| Event | Fields | Checkpoint |
|---|---|---|
| `retry_deferred` | `node`, `after_attempt`, `retry`, `step`, `binding`, `delay_ms`, `failure_kind?` | no |
| `deferred_retry_guard` | `open` | no |
| `drive_released` | `wake_at_ms`, `nodes` | no |
| `retry_expedited` | `node`, `by?` | no |

Every field is `serde(default)`. New fields on known events:
`attempt_started.exec`, `run_provenance.apb`, `run_resumed.apb`.

The problem: 1.0.0-beta.1 and 0.25.0 skip an unknown event once a later
checkpoint exists. A sibling's `node_finished` after `retry_deferred` would
let such a binary fold the waiting node as interrupted and run it again at
once, with a fresh retry budget and a fresh chain.

The guard: while any deferral is open, no checkpoint is the last event of
the journal.

1. `EventLog` keeps the run's open deferrals, folded from the journal it
   opens and caught up from the bytes other writers appended before each
   append.
2. Every `node_finished` or `run_paused` appended while that set is not
   empty is followed by `deferred_retry_guard { open }`, both lines from
   one buffer in one write under the journal lock. The rule lives in the
   append itself, so no call site can forget it. `run_aborted` closes every
   deferral and needs no guard; `run_finished` never comes with one open.
3. To an older binary the guard is an unknown event after the last
   checkpoint, so its resume and every other writer refuse with its "written
   by a newer apb" message. Its read surfaces still list the run, the
   unknown records counted, and its stop still works, because stopping only
   appends `run_aborted` after a warning.
4. A process crash cannot split one `write(2)` of a small buffer on a
   regular file, so a crashed writer leaves neither line or both. A machine
   crash can leave a torn last line, which the strict read of every binary
   refuses, so nobody continues on half a pair; it can also cut the write
   exactly after the checkpoint's newline, leaving a whole journal with no
   guard. A writer of this binary that opens such a journal appends the
   missing guard first, before anything else, decided under the append
   lock (`EventLog::open`); `apb doctor --run` reports a checkpoint with a
   deferral open and no guard after it. A guard anywhere after the last
   checkpoint protects the deferrals, since an older binary refuses any
   unknown record after its last checkpoint. The window that stays: a
   machine crash cut there and an older binary opening the run before this
   binary does (`docs/DEPLOYMENT.md`, "Deferred retries and older apb
   binaries").
5. Once the last deferral closes, the next checkpoint carries no guard and
   an older binary resumes and finishes the run with the same routing.

A run an older binary starts with `retry_delay` simply retries at once; a
newer binary that resumes it applies the delay to later gaps only.

## 9. Binding snapshots and the chain step

Owner: `apb_engine::execution_identity`.

1. Before the attempts of an `agent_task` execution the engine writes the
   binding in force (the manifest entry, or the rebind or tier overlay over
   it) to `runs/<id>/bindings/<sha256>.json`: canonical JSON, private,
   atomic, written once per distinct binding.
2. Every agent attempt journals `attempt_started.exec { binding, step,
   source, executed_by }`: the snapshot digest, the step index (the host
   step is `chain.len()`), how the step was reached (`primary`,
   `fallback { index, of }`, `tier { name }`, `host_fallback`, `ephemeral`)
   and `cli` or `host`. The finish composer records none.
3. A later rebind writes another snapshot and never changes what earlier
   attempts, or a waiting retry, ran on. An older apb drops `exec` and
   never opens `bindings/`.

## 10. Binaries of a run and the drift report

1. `run_provenance.apb` and `run_resumed.apb` record the binary version of
   each drive. `RunBinaries` lists the drives in order; a drive with no
   version is "an older apb", never a guess. A downgrade is a drive by a
   lower version, or with no version after a recorded one.
2. The run detail on every surface shows "driven by apb X, resumed by Y"
   (`driven_by`).
3. `apb doctor --run <id>` adds drift rows built on `event::survey`, a
   reader that classifies every line and never fails: the binaries, any
   downgrade, unknown event types with counts and seqs, lines of a known
   type that do not decode (with their line number), a torn last line, and
   each open deferral with or without its guard. It only reads.

## 11. Defect fixes

1. **List items never fall back to a host task.** Host tasks are adopted by
   node, so items running side by side would supersede each other's tasks,
   the same reason host mode refuses list nodes at start. A list item whose
   CLI cannot start now fails with the CLI's error and writes no
   `execution_fallback`; the other items are unaffected.
2. **Retry of the last notifications.** The drive's last sweep is the last
   one a driver runs, so a refused send that was due a retry waited forever.
   When a send may be due again, the drive now leaves
   `apb __pause-waker --notify`, which sleeps the retry gap (30 s,
   `APB_NOTIFY_RETRY_GAP_MS` shortens it in a debug build, for tests) and
   sweeps the run once, again while a retry is ahead and no driver runs; each notification gets at
   most three tries. Due-ness is a
   pure function of the send ledger, so nothing is sent twice and a send
   the trust gate retired is never retried. No new journal state. A reboot
   loses the waker; `apb resume --all` sweeps a finished run once.

## 12. Tests

1. Delay arithmetic, the cap and `Retry-After`; V126 to V130.
2. A sibling runs while a node waits, in a batch whose sibling outlives
   the delay and in one whose sibling ends first, and with one slot; a
   join waits for it; the retry continues on the recorded step and binding
   across a pause, a resume and a rebind; a sibling's commit during the
   wait is never the waiting node's (`deferred_retry_test`).
3. Release, wake, supersession; `resume --all` resumes a due release and
   re-arms a future one; a pause shifts the retry by the paused interval; a
   stale release never resumes over a newer pause; a held lease keeps a
   second waker and `resume --all` off the wait; nothing asked in the
   release window is left to the exiting driver; the release is refused
   while a control or an expedite came in (`deferred_wake_test`, unit tests
   in `scheduler::deferred`, `crate::waker`).
4. Retry now: a live drive starts the delayed attempt early, a paused and
   a released run are resumed with the retry due, and a refused resume
   waives nothing (engine, real drives); the live path on CLI, MCP and
   HTTP, with MCP and HTTP refusing in the same order with the same codes
   and a host recorded as `host`; a released run through the real binary
   resumed by the CLI, by MCP (`by: host`) and by the dashboard
   (`by: operator`), each finishing the run (`apb run` exits 4 first); the
   detached driver's refusal of arguments it cannot honor; next actions
   and `apb wait` exit codes; the dashboard badge, the released line, the
   binaries line and the button with its launch-confirmation and
   irreversible-consent replays (vitest).
5. The guard on raw bytes, a fault injected inside the two-line write, no
   guard after the last deferral closes, a revival closing a deferral in
   the event log as in the run state, and the guard added on open after a
   write cut right after the checkpoint.
6. The journal corpus with 1.0.0-beta.1 and 0.25.0 (rows D1 to D8) covers
   the open, released, paused, torn and closed cases of section 8; row D9
   kills the new driver during a delay and resumes it with the new binary,
   which keeps `max_retries`; both defects were reproduced failing before
   their fix.
7. `apb doctor --run` escapes and clips journal text, counts undecodable
   lines past eight and reports an unguarded checkpoint.

## 13. Not covered

1. Delays for anything section 3 lists as unchanged; jitter.
2. Keeping the busy lock while a run is released (`--keep-lock` applies to
   pauses only).
3. Repairing a torn journal; the doctor names the torn line. (A whole
   journal whose guard line is missing gets its guard on open, see
   section 8.)
4. A waker that survives a reboot on its own; `apb resume --all` at login
   (`docs/DEPLOYMENT.md`) is the way back.
5. Reading the chain step back for other uses (attempt history views); the
   record is there, its readers come later.
