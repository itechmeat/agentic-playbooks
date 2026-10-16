# Operator pause and resume for runs

Date: 2026-10-15. Status: accepted. Issue: #200. Release: 0.25.0 (one PR).

## 1. Goal

An operator (CLI, MCP, dashboard) can pause any run and continue it later
without losing finished work. The engine uses the same mechanism to pause on
provider quota errors and to surface runs left without a driver. Paused time
never counts against deadlines, budgets or statistics.

The issue text (#200) is the requirement list; this document records the
decisions on the points the issue leaves open.

## 2. Journal and control

1. `Control::Pause` gains optional fields, all `#[serde(default)]`, so an old
   `{"cmd":"pause"}` line still parses: `reason`, `by`
   (`operator` | `supervisor` | `engine`), `mode` (`drain` | `now`, default
   `drain`), `until` (epoch ms, optional), `keep_lock` (bool).
2. `RunPaused` gains `#[serde(default)]` `by`, `mode`, `until`, `keep_lock`.
   `RunResumed` gains `#[serde(default)]` `by`. No new event type for the
   pause itself. Old journals decode with the new fields absent; a missing
   `by` on `RunPaused` reads as `supervisor` only in views, never rewritten.
3. `supervisor_run_pause` keeps working and journals `by: supervisor`.
4. `pausing` is a view overlay, not a `RunStatus` variant: a run whose
   journal status is `running`, with a pending `Pause` in `control.jsonl` and
   a live driver, reads `pausing` in every surface. The TS contract gets the
   overlay through the run summary's pause object, not through `RunStatus`.

## 3. Drain and now

1. `--drain` (default): the existing boundary scan admits no new node or
   batch chunk; running nodes finish; the run then journals `run_paused` and
   the driver exits.
2. `--now`: the stop watcher fires a pause interrupt that is separate from
   the Abort-only `cancel` signal. Scripts get SIGTERM to the process group,
   a grace period (default 10 s, env override for tests), then SIGKILL.
   Agents are killed the same way; their session id stays in the journal so
   resume continues the session where the node supports it. Only the first
   resume after that pause continues it: a `--from-node` resume
   (`run_resumed.explicit`) or a later pause and resume before the node
   ran again starts the node fresh. A `--now` pause during a retry backoff
   keeps the session of the attempt that just failed.
3. An attempt ended by a `--now` pause journals
   `AttemptFinished { status: "interrupted" }` and no `NodeFinished`. It
   spends no retry and no infra retry, and retro and stats do not count it
   as a failed attempt. `RunPaused` (a checkpoint) follows it, so the event
   stays skip-safe for older binaries.
4. Resume re-runs every interrupted node, including several parallel ones;
   `plan_resume` must not fall back to "rerun only from the last node" for a
   `--now` pause. Completed nodes never run again.

## 4. A paused run holds no driver; who resumes at `until`

1. The driver exits after journaling `run_paused`. Releasing the busy lock is
   the default.
2. When the pause has an `until` (operator `--until`, or a quota pause), the
   exiting driver spawns a detached waker: a small `apb` process that holds
   no lock and runs no node. It sleeps until `until`, then resumes the run
   only if the run is still paused by the same `run_paused` event (journal
   sequence match). An earlier `apb resume`, a new pause or a stop makes the
   waker exit without action.
3. A reboot kills the waker. `apb resume --all` (section 7) resumes paused
   runs whose `until` has passed, so a boot unit restores the schedule.
4. `--keep-lock`: the lock file records the paused run id instead of a pid.
   `acquire_tree` treats it as held by that run, the run's own resume takes
   it over, `apb stop` of the run releases it, and `apb doctor` lists it.

## 5. Frozen clocks

One pure helper in apb-engine computes the paused intervals of a run from
its journal (`run_paused.ts` to the next `run_resumed.ts`; an open pause
counts until now) and the active elapsed time between two instants. Every
clock below uses it:

1. Host-task deadlines: remaining time is the timeout minus the active
   elapsed time since the task opened; listed deadlines shift by the paused
   time. A pause never closes or expires an open task, and a reply submitted
   while paused is stored and applied on resume.
2. Wait-node timers and `timeout_seconds`, question timeouts.
3. The eval `DeadlineWatch`.
4. `expected_duration` comparisons (liveness, stall, progress).
5. Run and node durations in `apb stats`, `run_report` and the retro.
6. New template value `run.active_elapsed_seconds`, so a playbook computes
   its own budget without paused time (field report item 3).

## 6. Quota pause

1. New `FailureKind::Quota`. Patterns: HTTP 429, `too many requests`,
   `rate limit`, `FreeUsageLimitError`, `credit usage limit`. It takes
   precedence over `Transient`. `Budget` keeps its current patterns
   (`usage limit`, `insufficient_quota`, `quota exceeded`) and its current
   behaviour.
2. A `Retry-After` value (seconds or an HTTP date) is extracted from the
   failure detail when present.
3. Order: profile fallbacks first. Then, if `Retry-After` is longer than the
   next infra backoff step, pause right away; otherwise use the normal infra
   retries and pause when they are exhausted. The node never fails because
   of quota while pauses remain.
4. The pause journals `reason: "quota (<agent>/<model>)"`, `by: engine`,
   `mode: drain` (so batch siblings finish their work) and `until` from
   `Retry-After` or the schedule 15 min, 30 min, 1 h, 1 h, ... (env
   override for tests). The quota-failed attempt itself is `interrupted`,
   not failed, and spends no retry. `credit usage limit` and
   `FreeUsageLimitError` are matched before `Budget`, because the former
   contains Budget's `usage limit`.
5. Cap: after 6 consecutive quota pauses of the same node without a
   successful attempt, the node fails with a quota-exhausted reason.
6. On wake, the engine first sends a probe: one minimal invocation of the
   same agent and model with a fixed tiny prompt and no tools. A quota
   failure of the probe re-pauses at the next schedule step without touching
   the node. Script nodes need no probe and never quota-pause. Only the
   waker and `apb resume --all` probe; an operator's `apb resume` does not.
   The probe outcome is journaled as a `supervisor_action` marker
   (`quota_probe`), an existing event type.

## 7. Driverless runs and `apb resume --all`

1. `apb resume --all` scans the runs of the workspace and, skipping
   sub-playbook children (they follow their parent):
   1. resumes runs reported `running` whose driver is dead;
   2. resumes paused runs whose `until` has passed;
   3. leaves other paused runs alone.
2. A driverless run that the run gate refuses to resume is journaled as
   `run_paused { reason: "driverless", by: engine }`, so `apb runs` shows it
   instead of a false `running`.
3. `docs/DEPLOYMENT.md` documents an optional systemd user unit that runs
   `apb resume --all` at boot.

## 8. Surfaces

1. CLI: `apb pause <run_id> [--drain|--now] [--reason] [--until] [--keep-lock]`.
   `--until` takes RFC 3339 or a duration (`90m`, `2h`), parsed with the
   existing duration helper. Pausing a paused run updates reason and
   `until` (a new `run_paused`); pausing a terminal run is an error.
2. `apb resume <id>` accepts a paused run and says when it resumes before
   `until`. `apb resume --all` as in section 7.
3. `apb runs` keeps the status column parseable and appends a marker
   `paused (reason, by, since, until)`; `apb runs <id>` prints a pause
   block.
4. `apb wait`: a paused run returns exit 4 with reason, by, since and
   until, even when a review, question or host task is open; the open gates
   are listed in the output and stay open.
5. MCP: new operator tool `run_pause` (any run, no supervisor token,
   destructive hint, refused on the supervisor-role server like
   `run_stop`). `run_status`, `run_wait` and `runs_list` carry a `pause`
   object (`reason`, `by`, `mode`, `since`, `until`, `pausing`).
6. Dashboard: `POST /api/runs/{id}/pause` and `POST /api/runs/{id}/resume`.
   Resume goes through the same run gate and consent check as MCP; an
   irreversible run without consent opens the existing consent dialog. The
   run view shows Pause and Resume buttons and a pause badge; the run list
   shows the marker.

## 9. Out of scope

Per-branch pause and editing the playbook while paused, as in the issue.
