# Proposal: wait park

## Why

A long timer wait holds a driver process for its whole length. The wait
branch of the drive loop sleeps `AWAIT_CONTROL_POLL` and looks again
(`crates/apb-engine/src/scheduler.rs` lines 2643 to 2704 on main
e76aa42e), so a 6 hour wait keeps a driver process, its `driver.pid` and
the working-tree lock alive for 6 hours, and a machine that sleeps or
reboots in between leaves the run reading "driver dead".

An earlier attempt (`typed-inbox-and-released-waits`, its half D3
"Released timer waits") released the driver the way a deferred retry
does, with new release machinery. Five review rounds kept finding crash
windows in it. Revision 1 of this change parked the wait on the engine
pause, but still added its own hand-over, lock writes, a waker that
applied pending stops and a lock rule for a person's pause; two design
reviews found crash windows in each of those additions and nowhere else.

Revision 2 cuts them. A drive that would sit in a long timer wait posts
the engine's own pause, exactly as the quota pause and the budget gate
post theirs today: `Control::Pause` with `by: engine`, the reason
`wait (<node>)`, `until` at the wake time and `keep_lock` for a write
run. The existing drain path applies it: the control scan journals
`run_paused`, the driver exits, `after_pause` parks the lock (the code
an operator's `--keep-lock` pause runs) and spawns the pause waker.
Every pause, resume, stop and crash afterwards is main's behaviour for an
operator pause with `--keep-lock` and `--until`.

Revision 3 answers design review 3, which found that the drive's own post
can put the park in front of a person's later pause or stop, or behind a
patch that changed the run. It makes two targeted additions to existing
paths: the control scan applies a park only as the last pending entry of
its pass, with nothing else applied in that pass, and discards it
otherwise; and the pause waker of a park that finds a control pending at
its start resumes the run at once.

Revision 4 answers design review 4. Main's `post_control` counts the
control file and appends without a lock, so two writers can take the
same sequence number and one entry can be hidden from the scan; this is
a defect of main that the park exposes, and `post_control` now counts
and appends under one lock for every writer of this apb and later. The
park's waker looks for a pending control on every poll, not only at its
start. Every exit of a
scan pass, a patch that paused the run among them, discards a park right
behind it, and a park is applied only at the node it names. A park
therefore never outlives another control and never lands on a changed
run; an entry posted while the run is parked reaches the run within one
waker poll (5 s by default) while the waker lives.

Revision 5 answers design review 5. The park's waker no longer ends on a
control line it cannot read yet; it reads again at its next poll. A park
and the waker's resume of it are not counted as interventions. Two
limits are stated instead of closed, because they are contracts of main
for every feature: an apb older than this change that stops a parked
run and is then used to resume it counts the park as paused time, and
an older apb posting a control to a run a newer drive is driving is not
ordered by the new lock.

Revision 6 moves in, whole, the reminders for open waits that the change
before-running-alone held as its D4, with the decisions of that change's
review rounds 1 and 2 (design D9). On main a wait notifies once: every
transition has one send label
(`crates/apb-engine/src/notify/transitions.rs` `TransitionId::label`,
line 57) and `docs/NOTIFY.md` line 71 says "The same wait never sends a
second message", so a gate nobody answers stays silent for days.

Revision 7 answers design review 6. A reminder whose first try failed is
tried again while it is the latest due one; two sweeps that race record
one intent for a reminder on every path; a reminder goes only to the
target the request's notice reached; reminders are best effort across
binary versions, with no run guard; and a park still pending when a
person's pause was journaled directly is discarded once its wake time no
longer matches the wait's due time on the current clock.

| Decision | Code example | Non-code example |
|---|---|---|
| D9 reminders for open waits | a review of a dependency bump nobody answered gets a reminder after 4 h of active run time | a mail approval gate reminds after 1 h, 4 h and 24 h, and never after the decision |
| D1 a long timer wait posts an engine pause | a 2 hour timer before a canary re-check pauses the run with `until` at the due time; the tree stays locked, no driver and no agent runs, the pause waker resumes the run when the timer is due | a 3 day cool-off wait before a follow-up mail holds no driver; the machine reboots and `apb resume --all` resumes the run when it is due |

## What changes

1. A timer wait in a run that may release (`deferred::may_release`: top
   level, no eval, not supervised, not unattended, more than 60 s left)
   and carries the `wait_park` run guard, with no other frontier node, no
   open deferred retry and no control entry pending, posts
   `Control::Pause { reason: "wait (<node>)", by: engine, mode: drain,
   until: <wake time>, keep_lock: <write run> }` through the existing
   `post_engine_pause` and goes to the loop top, where the control scan
   applies it (capability `wait-park`). No new event, field, lock
   function or waker body.
2. `PauseCause::Wait`, derived from the reason prefix `wait (` of an
   engine pause, never journaled.
3. The paused-interval clock leaves a park out: the wait's own clock, a
   deferral's due time, durations, estimates and statistics read the park
   as active time, as they read a live wait today. The run deadline still
   counts it. The intervention counts of `apb stats` and the retro skip a
   park and the engine's resume of it, as they see nothing of a live
   wait; a person's resume of a park counts as the person's.
4. A run guard feature `wait_park`: a definition with a timer wait is
   guarded from its start (the existing `run_guard`), so an apb that does
   not know the park refuses to resume or pause such a run while the park
   is its open state, and still reads and stops it. The limit is the
   guard contract's: after that apb stopped the run, a person's explicit
   resume with it counts the park as paused time.
5. Surfaces: `apb runs`, `apb wait`, MCP `run_status` and `run_wait`, the
   dashboard, next actions and the HTML report show "waiting on timer
   `<node>` until <time>"; the notify sweep sends no "paused" notice for a
   park; next actions follow it to `until` and say when a wake is
   overdue; unattended next actions read it as resuming at `until`.
6. A resume before `until` re-enters the wait, which posts a new park for
   the time left; at or after `until` the wait ends as due. Retry-now is
   refused `no_deferred_retry`, as for every run without a deferral.
7. A park yields: the control scan applies it only when it is the last
   pending entry of its pass and nothing else was applied in the pass,
   decided again under the run's event lock, where its wake time must
   still equal the wait's due time computed again on the current clock
   (a pause journaled directly while the park was pending makes it
   stale); otherwise it discards the
   park (the cursor moves past it, nothing is journaled) and goes on, so
   a person's pause or stop applies as without the park and a patch,
   rebind or note is never followed by a stale park. The stop watcher
   does not halt work for a park, and no surface reports a pending park
   as `pausing`. Every exit of a pass discards a park right behind it,
   and a park naming another node is discarded. The waker of an applied
   park looks for a pending control entry on every poll and on the first
   resumes the run once, so an entry posted while the driver exits or
   while the run is parked is applied within one poll instead of at the
   timer. A control line the waker cannot read yet (a writer in the
   middle of its append) or an I/O error never ends the waker; it reads
   again at its next poll.
8. A fix of main: `post_control` takes the entry's number and appends
   it under one lock, `control.jsonl.lock` through the existing
   `fsutil::hold_lock`, for every writer of this apb and later. Two
   posts at once get two numbers in append order; a pause posted beside
   a note or a park is no longer hidden. An older apb posting to a live
   run of this apb is not ordered by the lock; two binaries of different
   versions on one live run at once stays unsupported, as on main.

9. Reminders for open waits (capability `wait-reminders`, design D9):
   `policy.yaml` `notify.remind: [1h, 4h, 24h]` (machine file; a project
   may only turn it off with `[]`; off by default) sends one reminder per
   entry for an open review, question or host task covered by the run's
   subscription, each its own send-ledger label
   `notify:<kind>.<seq>.r<n>`, measured on the active run clock, never
   before the notice went out, and only the latest due one after an
   outage. Every reminder intent, a `not_sent` one included, is appended
   under the journal lock only while its wait is still open, its
   notice's target is still in force and its own label is still
   admitted, so a reminder is never sent or recorded for a wait that
   closed before its intent, and two sweeps record one intent; one
   admitted while open may complete. A failed reminder is tried again
   while it is the latest due one. A reminder goes only to the target
   the request's notice reached: after a target change an open request
   gets none, and the next request is noticed and reminded at the new
   target. A re-asked request starts its own reminders. A park is not a
   wait on a person and is never reminded. One additive field,
   `notify_set.remind_ms`; no run guard, so reminders are best effort
   across binary versions: an older apb driving the run sends notices
   only.

## Code that differs from the research

1. The research proposed that next actions offer a pause and a stop and
   no resume. Next actions have no pause or stop action code
   (`crates/apb-engine/src/next_action/types.rs` line 30); a resume of a
   park is harmless (the drive parks again). The park keeps the resume
   action every paused run with `until` has, with the wait line and
   `auto` at `until`.
2. The research said an older apb only waits longer, so no run guard.
   It counts the park as paused time and waits the whole timer again,
   which under a run deadline can end the run aborted where this apb goes
   on. The run carries a guard (design D4).

## Out of scope

The numbered items below are the park's; for the reminders: a reminder
cadence per run or per playbook (the cadence is the person's), and quiet
hours (reminders follow active run time, so a person who pauses a run
silences them; a time-of-day window is a later notify setting).

1. Webhook waits keep their driver: a signal is read under a live driver
   (`scheduler.rs` line 2681).
2. Inbox waits (`evals-and-inbox-waits`): they post the same request
   with their own wake time.
3. A rule's unattended run (`start-rules-and-schedules`): `may_release`
   refuses it.
4. Moving the deferred-retry release onto the park. `drive_released`
   stays as it is.
5. Any change to the limits every pause has on main (design "Unchanged
   from main"): a waker that failed to spawn, a resume the gate refuses,
   the abort replay rule, a person's pause without `--keep-lock` freeing
   the tree. A stop posted in the driver's exit window is handled for a
   park only (design D7); for every other pause it stays as on main.

## Impact

1. Crates: `apb-core` (`validate/guarded.rs`), `apb-engine` (new
   `scheduler/wait_park.rs`; `scheduler.rs`, `pause/cause.rs`,
   `pause/kind.rs`, `pause/clock.rs`, `pause/display.rs`,
   `pause/waker.rs`, `scheduler/control_apply.rs`, `stop.rs`,
   `control.rs`, `run_stats/interventions.rs`,
   `notify/transitions.rs`, `next_action/verdict.rs`,
   `next_action/unattended.rs`, `report_html/model.rs`,
   `report_html/sections.rs`), `apb-mcp` (`tools/run.rs`), `apb` CLI
   (`pause.rs`), `web` (`lib/pause.ts`, the regenerated `lib/api.gen.ts`).
   For the reminders (D9): `apb-core` (`policy_file.rs`,
   `notify/precedence.rs`), `apb-engine` (`event.rs`, `notify/mod.rs`,
   `notify/start.rs`, `notify/transitions.rs`, `notify/notice.rs`,
   `notify/view.rs`, `notify/send.rs`, `connector/call/request.rs`,
   `connector/call/send_once.rs`), `apb` CLI (`notify.rs`).
   No change to `workdir.rs`, `driver.rs`, `scheduler/deferred.rs`,
   `scheduler/resume.rs`, `pause/request.rs` or `pause/resume_all.rs`.
2. Journal: no new event type. A park is a `run_paused` by the engine;
   the run carries a `run_guard` naming the new feature `wait_park`. The
   reminders add one field, `notify_set.remind_ms` (serde default, an
   older apb ignores it and sends no reminder; best effort across
   binary versions, no run guard), and their sends are
   ordinary send-ledger intents under their own labels.
3. New state: none; one new lock file per run, `control.jsonl.lock`,
   beside `control.jsonl`. No new dependency, no new validation rule.
4. Files: 51 with this change's own records (design.md "Code map"): 34
   for the park and 17 for the reminders.
5. Behaviour changes for release notes:
   1. A timer wait with more than a minute left pauses the run until the
      timer is due: no driver and no agent runs (only the small pause
      waker waits), the tree of a write run stays locked, the run resumes
      itself when the timer is due, also after a reboot through
      `apb resume --all`.
   2. Such a run reads `paused` with the line "waiting on timer `<node>`
      until <time>", and no "paused" notification is sent for it.
   3. A run whose playbook has a timer wait carries the run guard
      `wait_park`: while such a run waits on its timer, an apb older than
      this change refuses to resume or pause it, and can still read and
      stop it. Limit: once an older apb stopped such a run, resuming it
      with that older apb counts the time it waited as paused, so the
      timer waits again and a run deadline may end the run.
   4. A pause or a stop sent while a run is about to wait on its timer
      takes effect as it would without the wait. A note, patch or other
      supervisor change sent while the run waits on its timer reaches it
      within a few seconds (one poll of the waker).
   5. Two control commands sent to the same run at the same moment (for
      example a pause and a supervisor note) are both applied; before
      this change they could take the same number and one of them could
      be skipped. This holds for commands sent by this apb and later;
      sending commands to one live run from two apb versions at once is
      not supported.
   6. A timer wait that pauses this way is not counted as an
      intervention in `apb stats` or the retro; a person's resume of it
      is.
   7. `policy.yaml` accepts `notify.remind` (reminders for an open
      review, question or host task, on active run time, off by
      default; a project file may only turn them off). An older apb
      reads a file with it as invalid (it refuses launches through
      agent channels and creates no default notify subscription until
      updated), as for every earlier key of that file.
   8. Reminders are recorded in the send ledger under their own label;
      a reminder is never sent or recorded once its wait closed before
      its intent was admitted. A reminder that failed is tried again.
      A reminder goes only where the request's notice went: after the
      notification target changes, a request already open gets no
      reminder, and the next one is reminded at the new target. A gate
      asked again gets its own reminders. A run parked on a timer wait
      gets none. Reminders are sent only while an apb of this version or
      later drives the run; an older apb sends the notice only.
9. Shared files with before-running-alone, which can be built in
   parallel: `validate/guarded.rs`, `policy_file.rs` and its test,
   `event.rs`, `scheduler.rs`, `stop.rs`, `next_action/unattended.rs`,
   the engine `tests/main.rs`, `journal_corpus_test.rs`,
   `docs/HOWTO-authoring.md`, `docs/GUARDRAILS.md`, `CLAUDE.md`,
   `AGENTS.md` and the generated `api.gen.ts`; each change adds beside
   the other (tasks 0.1 names the order). This change is meant to merge
   first.
