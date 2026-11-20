# Design: wait park

Read `proposal.md` first. Every file:line below was read on main e76aa42e
(after "People in the loop", #288; the change was first written on
fd49e0da, after "Typed inbox reads", #287, and re-checked on the
rebase). If a line moved, find the site by the name given next to it,
never by the number alone.

## Revision 2: the cut

Two design reviews of revision 1 found crash windows, and every one was
in machinery revision 1 added beside the existing pause: the `try_park`
hand-over with its own lock writes (`park_in_place`, `take_back`), the
waker applying a pending stop through a control rule extracted from the
drive's scan, a person's pause over a park inheriting its lock, and the
`release_paused` change. That is the failure of the earlier attempt
(`typed-inbox-and-released-waits`) again, one level smaller.

Revision 2 adds no hand-over and no control handling of its own
(revision 3 adds the two narrow ones of D6 and D7, see below). A drive
that would sit in a long timer wait posts the engine's own pause, the
way the quota pause and the budget gate post theirs today, and the
existing drain path applies it. Everything after the post (the
`run_paused`, the driver's exit, the parked lock, the waker, a person's
pause, resume or stop, a crash) is main's behaviour for an operator
pause with `--keep-lock` and `--until`, listed with file:line under
"Unchanged from main".

Removed from revision 1: `try_park` and its nine-step hand-over table,
`WaitPark`, the test hooks `WaitParkBeforeLock`, `WaitParkLocked` and
`WaitParkJournaled`, `WorkdirGuard::park_in_place` and `take_back`, the
`release_paused` change, `deferred::controls_pending`, the extracted
control rule (`TerminalStep`, `terminal_step`, `salvage`,
`commit_terminal`, `pending_terminal`) and the waker's pending-stop
handling (old D3), the rule for a person's pause over a park (old D5),
the wait wording of `busy_reason`, the dashboard's Pause button for a
park, and crash rows W1 to W10 (replaced by P1, P2, G1 and G2).

## Revision 3: the park yields

Design review 3 found that a park posted by the drive can sit in front
of a person's later pause or stop, and behind a patch that changed the
run, because the scan applies the first Pause it meets. Revision 3 makes
two targeted additions to existing paths and nothing else new:

1. D6, in the control scan (`scheduler/control_apply.rs`): a park is
   applied only when it is the last pending entry and nothing else was
   applied in the same pass, decided again under the run's event lock;
   otherwise the scan discards it and goes on. The stop watcher
   (`stop.rs`) does not latch `halt` on a park, so a discarded park
   halts nothing (the code required this, see D6 "The stop watcher").
2. D7, in the pause waker (`pause/waker.rs`): the waker of a park that
   finds a control entry pending at its start resumes the run at once,
   once.

It also keeps one reason form only (D2), states that `keep_lock` follows
a busy lock actually held (D1), and makes the process test wait for the
settled state (Tests 18).

## Revision 4: one numbering, a waker that keeps looking, no park left behind

Design review 4 found that D6's ordering rests on a property main does
not have, that D7 looked for a pending control only once, and that one
more exit of the scan leaves a park behind it. Revision 4:

1. D8 (new), in `control.rs`: `post_control` numbers its entry and
   appends it under one lock, for every writer (narrowed in revision 5
   to every writer of this apb and later, D8). This is a fix of a
   defect of main that the park exposes: today two writers can pick the
   same sequence number (`control.rs` 128 counts the lines, 134 to 136
   appends, no lock between).
2. D7: the waker of a park looks for a pending control entry on every
   poll of its existing loop, not only at its start.
3. D6 rule 5: every exit of a pass (an applied patch, a patch that
   paused the run, an Abort, a Pause other than a park, a salvage)
   discards a park that is the entry right after the one it ended at,
   in the same cursor write.
4. From the review's own question (step 4 of the brief): D6 rule 2 also
   requires that the park names the node the drive is at, so a park
   left behind an unconsumable `Retry` or `ContinueFrom` is never
   applied at a later node; `pending_pause` (`control.rs` 228) skips a
   park, so no surface says `pausing` for one; and the claim "a park
   never delays another control" is replaced by the bound the code
   supports (Goals 6).

## Revision 5: limits stated, a waker that survives a torn read

Design review 5 raised five findings. Two are rejected as goals and
recorded as limits; three are fixed:

1. Rejected, recorded as a limit: an older apb that stops a parked run
   and then resumes it from a node counts the park as paused time. This
   is the run guard contract every guarded feature has on main ("An
   older apb", the limit stated there; Goals 5).
2. Rejected, recorded as a limit: an older apb posting a control while a
   drive of this version runs can still take a number already used,
   because it numbers without the lock of D8. Two binaries of different
   versions operating on one live run at once is unsupported on main for
   every control (D8 "Writers of an older apb"; Unchanged 12).
3. Fixed in D7: the waker's per-poll read never ends the waker on a read
   error; a line that does not parse or an I/O error is retried at the
   next poll. Test 13 (`a_half_written_control_line_does_not_end_the_waker`).
4. Fixed in D3 and Surfaces 12: a park and the waker's automatic resume
   of it are not interventions; a person's resume of a park stays the
   person's (`run_stats/interventions.rs`, which the retro shares).
5. Fixed in Tests: the race test that claimed to produce both orders is
   gone. The two orders of D6 "What each interleaving does now" 7 are
   tests 9 (park, then pause) and 16 (pause, then park), each with its
   posts made one after the other in a fixed order; the race stays only
   in the unit test of D8, for distinct numbers.

## Revision 6: reminders for open waits moved in

The owner moved the reminders for open waits from the change
before-running-alone (its draft D4) into this change, whole, in that
change's design review round 2. They are D9 below, with their own
capability `wait-reminders`, tasks section 4a and tests, and with the
decisions of that change's review rounds 1 and 2 applied: the openness
of the wait is rechecked when the reminder's send intent is admitted
under the journal lock, for every reminder intent including the
`not_sent` ones (round 2, major 2); a re-asked request restarts its
reminders. This change decides how a reminder treats a park: a park is
not a wait on a person and is never reminded (D9 "Which waits, and
when"). The park itself (D1 to D8) is unchanged by this revision.

## Revision 7: reminders retried, bound to their target, best effort across versions; a stale park discarded

Design review 6 raised five findings; the owner decided each:

1. D9: `due` selects the latest due threshold's label and applies
   `may_try` to it, so a failed first try of a reminder is tried again
   (the skip of revision 6 is removed).
2. D9: the label's ledger admission is rechecked under the same journal
   lock as the openness check, on every path that appends a reminder
   intent, the unresolved target included.
3. D9: a reminder goes only to the target the request's notice was
   confirmed to, with the same pins; after a target change an open
   request gets no reminder, and the next request is noticed and
   reminded at the new target.
4. D9: reminders are best effort across binary versions; no run guard,
   the requirement is narrowed, and "An older apb" says what an older
   binary driving the run does.
5. D6: before a pending park is applied, the wait's due time is computed
   again on the current clock; a park whose `until` no longer equals it
   is discarded by the same discard rule (rule 3.1).

The three findings the review marked pre-existing on main stay out of
scope (Review record). No file is added by this revision.

## Goals and non-goals

Goals:

1. A long timer wait holds no driver process and no agent: the driver
   exits through the drain path and nothing executes the run until the
   timer is due. What stays is what every pause with `until` leaves
   behind: the detached pause waker (`after_pause`, `pause/request.rs`
   207 to 209) and the deadline waker of a run with a deadline (210).
2. A write run keeps its working tree while parked, through the
   operator `--keep-lock` path (`after_pause` 199 to 206).
3. The wait's clock runs through the park exactly as it runs under a live
   driver today.
4. Every surface says the run waits on a timer, and nobody is notified of
   a "pause" nobody asked for.
5. An apb that does not know the park refuses to resume or pause a
   parked run while the park is the run's open state (D4, "An older
   apb"). This is the run guard contract of every guarded feature, with
   its one limit: once that older apb stopped the run, a person's
   explicit resume with it counts the park as paused time ("An older
   apb").
6. A park never outlives another control and delays one only within a
   stated bound: a person's pause or stop posted around a park applies
   as it would without the park, a park never lands on a run a patch, a
   rebind or a note changed, nor at a node other than its wait (D6, D8),
   and an entry posted while the run is parked reaches the run within
   one poll of the park's waker after its line is whole (`APB_WAKER_POLL_MS`, default 5 s,
   `pause/waker.rs` 34) plus the start of the resumed drive (D7). A
   person's pause or stop of a parked run is not delayed at all: the
   pause is journaled directly and the stop finalizes the run itself
   (Unchanged 4, 5). Without a living waker (a failed spawn, a reboot) a
   posted entry waits for `apb resume --all` or a person, as on any
   paused run (Unchanged 6).

The wake guarantee: a parked run wakes at `until` through its pause
waker, or through `apb resume --all` when no waker lives (a reboot, a
waker that failed to spawn or died). Nothing stronger is promised; it is
the guarantee of every pause with `until` on main.

Non-goals:

1. For the park, no new journal event, event field, state file, lock
   function or waker body (the reminders of D9 add one event field,
   `notify_set.remind_ms`, and nothing else). The one new lock is a file name, `control.jsonl.lock`,
   taken through the existing `apb_core::fsutil::hold_lock` (D8). The
   one journal addition is a new feature name, `wait_park`, in the
   existing `run_guard` (D4). The only control handling added is D6 (how
   the scan treats a park entry, the stop watcher and `pending_pause`
   ignoring one), D7 (the waker's check on each poll), each for a park
   only, and D8 (one numbering for every writer of this apb and later,
   a fix of main).
2. No change to how any pause other than a park, a resume, a stop or a
   crash is handled. The limits of every pause listed under "Unchanged
   from main" stay; closing them is not this change.
3. Webhook waits, inbox waits and a rule's unattended run (proposal "Out
   of scope"). The deferred-retry release (`drive_released`) is unchanged.

## The quota pause on main, traced

The park takes this path step for step; only the arguments differ.

| Step | Quota pause on main | Where |
|---|---|---|
| 1 | The attempt loop decides to pause and returns `AttemptOutcome::EnginePaused` with `quota_pause(agent, model, until)`: `by: Engine`, `mode: Drain`, `until`, `keep_lock: false` | `scheduler/node.rs` 3718 to 3724; `pause/quota.rs` 89 to 98 |
| 2 | The sequential arm journals the attempt's events and calls `engine_pause` | `scheduler.rs` 3541 to 3554 (batch arm 1939 to 1956) |
| 3 | `engine_pause` posts the request on the run's control channel (a scenario run journals it in-process instead) | `scheduler.rs` 559 to 568; `scheduler/journal.rs` 592 to 597 `post_engine_pause`; `control.rs` 125 `post_control` |
| 4 | The arm `continue`s; the loop top runs the deadline latch, then the control scan | `scheduler.rs` 1006 to 1057 (`refuses_work` 1010, `scan_control` 1047) |
| 5 | The scan's Pause arm journals `req.to_event()` (a park goes through D6 first: applied this way only as the last entry of the pass, else discarded) (a `run_paused` with the request's reason, `by`, mode, `until` and `keep_lock`), then writes the control cursor, and returns `Terminal(Paused)` | `scheduler/control_apply.rs` 81 to 86; `pause.rs` 83 to 92 `to_event` |
| 6 | The drive returns `RunResult { outcome: Paused }`; leaving `drive_inner` drops its `DriveClaim`, which removes `driver.pid` | `scheduler.rs` 1058 to 1063; claim 737; `driver.rs` 182 to 188 |
| 7 | `drive` runs the notify sweep, then `after_pause` | `scheduler.rs` 507 to 524 (`notify_after` 511, `after_pause` 522) |
| 8 | `after_pause` reads the latest pause; with `keep_lock` it parks the still-held busy lock under the run (`WorkdirGuard::park`, the file becomes `paused:<run>`); a lock that cannot be parked is a warning | `pause/request.rs` 187 to 206; `pause/waker.rs` 72 `latest_pause`; `workdir.rs` 56 `park` |
| 9 | With `until` it spawns the pause waker; it arms the deadline waker | `pause/request.rs` 207 to 210; `pause/waker.rs` 292 `spawn_waker`; `waker.rs` 171 `start` (only the `apb` binary spawns; a failed spawn is a warning) |
| 10 | The driver process exits. The waker sleeps toward `until` in polls, exits when another pause, resume or stop replaced its pause, checks the deadline, probes (a quota pause only) and resumes the run while its pause holds | `pause/waker.rs` 131 `run_waker`, 60 `same_pause_until`, 159 `stop_if_expired`, 166 `probe_before_wake`, 248 `resume_while` |
| 11 | The resumed driver takes the parked lock as the run's own and restarts the interrupted node | `workdir.rs` 358 `acquire_at`, 64 `took_parked`; `scheduler/resume.rs` 157 `interrupted` |

The budget gate posts its pause the same way in two lines,
`journal::post_engine_pause(run_dir, p)?; continue;` (`scheduler.rs` 1204
to 1206). The park is those two lines in the wait branch.

### `keep_lock` needs no new argument

`PauseRequest` carries `keep_lock` today (`pause.rs` 57 to 59), the scan
journals it (step 5) and `after_pause` honours it (step 8). The engine's
own pauses pass `false` (`pause/quota.rs` 96, `pause/budget.rs` 49,
`pause/billing.rs` 35). An operator's `--keep-lock` takes the same path
for a driven run: `apb pause --keep-lock` builds the request
(`apb-cli/src/pause.rs` 19 to 25), `request_pause` posts it to the live
driver (`pause/request.rs` 81 to 90), and steps 5 to 10 above apply it.
The park builds its request with `keep_lock: lock.is_some()` and reuses
that code unchanged. `keep_lock` therefore states that the drive
actually holds a busy lock, not that the run writes: a write run started
with `allow_shared_workdir` takes no lock (`workdir.rs` 287 to 289
`acquire_tree`, 300 to 302 `acquire_tree_for` return `None`) and parks
with `keep_lock: false`, as does a read-only run.

## D1. When a wait parks, and where

In the wait branch of the drive loop, in the `else` arm that sleeps today
(`scheduler.rs` 2702 to 2704), before `std::thread::sleep`. All of these
must hold:

1. Kind: `wait_for` is `WaitFor::Timer { seconds }`, `scripted.is_none()`
   and the run is not a scenario run
   (`crate::scenario::session::is_scenario(run_dir)` false; a scenario's
   wait is due at once, 2566 to 2571, and `engine_pause` would journal a
   scenario pause in-process, 544 to 547).
   The node's id reads back from its reason: `parse_wait_reason(
   wait_reason(node)) == Some(node)`. Node ids are not restricted to safe
   segments (`deploy/prod`, `a..b` are valid), and such a reason is not a
   park to any reader, so the pause would be applied as an ordinary
   engine pause, counted as paused time and parked again forever; that
   wait stays a live wait (review round 1, B1).
2. Nothing else runnable: `state.deferred.is_empty()` and every entry of
   `frontier` is `current`.
3. No control pending: `control::read_control_after(run_dir,
   control_cursor)` is empty. The scan at this turn's top consumed
   everything it can; what is left is an entry only a wake consumes, and
   posting behind it would leave the park unapplied and post it again at
   the next turn.
4. How long and the mode, one existing call:
   `deferred::may_release(cfg, mode, wake_at_ms, now)`
   (`scheduler/deferred.rs` 42 to 54: top level, no eval, not
   unattended, not supervised, and `wake_at_ms - now` more than
   `release_after()`, 60 s, `APB_RELEASE_AFTER_MS` in a debug build, 62),
   with `wake_at_ms = now + left` and
   `left = min(seconds, timeout_seconds) * 1000 - elapsed` (`elapsed`
   the unscripted value of `scheduler.rs` 2666).
5. The guard: the run's latest `run_guard` names `wait_park`
   (`crate::event::latest_run_guard(&events)`, `event.rs` 3498, over the
   events the branch read at `scheduler.rs` 2665). A run started by an apb before this
   change carries no such guard and keeps its driver (D4).

When all hold, the branch posts the park and goes to the loop top, where
step 4 of the trace applies it:

```rust
let now = apb_core::clock::now_ms();
// One `now` for the elapsed time and the wake time, so D6 rule 3.1 can
// compute the same value again.
// No due time, no park: a park never carries an unbounded `until`. A
// park the scan discarded is not posted again before the journal or the
// control channel changed or `PARK_RETRY` passed (below).
let Some(wake_at_ms) =
    wait_park::due_at_ms(&events, &current, *seconds_or_zero, *timeout_seconds, now)
        .filter(|_| !park_retry.holds(events.len(), control_cursor))
else { /* sleep and continue */ };
if let Some(pause) = wait_park::park_request(wait_park::ParkCheck {
    timer: matches!(wait_for, WaitFor::Timer { .. }),
    scripted: scripted.is_some(),
    scenario: crate::scenario::session::is_scenario(run_dir),
    alone: state.deferred.is_empty() && frontier.iter().all(|n| n == &current),
    controls_pending: !read_control_after(run_dir, control_cursor)?.is_empty(),
    may_release: deferred::may_release(cfg, mode, wake_at_ms, now),
    guarded: crate::event::latest_run_guard(&events).is_some_and(|g| g.iter().any(|f| f == "wait_park")),
    node: &current,
    wake_at_ms,
    keep_lock: lock.is_some(),
}) {
    // A numbering lock busy past its bound posts nothing (D8 item 5).
    if let Some(seq) = control::post_control_unless_busy(run_dir, Control::Pause(pause))? {
        park_retry.posted(events.len(), seq);
        continue;
    }
}
std::thread::sleep(AWAIT_CONTROL_POLL);
continue;
```

A discarded park is not posted again on every 50 ms poll (review round
1, audit m2). The drive keeps one `wait_park::ParkRetry` across its loop:
the journal length the wait read and the seq of the park it posted
(`post_control_unless_busy` returns the entry's number). A discard journals
nothing and moves the cursor to the park's entry, so a drive back at the
wait with the same journal length and its cursor at that entry knows
nothing changed since; it posts no new park until a new event arrived, a
control entry was consumed (the cursor moved on), or `PARK_RETRY` (1 s)
passed. Everything the loop already reads, no new state on disk.

`seconds_or_zero` stands for the timer's seconds (zero for a webhook
wait, which `timer: false` refuses anyway). The exact shape of
`ParkCheck` is the implementer's; the decision is a pure function of
these inputs, so it is unit-tested without a drive.

New file `crates/apb-engine/src/scheduler/wait_park.rs`, declared next to
`mod deferred;` in `scheduler.rs`, holds only:

1. `left_ms(seconds, timeout_seconds, elapsed_ms) -> u128`, saturating,
   and `due_at_ms(events, node, seconds, timeout_seconds, now) ->
   Option<u64>`: `now + left_ms(.., wait_active_ms(events, node, now))`,
   `None` before the wait started. The wait branch (wake time) and the
   scan (D6 rule 3.1) both call it, each with one `now`.
2. `ParkCheck` and `park_request(check) -> Option<PauseRequest>`:
   conditions 1 to 5 (with the node's round trip of condition 1) and
   the request `PauseRequest { reason: Some(wait_reason(node)), by:
   Some(PauseBy::Engine), mode: PauseMode::Drain, until:
   Some(wake_at_ms), keep_lock }`, where `keep_lock` is the caller's
   `lock.is_some()`: true exactly when the drive holds a busy lock.

When the timeout is shorter than the timer, the run wakes at the timeout
and the resumed drive journals `wait_timeout` (2595 to 2601), as a live
drive would.

`wake_at_ms` is computed by the caller of `park_request` and passed in,
so the inbox wait of `evals-and-inbox-waits` can post the same request
with its own wake time.

## D2. The wait reason and `PauseCause::Wait`

In `pause/cause.rs` and `pause/kind.rs`:

1. `pub const WAIT_PREFIX: &str = "wait (";`
2. `pub fn wait_reason(node: &str) -> String` gives `wait (<node>)`.
3. `pub fn parse_wait_reason(reason: &str) -> Option<&str>`: the node of
   the one form `wait (<node>)`. The reason must start with the fixed
   prefix `wait (` and end with `)`; everything between is the node id,
   taken verbatim (no splitting at a comma or a parenthesis), and it must
   be a safe segment (`apb_core::registry::is_safe_segment`). Any node
   id that is a safe segment round-trips: `wait_reason` then
   `parse_wait_reason` gives the id back, also for `a, inbox`, `x (y)` or
   `a)`. A valid node id that is not a safe segment (`deploy/prod`,
   `a..b`) does not, and D1 condition 1 never parks on it. There is no
   second form.
   A later change that adds another wait kind brings its own reason form,
   one that no `wait (<node>)` reason can be read as.
4. `PauseRequest::is_park(&self) -> bool` (in `pause.rs`, beside the
   type, so `cause.rs` never reaches back into its parent): `by ==
   Some(PauseBy::Engine)` and `parse_wait_reason` reads its reason. The
   one predicate the scan (D6), the stop watcher and `pending_pause`
   (D6) use for a request; for a journaled `run_paused` every reader,
   the waker (D7) among them, uses `cause::is_park_event(reason, by)`,
   `cause_of` giving `Some(PauseCause::Wait)`, the one predicate the
   clock, the waker, the notify fold and the interventions count share.
5. `cause_of` (`pause/cause.rs` 40) returns `Some(PauseCause::Wait)` for
   an engine pause whose reason parses, checked after billing. A
   person's or a supervisor's pause with the same words is never a wait
   (`cause_of` returns `None` for any `by` other than `Engine`, 41).
6. `PauseCause::Wait` (`pause/kind.rs` 66, `as_str` "wait"); the
   TypeScript contract regenerates `PauseCause`.

No engine reason starts with `wait (` today (the engine reasons are
`scheduler.rs` 2220, 3658, `scheduler/patch.rs` 60 and the quota, budget
and billing prefixes, `pause/cause.rs` 10 to 14), so no older journal's
pause reads as a park.

## D3. The clock exemption

`paused_fold` (`pause/clock.rs` 41) records for each segment whether it
is a park: a `run_paused` with `by == Some(Engine)` and
`cause_of(reason, Engine) == Some(Wait)`. The segment tuple becomes a
small struct `{ start, end, by, park }`. `paused_intervals` (86) returns,
for each interval, the spans of its segments that are not a park, merged
where adjacent, so a park followed by a person's pause yields one
interval from the person's `run_paused` on. `deadline_elapsed_ms` (99)
keeps subtracting only `operator` segments (126), so the park counts
toward the deadline as every engine pause does.

Every reader of paused time and what it reads for a park (the expected
behaviour is always "as if a live driver waited"):

| Reader | file:line | Behaviour with a park |
|---|---|---|
| the wait clock | `scheduler/journal.rs` 246 `wait_active_ms` | the park is active time: the resumed wait ends at its due time |
| a question's clock | `scheduler/journal.rs` 254 `question_active_ms` | no question is open during a park (only the wait is in the frontier); unchanged |
| a deferral's due time | `scheduler/deferred.rs` 144 in `due_in` | no deferral is open during a park (D1 condition 2); a later one reads the park as active |
| `active_elapsed_ms`, `paused_ms_between`, `paused_ms_in`, `inactive_intervals`, `inactive_ms_between`, `active_run_ms` | `pause.rs` 223 to 258 | all over `paused_intervals`: the park is active |
| `shift_by_paused` (eval deadline watch) | `pause.rs` 263, `scheduler.rs` 744 | an eval never parks (`may_release`); unchanged |
| `{{run.active_elapsed_seconds}}` | `pause.rs` 275, `context.rs` 742 | the park counts, as a live wait does |
| host task deadline and timeout | `host_task.rs` 476 and 491 | no host task is open during a park; unchanged |
| next-action deadlines | `next_action/deadlines.rs` 139 | the wait's remaining time keeps running; see "Surfaces" 6 for the `paused` flag |
| pending question timeout | `progress.rs` 752 | no question is open; unchanged |
| node durations | `progress.rs` 1286 to 1298 | the wait node's duration includes the park, as a live wait's does |
| run estimate samples | `run_estimate/sample.rs` 251 | the park is active time, as a live wait is today, so estimates do not change shape |
| `apb stats` spans | `run_stats.rs` 642, `run_stats/spans.rs` 20 | the same |
| the retro | `run_retro.rs` 329, 369, 523 | the same |
| a review decided faster than its scope's floor (added by #288) | `run_stats/interventions.rs` 421 `active_run_ms` | no review is open during a park (only the wait is in the frontier); unchanged |
| the run listing's run time | `scheduler/listing.rs` 216, 320 | the same |
| retention summaries | `retention/mod.rs` 623 | the same |
| the run deadline | `pause/clock.rs` 99 | unchanged: engine pauses already count |

The paused-interval doc comment of `pause/clock.rs` (1 to 5) gains one
sentence: a wait park is the engine's own way to wait and
is not paused time.

The intervention counts are not a clock reader but follow the same
rule. `run_stats/interventions.rs` `interventions` (296) counts every
`run_paused` and `run_resumed` by `by` (449 to 455, `actor` 560), so a
live wait adds nothing while each park would add an engine `pause` and
an engine `resume`. #288 ("People in the loop") grew this fold
(scope re-asks, decisions faster than their floor) but left the
`run_paused` and `run_resumed` arms as they were, so the rule below
still sits in those two arms. The fold therefore skips:

1. a `run_paused` by `Engine` whose cause is `Wait`
   (`cause_of(reason, by)`, D2 5);
2. a `run_resumed` by `Engine` whose open pause (the latest `run_paused`
   before it with no `run_resumed` between) is such a park: the waker's
   resume at `until` and its early resume of D7.

A `run_resumed` by `Operator` or `Supervisor` of a parked run is counted
as today, as the person's or the supervisor's. The retro reads the same
fold (`run_retro.rs` 556 `of_run`, printed by `run_retro/text.rs` 195),
so it needs no change of its own. Unit test
`a_park_and_its_automatic_resume_are_no_intervention` (an engine park,
the engine resume after it, then a second park and an operator resume:
`engine` is empty, `operator` holds one `resume`; a quota pause and its
engine resume still count under `engine`).

## D4. The run guard `wait_park` (as revision 1 decided)

An apb that does not know the park reads it as an ordinary engine pause
and counts it as paused time (`pause/clock.rs` 86,
`scheduler/journal.rs` 246), so its resumed drive would wait the whole
timer again, and with a run deadline (which counts engine pauses,
`pause/clock.rs` 99) end the run aborted where this apb would have gone
on: a 2 hour timer under a 3 hour deadline is resumed at hour 2, waits 2
more hours, and is aborted at hour 3. The existing run guard keeps that
apb from resuming or pausing the run while the park is its open state,
with no new write path (its one limit, a stop by that apb followed by
its explicit resume, is stated under "An older apb"):

1. `apb-core/src/validate/guarded.rs`: `GuardedFeature::WaitPark`
   (`as_str` "wait_park", last in `ALL`, which grows from 12 to 13:
   #288 added `review_fields` and `review_max_scope` after `concurrency`,
   and #289 added `list_pilot` after them; the order is the declaration
   order, so a guard naming both reads `["list_pilot", "wait_park"]`).
   `guarded_features` names it when any node is a `wait` whose `wait_for`
   is a timer (`schema.rs` 2260 `WaitFor::Timer`). The module doc gains
   one sentence: an apb that does not know the park would count it as
   paused time and wait the timer again.
2. Written at start from the definition: `scheduler/prepare.rs` 1644
   appends the run's guard right after `run_provenance`, before any node
   runs. `run_guard_of` (1973) passes every definition feature through
   except `deadline` and `concurrency`, so it needs no change; nor does
   `patch.rs` 259, which recomputes the guard from the patched
   definition.
3. Kept by the event log: the tracker appends the guard after every
   checkpoint in the same write (`event.rs` 3069 to 3080, in
   `append_lines`, which `append_inner` calls since #288), so the park's
   `run_paused` (a checkpoint, 3799) is never the journal's last line.
4. The park checks it (D1 condition 5).

Accepted cost: a run whose timer waits can never park (a child, an eval,
an unattended or supervised run, a timer of a few seconds) is guarded too,
and an older apb refuses to resume or pause it. The guard follows the
definition, as `evidence`, `write_scope` and the approval features do.

## D5. Resume and retry-now of a parked run

Nothing new is written for these; each follows from D1 and D3 over
main's resume.

1. A resume before `until` (`apb resume`, MCP `run_resume`, the
   dashboard, `apb resume --all` does not resume a pause ahead): the
   resumed drive restarts the interrupted node, which is the open wait
   (`scheduler/resume.rs` 157; the wait branch does not open it again,
   `scheduler.rs` 2651), reads `elapsed` on the clock of D3, finds more
   than `release_after` left and posts a new park whose `until` is the
   same wake time within one poll.
2. A resume at or after `until` (the waker, `apb resume --all`, a
   person): the drive finds `elapsed >= seconds * 1000` and journals
   `wait_signalled` (2581 to 2586), or `wait_timeout` past the timeout.
3. A person's pause during the park followed by a resume: the person's
   segment is paused time (D3), so the wait's remaining time is what it
   was when the person paused, and the resumed drive parks again for it.
   A person's pause posted while the park's entry is still pending is
   applied instead of the park (D6), with the same result.
4. Retry-now: refused `no_deferred_retry` (`retry_wait.rs` 187); a park
   has no deferral. Unchanged.
5. `apb resume` prints the early-resume note today
   (`apb-cli/src/pause.rs` 70 `early_resume_note`, "resuming early: the
   pause was set until <T>"). For cause `wait` it reads ``the wait on
   timer `<node>` is due at <T>; the run waits again until then``.

## D6. The scan applies a park only as the last word

Review 3 named two interleavings in which the park, once posted, is
applied wrongly. The wait branch checks condition 3 (no control pending)
and then posts the park; another poster can append between the two, or
after the post and before the scan:

1. A person's pause or stop is appended after the park. The scan applies
   pending entries in sequence order and returns at the first Pause
   (`scheduler/control_apply.rs` 81 to 86), so it applies the park, the
   driver exits, and the person's entry stays pending while the park's
   exempt clock lets the timer run.
2. A patch is appended before the park. The scan applies the patch and
   ends the pass (`control_apply.rs` 125 to 128, `Migrated`); the next
   pass applies the park on the changed run.

Revision 2's "Unchanged from main" 1 claimed that a command posted after
condition 3 and before the scan has a lower sequence than the park. That
is false when the drive posts first: `post_control` (`control.rs` 125)
numbers an entry by the line count it read before it appends. Revision 3
removed the claim and added this rule. Review 4 showed that on main the
count and the append are not even atomic together, so two writers can
pick the same number and the file order need not be the number order;
D8 makes the number the append order, which this rule relies on.

### The rule

In `scan_control` (`control_apply.rs` 27), for an entry
`Control::Pause(req)` with `req.is_park()` (D2 4):

1. The pending tail of a pass is `pending_control`, the one read of
   `control.jsonl` after the in-memory cursor at the top of the pass
   (`read_control_after(run_dir, *control_cursor)`, line 65). That read
   takes no lock, as on main; writers number and append under the
   control lock of D8, so every entry in the file carries a number
   higher than every entry before it.
2. The park is a candidate only when all of these hold:
   1. it is the last entry of `pending_control`;
   2. no entry before it in the same pass was consumed. A new local
      `consumed: bool`, false at the top of the pass, is set by every
      arm that writes the cursor (`ContextAppend`, `Patch` applied or
      rejected, `Progress`, `Interrupt`, `Rebind`; the `Abort` and the
      other `Pause` arms return);
   3. it names the node the drive is at: `parse_wait_reason` of its
      reason is `Some(current.as_str())` (`current` is the scan's
      argument, line 33). A park can stay pending behind an entry the
      scan does not consume (a `Retry` or `ContinueFrom`, the `break` at
      188 to 193) while the wait ends live; once `await_control` takes
      that entry, the park would otherwise be alone at a later node. A
      park for the same node in a later visit of a loop carries the
      `until` of the earlier visit, computed from that visit's
      `wait_started`, so the due time of rule 3.1 differs and it is
      discarded; the wait branch decides afresh.
3. A candidate is applied under the run's event lock
   (`apb_core::fsutil::lock_dir(run_dir, crate::stop::EVENT_LOCK)`, the
   lock `request_pause` (`pause/request.rs` 73), `stop_run_with`
   (`stop.rs` 194 to 196) and the drive's own `try_release`
   (`scheduler/deferred.rs` 329) decide under). Holding it, the scan
   reads `read_control_after(run_dir, Some(entry.seq))` again. When that
   is empty it computes the wait's due time again on the current clock
   (rule 3.1) and, when it equals the park's `until`, journals
   `req.to_event()` and writes the cursor to `entry.seq` exactly as the
   Pause arm does today, gives the lock back and returns
   `Terminal(Paused)`. The lock covers only that re-read, the
   append and the cursor write. `EventLog::append` takes no event lock,
   so there is no self-deadlock; `try_release` appends under the same
   lock today. No caller of `scan_control` holds the event lock (the
   implementer checks the call at `scheduler.rs` 1047).
   1. The due time again (design review 6, finding 5). A park's `until`
      was computed when the wait branch posted it; a pause journaled
      directly in between makes it stale. The case: the driver posts the
      park and dies before its scan; a person's `apb pause` finds no live
      driver (`pause/request.rs` 81 to 90) and journals its `run_paused`
      directly, leaving the park pending in `control.jsonl`; a resume does
      not move the control cursor, so after the person's resume the first
      scan finds the park last and alone at the same node and would apply
      its old `until`, which the paused interval made too early. So, under
      the lock and after the empty re-read, the scan reads the journal
      (`read_all(run_dir)`), takes one `now = apb_core::clock::now_ms()`,
      and computes `wait_park::due_at_ms(&events, &current, seconds,
      timeout_seconds, now)` for the node `*current` of `playbook`: `now`
      plus `wait_park::left_ms(seconds, timeout_seconds, elapsed)` with
      `elapsed = scheduler::journal::wait_active_ms(&events, &current,
      now)` (`journal.rs` 246, over `pause::active_elapsed_ms`, the paused
      interval fold with D3's park exemption). It is `None` when the node
      is not a timer wait or its wait has not started. The park is applied
      only when that is `Some(req.until)`. While the run is not paused,
      `now - elapsed` does not move, so the due time recomputed from the
      same journal equals the one the wait branch posted (D1 computes it
      with the same function and one `now`); a paused interval since the
      post moves it later by that interval, and a due time already past
      saturates `left` to 0 and gives `now`, so a stale or past park never
      equals it.
4. Every other park entry is discarded: it is not a candidate (2), the
   lock cannot be taken (`lock_dir` gave an error after its bounded
   wait), the re-read under the lock is not empty, or the due time
   computed again under rule 3.1 is not the park's `until`. A discard sets
   `*control_cursor = Some(entry.seq)`, persists it with
   `write_control_cursor(run_dir, entry.seq)`, journals nothing, and the
   loop goes on with the next entry of `pending_control`. A discard
   changes nothing else in the run. A lock that cannot be taken is
   matched with `let Ok(lock) = .. else { discard; continue }`, the shape
   of `deferred::try_release`, never propagated: a held or freshly
   crashed event lock must not end the drive (review round 1, M1; test
   `control_apply::tests::a_park_whose_event_lock_is_busy_is_discarded`).
5. Every exit of a pass that consumed an entry discards a park that is
   the entry right after that one in `pending_control`. The exits are:
   the `Abort` arm (77 to 79), the `Pause` arm for an entry that is not
   a park (81 to 86), a patch that applied (`PatchResult::Applied`, the
   `break` at 124 to 128, then `Migrated` at 266 to 269), a patch that
   paused the run (`PatchResult::Paused`, 130 to 132: the patch limit,
   `scheduler/patch.rs` 58 to 63, journals an engine pause without
   applying the patch), and the salvage (253 to 262). Each of these
   writes its cursor once today, after its effect; when the entry right
   after its own is a park, it writes the higher of the park's `seq`
   and its own in that one write (and sets `*control_cursor` to it where
   it sets it today; the park's is lower only after a stop posted
   without the numbering lock, D8 item 4). One write covers both, so no crash leaves the consumed
   entry applied and the park still pending; the park is discarded as in
   4 (nothing journaled for it). Without this a later pass (after a
   person's resume of the pause, or the migrated iteration) would see
   the park alone and apply it with a wake time computed before the
   pause or on the patched run. A park further back (behind another
   entry) is discarded by the next pass under 2.2, because that entry is
   consumed first; a park behind an unconsumable `Retry` or
   `ContinueFrom` (the only `break` that consumes nothing) is never a
   candidate at another node under 2.3, and while the drive is still at
   its wait, condition 3 of D1 keeps the wait live and parks nothing
   new until the blocking entry is consumed.
6. The salvage below the loop (`control_apply.rs` 231 to 263, a terminal
   entry queued behind an unconsumable `Retry` or `ContinueFrom`) never
   salvages a park: its `find_map` arm for `Control::Pause(req)` (242)
   matches only `!req.is_park()`. A salvaged Abort or Pause behind a park
   moves the cursor past the park, which discards it the way the salvage
   discards the Retry today.

### What each interleaving does now

1. A person's pause or stop appended after the park and before the
   tail read: the park is not last, it is discarded, and the same pass
   applies the person's entry through its own arm. The journal holds the
   person's `run_paused` or `run_aborted` and no park.
2. Appended after the tail read and before the lock: the re-read under
   the lock sees it, the park is discarded, the pass ends (`Proceed`),
   the drive reaches the wait branch, condition 3 sees the entry and
   sleeps one poll, and the next pass applies the entry.
3. A person's pause after the park was applied: `request_pause` decides
   under the same lock and posts only while the run is not paused and
   its driver lives (`pause/request.rs` 84 to 90). Past the park's append
   the run reads `paused`, so it journals its `run_paused` directly, as
   for any paused run (Unchanged 5). No person's pause is ever left
   pending behind an applied park.
4. A stop after the park was applied: `stop_run_with` posts its Abort
   and returns `SignaledLiveDriver` while the exiting driver still holds
   its claim (`stop.rs` 214, 231 to 237); that entry is pending when the
   driver exits. A stop without the lock (it is best effort) may also
   append between the re-read and the append. Both are the exit window
   that D7 closes.
5. A patch, rebind, note or any other entry appended before the park:
   consumed first in the same pass (2), or by the pass before under 5;
   the park is discarded, the wait branch decides afresh on the changed
   run, and parks again only if all of D1 still holds.
6. Appended after the park was applied (a supervisor tool or `apb note`
   posts without the event lock): pending when the driver exits, the
   exit window of D7.
7. Review 4's interleaving: the wait branch posts the park while
   `apb pause --now` posts the person's pause at the same moment. Under
   D8 the two writers take the control lock one after the other, so
   they get two numbers, `N` and `N + 1`, in the order they appended.
   Park `N`, pause `N + 1`: if the scan's tail read holds both, the park
   is not last and is discarded (1); if the read ended at the park, the
   re-read after `N` under the event lock finds the pause and the park
   is discarded (2); the pause is applied by the same or the next pass.
   Pause `N`, park `N + 1`: the scan applies the person's pause first
   and its exit discards the park right after it in the same cursor
   write (rule 5). Either way the journal holds the person's
   `run_paused` and no park, and the cursor never moves past the
   person's pause without applying it. On main both entries could carry
   `N`, and the re-read after `N` would miss the second. Tests 9 and 16
   make the two orders with posts made one after the other. Both writers
   here are this apb's; an older apb's post is not ordered (D8 "Writers
   of an older apb").
8. A pause posted directly before the park, after condition 3's read:
   the case "Pause `N`, park `N + 1`" of 7. A person who resumes later
   gets a drive whose wait decides afresh with the wait's remaining
   time, never the park's old `until`.
9. The driver posts the park and dies before its scan; a person's pause
   finds no live driver and journals `run_paused` directly; the person
   resumes later (design review 6, finding 5). The park is the last
   pending entry, nothing was consumed, and it names the node, but rule
   3.1 recomputes the due time over the journal with the person's paused
   interval in it, which is later than the park's `until`: the park is
   discarded, nothing is journaled for it, and the wait branch decides
   afresh with the remaining active time (it parks again with the new
   due time when that is still over `release_after`). Without the
   person's pause (crash row P2) the recomputed due time equals `until`
   and the park is applied as before.

The park is posted only with nothing else to run (D1 condition 2), so a
discarded park leaves no work behind: the drive goes on exactly as if it
had never posted.

### The stop watcher

The code adds one more site. The drive's `StopWatcher` reads
`control.jsonl` after the drive's starting cursor on its own thread and
latches `halt` ("admit no new work") at the first pending Pause it sees
(`stop.rs` 567 to 568); `halt` is never cleared within a drive and stops
the admission of batch members (`scheduler.rs` 1624, 1860) and list
items (`scheduler/list/batch.rs` 229). A discarded park would therefore
stop every later batch and list of the same drive, for example after a
patch that continues the run. The watcher's Pause check becomes
`matches!(&e.cmd, Control::Pause(r) if !r.is_park())`.
A park is posted only when nothing else runs, so the watcher has nothing
to halt for it; the scan alone decides. The list's own pending check
(`list/batch.rs` 233 to 236) reads after the drive's in-memory cursor,
which a discard moves past the park, so it needs no change. This is the
only change to `stop.rs`.

### `pending_pause`

`control::pending_pause` (`control.rs` 228) gives the first unapplied
`Pause` after the persisted cursor; the run view reads it for a running
run (`run_view.rs` 795 to 797) and `pause_info` reports the run as
`pausing`. A park that is pending (for one turn of the drive, or longer
behind an unconsumable `Retry`, rule 5) would read `pausing` while the
drive goes on waiting. Its `find_map` matches only `!req.is_park()`, the
same guard as the watcher's. `pending_stop` is unchanged.

### Other readers checked

`await_control` (`scheduler/supervisor.rs` 263, returning any other
entry at 300) runs only at a supervised drive's wake, and a supervised
drive never posts a park (D1 condition 4, `may_release` refuses
`parks_on_failure`, `run_config.rs` 128); the top-of-loop scan judges any
park first. `drain_progress_after_execute` (`scheduler/supervisor.rs` 327) stops at the first
entry that is not `Progress`. `try_release` (`scheduler/deferred.rs` 322)
counts a pending park as pending, which only keeps a driver that has no
deferral to release anyway (D1 condition 2).

## D7. The waker of a park resumes the run when a control is pending

What D6 leaves is every entry posted after the park was applied: in the
exit window (a stop that found the driver still claiming the run, a
note, a patch), and at any time while the run is parked (a note, a
patch, a rebind or a supervisor tool, which post without asking whether
a driver lives). No driver scans them; without D7 they wait for the
timer. A person's pause and stop of a parked run are not among them:
the pause is journaled directly and the stop finalizes the run itself
(Unchanged 4, 5).

The waker closes this with one check on each poll of its existing loop,
for a park only. In `run_waker` (`pause/waker.rs` 131), inside the loop
(140), right after `same_pause_until` confirmed that the pause at `seq`
still holds the run (144 to 146) and before the due check (148):

1. When the `run_paused` at `seq` has `cause_of(reason, by) ==
   Some(PauseCause::Wait)` (read once, before the loop, from the journal
   the waker already reads; a quota pause's waker re-pauses under a new
   `seq` at 168 to 175, so the cause is read again when `seq` changes),
   and the pending read (below) gives at least one entry, the waker
   calls its existing `resume_while(root, run_id, WakeHold::Pause(seq))`
   (248) once, drops the lease and returns what it returned.
2. Otherwise the loop goes on unchanged: the due check, the deadline,
   the probe, or `sleep_toward` (one poll, or less when `until` is
   nearer, 233 to 236).

The pending read is `read_control_cursor(&run_dir)` then
`read_control_after(&run_dir, cursor)`, and neither error ends the
waker. `read_control_after` fails on a line that does not parse
(`control.rs` 253 to 254), which is what a reader sees while a writer
is in the middle of its append (D8 "What a reader sees"), and either
call can meet an I/O error (249 for the file). On an error the check counts as "nothing known
pending" for this poll: the loop goes on to the due check exactly as in
2, and the next poll reads again. A note appended while the waker polls
therefore reaches the run at the first poll after its line is whole,
and the due check at `until` is never skipped by a torn read. The log is
bounded: the waker writes one line to its stderr for the first failed
read and none for later ones (a detached waker's stderr is null,
`waker.rs` 194, so the line is seen only by an in-process caller such
as a test). A last line that stays torn (a writer that crashed in its
one write, crash row C1) is retried at every poll and never resumes the
run early; the waker still resumes at `until`, and the resumed drive's
scan meets that line as on main.

The poll is `pause::waker::poll()` (`pause/waker.rs` 50 to 55):
`APB_WAKER_POLL_MS` (32), default 5 s (`DEFAULT_POLL`, 34). An entry
posted while the run is parked therefore reaches a resumed drive within
one poll plus the resume (`resume_while`, `try_engine_resume`) and the
drive's start; its first scan applies it.

One resume per waker: the check that finds an entry returns from
`run_waker` through `resume_while`, which ends the waker, so there is no
second resume and no recursion. The waker only asks whether an entry is
pending; it does not interpret, apply or move any control entry and
never writes the cursor. The resumed drive's first scan applies the
pending entries in sequence order: an Abort ends the run; a note, patch
or rebind is applied and the wait decides afresh, and with all of D1
still holding parks again for the time left (a new `run_paused`, a new
waker for its new `seq`). An entry the scan does not consume (a `Retry`
or `ContinueFrom`) stays pending, so condition 3 of D1 keeps the
resumed driver in its live wait instead of parking: no resume loop.

Why every poll and not only the start: the start covers the exit window
(a stop or pause left pending was appended while the driver still
claimed the run, `stop.rs` 214, 231 to 237, before `after_pause` spawned
the waker, trace steps 6 and 9), but a note or patch posted an hour into
a six hour park would otherwise wait five hours, while a live wait scans
at every 50 ms turn (`AWAIT_CONTROL_POLL`, `scheduler/supervisor.rs`
246). The first poll is the check at the start: the loop's first
iteration runs it before any sleep.

How often a parked run can be woken this way (review round 1, audit
m3). Any pending entry wakes it, `Control::Progress` included. Each wake
is a launch check, a detached driver and, when D1 still holds, a re-park
and a new waker, so a parked run re-parks at most as fast as entries
arrive. No step of the run itself posts while it is parked: a park
needs the wait to be the only frontier node with no deferral (D1
condition 2), so no attempt of the run is running. Another process can
post (a supervisor, or a step of another run naming this run in
`run_progress_report`, which is meant for a step's own run and has no
own-tree rule to lean on); its rate bounds the churn, which costs
resumes and never loops by itself. This is recorded as a limit; no new
mechanism is added.

A resume refused (the gate, `resume_while` returns `Refused`) ends that
waker as a refusal at `until` does today: the entries stay pending,
nothing else wakes the run before `apb resume --all` or a person
(Unchanged 6), and next actions show the overdue line once `until`
passed (Surfaces 6). No new retry is added.

## D8. One numbering of control entries (a fix of main, for this apb on)

On main `post_control` (`control.rs` 125 to 139) counts the lines of
`control.jsonl` (128), then opens the file for append and writes the
entry (133 to 136), with no lock between. Two writers can count the same
number of lines and both write sequence `N`, and the entry appended
second can carry a lower number than one already in the file. Every
reader takes a number to mean the order of the entries: the cursor is a
single scalar (`control.rs` 164 to 191), so the scan that applies one
`N` writes the cursor to `N` and the other `N` is never read again
(`read_control_after` keeps `seq > after`, 256 to 258). This is main's
own defect: today a supervisor note posted beside an operator's pause
can hide one of them. The park exposes it, because D6 decides on the
entries after the park's number.

The fix, in `post_control` only:

1. Take `apb_core::fsutil::hold_lock(run_dir, CONTROL_LOCK)` with a new
   `pub const CONTROL_LOCK: &str = "control.jsonl.lock";` in `control.rs`
   next to the function. `hold_lock` is the existing OS lock (`flock`,
   `fsutil.rs` 794 to 862): the kernel releases it when the holder's
   process ends however it ends, so there is no stale-lock rule and no
   crash leaves it held; a holder that does not let go within its
   bounded wait (80 tries of 25 ms) gives `WouldBlock`, which
   `post_control` returns as an error, as it returns an I/O error today,
   for every writer but a stop (item 4).
2. Holding it: number the entry one above the highest number in the file
   (`read_control_after(run_dir, None)`, as at 128; equal to the line
   count while every writer holds the lock), build the line with its
   newline in one buffer, and append that buffer with one `write_all`
   (`writeln!` on a `File` may issue two writes, and a crash between the
   two would leave a line without its newline for the next writer to
   append onto); flush.
3. Give the lock back by dropping it, then return the number.
4. A stop never fails on this lock (review round 1, audit M2). A stop
   was never kept from stopping a run by anything but the event lock
   (`pause/request.rs` 69 to 72), and the lock is held only for the
   count and the append, so contention is short. When an `Abort` still
   cannot take it within its bound, `post_control` appends it without
   the lock instead of refusing, numbered `UNLOCKED_STOP_GAP` (2^20)
   above the next free number it reads. Every locked writer that counts
   before the stop is in the file (the holder that counted already, and
   any that takes the lock between the stop's count and its append)
   counts from the file's highest entry, below the stop, and would reach
   the stop's number only by posting 2^20 entries in that moment; one
   that counts after the stop is in the file numbers above it. So no
   locked writer publishes the stop's number first, whichever order the
   appends land in. A writer numbered below the stop may then stand after
   it in the file; the scan consumes the stop and moves its cursor to the
   highest number it consumed (D6 rule 5 takes the higher of the stop's
   and a discarded park's), so a consumed stop is never left pending. The
   cost is a gap in the numbers, which nothing reads, and two unlocked
   stops racing each other may share a number, both Aborts, which end the
   run either way. `Abort` is the only writer allowed to post without the
   lock; every other writer keeps the bounded wait and its error. Tests
   `control::tests::a_stop_is_posted_while_the_numbering_lock_is_held`
   and `control_apply::tests::a_stop_numbered_above_a_later_park_is_not_left_pending`.
5. The drive's park is optional: `post_control_unless_busy` posts it and
   answers `None`, appending nothing, while the lock stays busy past its
   bound; the wait sleeps and decides again at its next poll instead of
   ending the drive. Test
   `control::tests::an_optional_entry_is_left_out_while_the_numbering_lock_is_held`.

Lock order. `CONTROL_LOCK` is a leaf: `post_control` takes nothing else
while it holds it, and nothing else takes it. Several callers hold the
run's event lock (`stop::EVENT_LOCK`, a `lock_dir` lock) when they post:
`request_pause` (`pause/request.rs` 73, post at 88), `stop_run_with`
(`stop.rs` 194 to 196, post at 214) and, in the same stop,
`abort_children` posting into a child's file (335) under the parent's
event lock. The order is therefore always event lock, then control lock,
never the reverse; no caller can wait on the control lock while holding
it, and no holder of the control lock waits on the event lock, so there
is no deadlock and no inversion. The scan's re-read under the event lock
(D6 rule 3) and the waker's check (D7) only read and take no control
lock.

What a reader sees. Readers (`read_control_after` in the scan, the stop
watcher, the waker, `pending_stop`, `pending_pause`, the doctor) take no
lock, as on main. A reader sees an entry being appended as absent,
whole, or as a last line still being written; among the whole entries
written by this apb it never sees two with one number, nor a number
lower than one before it. A read that meets a line still being written
fails to parse, as it can on main; the stop watcher retries at its next
poll (`stop.rs` 542 to 544), the waker of a park retries at its next
poll (D7), and the scan's error is main's. This change narrows that
window (one write instead of two) and does not otherwise change it.

Writers of an older apb. The lock orders every writer of this apb and
later. An apb older than this change runs main's `post_control`, which
counts and appends without the lock, so its entry can still take a
number already used while a drive of this apb runs the same run: an
older stop counts `N`, this drive appends its park as `N`, the older
stop appends its Abort as `N`, and the scan that applies or discards the
park moves the cursor past both. This is rejected as a goal and recorded
as a limit: two binaries of different versions operating on one live
run at once is unsupported on main already, for every control (the
same race hides a note or a pause between two older writers today), and
the run guard only governs which binary may resume or pause a run, not
which may post to a live one. Nothing here claims an order for a writer
that does not take the lock (Unchanged 12).

The lock file lives in the run directory beside `control.jsonl` and is
never removed (`HeldLock` keeps the file). `hold_lock` checks
`process_mode::guard_write` for its path; a process whose mode refuses a
write beneath the run directory already cannot persist a cursor or
journal an event there, so no caller that works today is newly refused
(the implementer runs the scenario and read-only suites, task 6.3, and
reports any refusal as a finding).

## D9. Reminders for open waits

Moved here whole from the change before-running-alone (its draft D4) by
the owner in that change's design review round 2, with the decisions of
its review rounds 1 and 2 applied. It shares no code path with the park
(D1 to D8) and touches the notify module, which the park touches only
for its paused notice (Surfaces row 8). A wait notifies once on main:
every transition has one send label (`notify/transitions.rs`
`TransitionId::label`, line 57), and `docs/NOTIFY.md` line 71 says "The
same wait never sends a second message", so a gate nobody answers stays
silent for days.

| Code example | Non-code example |
|---|---|
| a review of a dependency bump nobody answered gets a reminder after 4 h of active run time | a mail approval gate reminds after 1 h, 4 h and 24 h, and never after the decision |

### Where the cadence lives

The cadence is the person's, so it lives in the machine's `policy.yaml`
(`crates/apb-core/src/policy_file.rs`), next to the other notify
filters, never in a playbook:

```yaml
notify:
  remind: [1h, 4h, 24h]   # one reminder after each, of active run time
```

1. `NotifyPolicy` (line 184) gets `pub remind: Option<Vec<String>>`,
   each entry read by `apb_core::duration::parse_duration_str`
   (`crates/apb-core/src/duration.rs` line 14: seconds, or `30m`, `4h`,
   `1d`, `1h30m`; it returns seconds).
2. `NotifyPolicy::validate` (line 219) takes `source: PolicySource`
   (its call in `parse_as`, line 426, becomes
   `file.notify.validate(source)?`, as `file.launch.validate(source)?`
   on line 427 already is) and checks: at most 5 entries, each from 10
   minutes to 7 days, strictly increasing; messages `notify.remind:
   \`{v}\` is not a duration`, `notify.remind has {n} entries; use at
   most 5`, `notify.remind: \`{v}\` is shorter than 10m`,
   `notify.remind: \`{v}\` is longer than 7d`, `notify.remind: \`{b}\`
   does not come after \`{a}\``. An empty list is valid (no reminders).
3. A project file may only turn reminders off: a non-empty `remind` in
   the project's file is the error `notify.remind: a project may only
   turn reminders off (remind: [])`.
4. `Policies::remind_ms(&self) -> Vec<u64>` (next to `finished`, line
   388): the machine's list in milliseconds, empty when the project file
   says `remind: []` or the machine sets none.
5. The module doc (lines 10 to 22) shows the key.

Reminders are off by default (no `remind`). A per-run or per-playbook
cadence and quiet hours are out of scope: a playbook author does not
decide how often someone is nagged, and a person who pauses a run
silences its reminders.

### The subscription carries it

1. `precedence::Effective` (`crates/apb-core/src/notify/precedence.rs`
   line 74) gets `pub remind_ms: Vec<u64>`, filled in `effective` (line
   99) from `settings.policies.remind_ms()` for every source.
2. `NotifyPlan` (`crates/apb-engine/src/notify/start.rs` line 33, built
   at line 209) gets `remind_ms` from it, and `set_event` (line 232)
   takes it and writes it.
3. `EventPayload::NotifySet` (`event.rs` line 2285) gets
   `#[serde(default, skip_serializing_if = "Vec::is_empty")] remind_ms:
   Vec<u64>`, with the doc line "The reminder cadence in force (active
   run milliseconds), from the machine's `policy.yaml`; an older apb
   ignores it and sends no reminder; reminders are best effort across
   binary versions." (D9 "An older apb" below.) Every literal construction of
   `NotifySet` gets `remind_ms` (the compiler names them; on main the
   constructions are in `notify/start.rs` and `notify/mod.rs`, the
   others are patterns with `..`). This is the one journal field of this
   change; the park adds none (Goals, non-goal 1).
4. `Active` (`crates/apb-engine/src/notify/mod.rs` line 45) gets
   `remind_ms: Vec<u64>`, read in `NotifyState::fold` (line 96) from the
   latest `notify_set`; `follow_parent` (line 178) keeps the parent's
   cadence (a child's reminders follow the parent's subscription) as it
   is in force now: `NotifyState::in_force` follows the whole parent
   chain (a parent that inherited its own subscription reads its own
   parent's, at most `MAX_SUBPLAYBOOK_DEPTH` hops), and the sweep's
   early check `may_be_due` reads that same subscription in force, never
   only the copy the child journaled at its start (review round 2, M1
   and the own sweep). A
   person's live change (`notify::set`, `notify/start.rs` line 272)
   journals the cadence in force at the change.

### Which waits, and when

Reminded transitions: `Transition::Review` (a `human_review`, a
`publish` consent, and a list pilot gate once before-running-alone
lands: every review request is one), `Transition::Question`,
`Transition::HostTask`, each only when the subscription covers it. Never
`paused`, `finished`, `supervisor`, `stalled`, `follow_up` or `health`.

A park is not a wait on a person, so it is never reminded: it is a
`run_paused` by the engine, which is not a reminded kind, and the
paused transition of a park does not exist at all (Surfaces row 8). A
park also never coexists with a reminded wait: D1 parks only a timer
wait with no other frontier node, and an open review, question or host
task is a frontier node (a question and a host task belong to an
attempt that is still running). So the clock exemption of D3, which
counts a park as active time, never lengthens or shortens a reminder.

`TransitionId` (`crates/apb-engine/src/notify/transitions.rs` line 41,
`Copy`) gets `pub reminder: u8` (0 for the notice itself). `new` (line
47) sets 0; `id()` (line 52) appends `.r{n}` when `reminder > 0`
(`review.12.r2`), so `label()` (line 57) is `notify:review.12.r2`.

`due` (line 257) adds, after the notices it returns today and with the
same `may_try` rule per label:

1. For every open transition `t` of a reminded kind, covered and with
   `t.seq >= active.since_seq`:
2. Skip unless the notice `t.label()` has a try whose effective state is
   `Confirmed` and that was sent to the target in force now ("The
   notice's target" below): a reminder never goes before the notice it follows, and
   never to a target that notice did not reach.
3. `waited = crate::pause::active_run_ms(events, opened_ts, now, now)`
   (`pause.rs` line 231), `opened_ts` the `ts` of the event at
   `t.seq`: paused time and time after the run ended do not count.
4. `n` = how many entries of `active.remind_ms` are `<= waited`; skip
   when 0. Reminder `n` is the latest due threshold's, and the only one
   this sweep may send: a reminder `m < n` is never sent once `n` is
   due, so a driver that was down for a day sends one reminder, not
   three.
5. Reminder `n` is due when `may_try` (line 309) over the tries of its
   own label `notify:<kind>.<seq>.r<n>`, filtered as the notice's are
   (`is_after_set` or not `retired`), allows it. Nothing else is
   checked: a first try that failed or was refused for a passing reason
   is tried again after the retry gap, up to `MAX_TRIES`, while `n` is
   still the latest due threshold; once a later threshold is due, its
   own label starts with no tries. (Revision 6 had a step here that
   skipped the label when any reminder `m >= n` had a try at all; it
   made a failed first try final and is removed, design review 6,
   finding 1.)

The steps 2 and 5 are one function, `reminder_may_send(ledger, active,
t, n, now_ms, window, gap_ms) -> bool` (it reads only the ledger; the
journal parameter it once had was unused and is removed) in
`notify/transitions.rs`. Steps 4 and 5 together are `due_reminder(events,
active, t, now_ms, window, gap_ms) -> Option<u8>`: the latest due
threshold `n` of the cadence in force, when `reminder_may_send` allows
it. `due` calls it, and the ordering boundary below calls it again under
the journal lock, so the sweep and the admission decide by one rule.

A reminder belongs to its request: a re-asked request restarts its
reminders. A scope re-ask on a form gate (people-in-the-loop) withdraws
the request and opens a new one with a new seq, and a supervisor
directive that withdraws a gate's request makes the gate ask again with
a new seq too: the new request gets its own notice, and its reminders
count from it; the withdrawn request is closed, so none of its
reminders is sent. A reminder's text never repeats the scope; it points
to the request the same way the notice does.

A wait that closed is not open (`open_transitions`, line 73, already
drops a decided or withdrawn review, an answered question, a submitted
host task and everything at the run's end), so `due` computes no reminder
for it. That is not enough on its own (before-running-alone review round
1, confirmed on main): `Notifier::sweep_within` (`notify/send.rs`)
computes `due` over the event slice it read, then calls the connector for
each due label through `connector::call::execute_with_within` with that
same slice; a decision, a stop or a withdrawal another process journals
in between is not seen.

#### The notice's target

`NotifyState::fold` (`notify/mod.rs` line 96) keeps `since_seq` across
a change of an active subscription (line 118), so a request opened
before a target change stays due for this subscription. Its notice went
to the old target; a reminder to the new one would remind someone who
was never told. The rule: a reminder goes only to the target the
request's notice was confirmed to, with the same pins.

1. Every notification's `send_intent_recorded` carries `target_digest`,
   the digest of the whole target it was sent to
   (`NotifyTargetRecord::digest`: connector, account, function, `to`,
   `connector_digest`, `account_digest`, each framed by its length), an
   additive `serde(default)` field an older apb ignores. The sweep
   hands it to the call (`CallContext::notify`), and the send ledger's
   admission writes it under the journal lock with the intent, so it
   names the target the request actually went to, whatever changed
   after the sweep read the journal.
2. A notice try counts for step 2 above only when it is `Confirmed` and
   its `target_digest` equals the digest of the target in force now
   (`names_current_target` in `notify/transitions.rs`). No time is
   compared, so a parent's change seen by a child (`follow_parent`
   replaces the child's target with the parent's current one) is judged
   by the target alone, across journals.
3. A notice an older apb recorded carries no digest. It counts only on
   the run's own subscription (not one inherited from a parent), when
   its `IntentKey` names the current target's connector, function and
   account and it was recorded after the `notify_set` that put the
   current target in force (`Active.target_set_seq`: the fold keeps it
   from the previous active subscription when the target, compared over
   all six fields, is unchanged, and sets it to the current event
   otherwise). So it gets no reminder after any target change.

So after a target change from A to B, a request whose notice went to A
gets no reminder at B for as long as it stays open; the next request is
noticed at B (its seq is after `since_seq`, and the notice is due as on
main) and reminded at B. A change of only `to` or of the pins is a
change of target. A change back to A reminds the request noticed at A
again at A, the exact target its notice reached. A notice admitted
after a target change that its sweep did not see records the target it
went to, so it is never read as sent to the new one.

### The ordering boundary: every reminder intent is admitted under the journal lock

A notify target always declares `idempotency` (`notify::start::check`
refuses one that does not, `notify/start.rs` line 203), so every
notification that goes out passes the send ledger's admission,
`connector::call::send_once::decide`, which runs under the journal lock
(`EventLog::append_decided_synced`, `event.rs` line 2918) over the journal
as it is under that lock and records `send_intent_recorded` there before
the request leaves. Besides it, `Notifier::send` records a try that
never left through `Notifier::record_unsent` (`notify/send.rs` line
292), called on two early errors (a live target that does not resolve,
line 220; args the function no longer names, line 236) and after a
refused call (line 286); it appends its `send_intent_recorded` with a
plain `log.append`. A wait can close after the sweep's `due` read and
before any of these appends (before-running-alone review round 2, major
2, confirmed on main). Two sweeps can also both reach `record_unsent`
for the same reminder (both find the target unresolved) and append two
`not_sent` intents; an openness check alone passes both (design review
6, finding 2, confirmed: `record_unsent`, line 292, appends with no
look at the ledger). The rule is one: no intent of a reminder, whatever
its settlement, is appended unless, on the journal as it is under the
lock that appends it, the wait it follows is still open, its notice's
target is still the target in force, the reminder's own label is
still admitted (`may_try` over its tries), and the due reminder
computed again from that journal under the subscription in force is
the one the sweep selected, under the same target and cadence (review
round 2, M2: a person who turned reminders off, changed the cadence or
the target between the selection and the admission gets no intent).
All of these are one predicate,
evaluated under the same journal lock, on every path that appends a
reminder intent: the send path, the unresolved target, the args the
function no longer names, and the refused call.

1. `CallContext` (`crates/apb-engine/src/connector/call/request.rs`
   line 41) gets `pub(crate) still_due: Option<&'a dyn Fn(&[Event]) ->
   bool>`; `CallContext::notify` (line 84) takes it as a third argument.
2. In `decide` (`connector/call/send_once.rs` line 188), inside the
   `append_decided_synced` closure, right after `ledger::admit`: when the
   admission is `Admission::New { .. }` and `still_due` is set and
   returns `false` over the locked `events`, nothing is recorded (the
   closure returns `None`, as for a spent budget) and the call ends with
   the existing code `CallErrorCode::AttemptClosed` (`connector/result.rs`
   line 37: what the call belongs to has ended, so it may not send
   anything; no new code, so no other match changes) with the message
   `this reminder is no longer due (its wait closed, its target or
   cadence changed, reminders were turned off, or another sweep took it);
   nothing was sent`. A `Replay`, a `Reconcile` or an `AlreadySent` of an intent
   admitted earlier is not rechecked: once admitted the send may already
   be out.
3. `Notifier::record_unsent` gets a parameter `still_due: Option<&dyn
   Fn(&[Event]) -> bool>`. With `Some`, it appends the intent through
   `EventLog::append_decided_synced` instead of `append`: the closure
   returns `None` (nothing is appended, and the settlement is skipped)
   when `still_due` is false over the locked events, else the intent's
   payload. With `None` (a notice) it appends as on main.
4. `Notifier::send` builds one predicate for a reminder only
   (`t.reminder > 0`), over the locked `events`:
   `transitions::reminder_open(events, &active, t)` holds (the
   transition `t.kind` at `t.seq` is still open, with the filters `due`
   uses: `t.seq >= active.since_seq`, a reminded kind, and a kind
   `active.on` still covers, so a person's change that drops `review`
   after the sweep's read stops the reminder, test
   `notify_remind_test::a_reminder_of_a_kind_no_longer_covered_is_not_admitted`),
   the target and the cadence in force equal the ones the sweep selected
   under, and `due_reminder(events, &active, t, now, window, gap_ms)`
   (D9 "Which waits, and when") answers `Some(n)`, the reminder the
   sweep selected, where `active` is `NotifyState::in_force(run_dir,
   events)` over the locked events (for an inherited subscription the
   parent chain's journals are read then, as the sweep reads them),
   `now` is the clock read again there (never earlier than the sweep's
   `now_ms`: a threshold passed since the selection makes a later
   reminder the due one, and the next sweep sends that), and `window`
   and `gap_ms` are the values the sweep's `due` used. Tests
   `two_sweeps_send_one_reminder` (reminders turned off),
   `a_cadence_changed_before_admission_records_no_intent` and
   `a_target_changed_before_admission_records_no_intent`. It passes it to
   `CallContext::notify` and to every `record_unsent` call of that send:
   the unresolved target (line 220) and the args the function no longer
   names (line 236) included, not only the refused call (line 286). A
   notice (`reminder == 0`) passes `None` everywhere and is unchanged:
   it reports a transition that happened, open or not, as on main.
5. A call refused by item 2 records nothing in the ledger, and
   `Notifier::send` treats it like a send that is no longer due: on
   `CallErrorCode::AttemptClosed` for a reminder it returns at once, no
   `record_unsent`, no retry. The next sweep computes the reminder again
   from the journal: none for a closed wait or a changed target, and for
   a label another sweep took, whatever `may_try` allows after that
   sweep's try. So two sweeps that race on the same reminder, on the
   send path or on the unresolved-target path, record exactly one intent
   for its label.

So: a reminder whose intent was admitted before the wait closed may still
complete; its ledger entry carries its label `notify:<kind>.<seq>.r<n>`,
which names the request it was sent for, and the surfaces list it under
that request. A reminder whose admission comes after the wait closed is
never sent and never recorded, `not_sent` included, whatever slice its
sweep computed `due` over. No new waker: the driver keeps polling while a
review, question or host task is open (the review gate and publish poll
sleeps, `scheduler.rs` lines 2591 and 2638; the question and host task
waits run the throttled sweep), and its sweep
(`Notifier::sweep_throttled`, `notify/send.rs` line 199) calls
`may_be_due` (line 144), which calls `due` and needs no change (the
target and admission rules live inside `due`, so its three callers,
`sweep_within`, `may_be_due` and `retry_ahead`, keep their signatures). Only a
delayed retry releases the driver on main (`scheduler/deferred.rs`), and
a park leaves no reminded wait without a driver (above).

### The reminder's text

`Notice::compose` (`crates/apb-engine/src/notify/notice.rs` line 55): for
`t.reminder > 0` the subject is prefixed `Reminder: ` and one line is
inserted before the link: `Still waiting after {h} of active run time.`
with `{h}` = `apb_core::duration::format_duration_ms(entry)`
(`duration.rs` line 234: `4h`, `1h30m`), the due entry of the cadence in
force: `Notifier::send` passes `active.remind_ms[n - 1]`, the threshold
the sweep counted `n` over, to `Notice::build_with`, so a child that
follows its parent's cadence or a cadence changed since never names an
entry of another list (review round 1, m6; test
`notice::tests::a_reminder_names_the_threshold_it_was_counted_over`).
Nothing reads the cadence back from the run's journal, and there is no
"a while" fallback. The command line
comes from the same `command_for` (line 184), so a reminder of a request
that moved on prints none.

### Surfaces and docs

1. `notify::view` (`crates/apb-engine/src/notify/view.rs`, the label
   parse at line 77, `transition_of`) lists reminders as their own rows
   (`review.12.r1`).
2. `apb notify` (`crates/apb-cli/src/notify.rs`, the status print)
   prints `reminders: 1h, 4h, 24h` or `reminders: off` from
   `Policies::remind_ms`.
3. `docs/NOTIFY.md`: line 71 becomes "The same wait never sends its
   notice twice, also when the driver restarts or a resume reads the same
   journal again. With `notify.remind` in `policy.yaml` an open review,
   question or host task also gets one reminder after each listed
   duration of active run time, never before its notice went out, only
   to the target that notice reached, and never after it closed. A timer
   wait the run waits out paused (a park) is not a wait on a person and
   gets none." plus a section `## Reminders` after `## Setting it up`
   (line 81) with the key, the rules above (one reminder after an
   outage, a failed one tried again, none for an open request after its
   target changed), the project's `remind: []`, and that reminders are
   sent only while an apb of this version or later drives the run.
4. `docs/GUARDRAILS.md` "Older apb binaries" (the paragraph of task 5.4)
   gets two sentences: an apb older than this change reads a
   `policy.yaml` with `notify.remind` as invalid, and one that drives a
   run ignores `notify_set.remind_ms` and sends no reminder; reminders
   are best effort across binary versions and carry no run guard, and
   this apb sends the latest due one when it drives the run again.

### An older apb: reminders are best effort across binary versions

Decided in design review 6 (finding 4): reminders carry no run guard,
and the requirement is narrowed to say so. A reminder is sent only while
a run is driven (or swept) by an apb of this change or later; across
binary versions it is best effort. A guard would make an older apb
refuse to resume or pause every run with a reminder cadence, a cost out
of proportion to a missing reminder, which is never a wrong outcome: the
notice of the request went out, and the request stays open on every
surface.

What an older apb does, exactly:

1. It reads a `policy.yaml` with `notify.remind` as invalid (its strict
   reader denies unknown keys: it refuses launches through agent
   channels and creates no default notify subscription until updated),
   as for every earlier key of that file.
2. It decodes `notify_set` with `remind_ms` as before:
   `EventPayload` is internally tagged without `deny_unknown_fields`
   (`event.rs` line 139 to 141, `NotifySet` at 2285), so the unknown
   field is dropped, its `Active` has no cadence, and its `due` returns
   notices only. While it drives the run, an open review, question or
   host task gets its notice (once, under the notice's label, as on
   main) and no reminder.
3. A reminder intent this apb recorded earlier is a ledger entry under a
   label it does not make. Its `notify::view` lists it as its own row
   (`transition_of` strips only the prefix, so the row reads
   `review.12.r1`), and its sweeps never retry it.
4. A person's live change of the subscription made with the older apb
   journals a `notify_set` without `remind_ms`; from that event on the
   run's cadence is empty and this apb sends no reminder for it either,
   until a change made with this apb journals a cadence again.
5. When this apb drives the run again, `due` computes the reminders
   afresh from the journal: the latest due threshold of each open
   request is sent once, as after any outage (step 4 of "Which waits,
   and when").

### Wrong implementations of the reminders

1. Scheduling by wall clock from `review_requested`. Caught by
   `notify_remind_test::a_pause_in_the_middle_moves_the_reminder` (a
   1 h reminder over a 2 h open gate that was paused 90 min is not due;
   its positive control, the same journal one wall hour earlier, is
   reminded).
2. A reminder after the decision. Caught by
   `notify_remind_test::a_decided_gate_sends_no_reminder` (decided at
   59 min of a 1 h reminder: nothing due at 60 min; its positive
   control, the same gate left open, is reminded).
3. Sending every due reminder after an outage. Caught by
   `transitions::tests::only_the_latest_due_reminder_is_sent`.
4. Reusing the notice's label. Caught by
   `transitions::tests::a_reminder_has_its_own_label` (the label is
   `notify:review.<seq>.r1` and the ledger shows two intents).
5. A reminder before the notice went out. Caught by
   `transitions::tests::no_reminder_before_the_notice_is_confirmed`.
6. Letting a project file set a cadence. Caught by
   `policy_file_test::a_project_may_only_turn_reminders_off`
   (`crates/apb-core/tests/suite/policy_file_test.rs`).
7. Trusting the slice a sweep read. Caught by
   `notify_remind_test::a_reminder_due_on_a_stale_journal_is_not_admitted`
   (moved from a unit test in review round 1, so the target resolves and
   the send path's recheck in `send_once::decide` is what refuses: the
   journal is read with the gate open and the reminder due;
   `review_decided` is then appended; a sweep over the stale slice
   records no `send_intent_recorded` for `notify:review.<seq>.r1` and the
   stub service receives no request; it fails with the recheck removed)
   and by
   `notify_remind_test::a_reminder_admitted_before_the_decision_completes`
   (the decision lands while the reminder's request is going out: the
   reminder settles `confirmed` under its label, and the next sweep
   sends nothing).
8. Recording a `not_sent` reminder for a closed wait (review round 2,
   major 2 of before-running-alone). Caught in its two variants, each
   decided after the sweep's read and before the append, with no
   `send_intent_recorded` and no `send_intent_settled` for
   `notify:review.<seq>.r1`: the unresolvable live target (the `Err(why)`
   path), `notify::send::tests::a_closed_wait_gets_no_unsent_reminder`;
   the function that no longer names the args a notification fills,
   `notify_remind_test::a_closed_wait_gets_no_unsent_reminder_when_the_args_are_gone`.
9. Sending or recording twice when two sweeps race (the drive's
   throttled sweep and `apb resume --all`'s one sweep). Two `Notifier`s
   over the same run sweep at once, released by one barrier, with a
   retry gap longer than the test: (a)
   `notify_remind_test::two_sweeps_of_an_unchanged_journal_send_one_reminder`,
   a resolving target: one `send_intent_recorded` for the label and one
   request, the other call answered from the ledger or refused by the
   locked predicate (it may pass through the send ledger alone), and
   `notify_remind_test::two_sweeps_send_one_reminder`, the same race over
   a slice read before a person turned the reminders off: no intent and
   no request, which fails with the locked recheck removed (review round
   2, m1); (b)
   `notify_remind_test::two_sweeps_record_one_unsent_reminder`, a live
   target whose pinned account digest no longer matches (the `Err(why)`
   path): exactly one `send_intent_recorded` and one
   `send_intent_settled` `not_sent` for the label, never two (design
   review 6, finding 2; it fails with the locked recheck of
   `record_unsent` removed). The integration tests reach `Notifier`
   through `notify::Notifier`, a re-export that exists only under
   `cfg(any(test, feature = "testing"))`; `notify::send` stays
   crate-private (review round 1, m7).
10. Losing or repeating a reminder on a crash. Caught by
    `notify_remind_test::a_crash_after_the_intent_does_not_resend`: a
    journal cut after a reminder's `send_intent_recorded`, before its
    settlement: the next sweep reconciles or refuses it through the
    ledger (`unknown_effect` for `idempotency: none`, a marker lookup for
    `marker`) and never sends a second request for the label.
11. Keeping the reminders of a withdrawn request. Caught by
    `notify_remind_test::a_reask_restarts_the_reminders` (a request
    withdrawn at 50 min under `remind: [1h]` and asked again: no reminder
    of the first is sent, and the new request's is due one hour of
    active run time after it).
12. Reminding a park. Caught by
    `transitions::tests::a_park_is_never_reminded` (a journal parked on a
    timer wait for two hours under `remind: [1h]`: nothing is due).
13. Making a failed first try of a reminder final (the skip of revision
    6, design review 6 finding 1). Caught by
    `transitions::tests::a_failed_reminder_is_tried_again`: under
    `remind: [1h, 4h]`, the notice confirmed, `.r1` tried once at 61 min
    and settled `not_sent` from `refused`; at 61 min plus the retry gap
    `.r1` is due again; with `MAX_TRIES` tries of `.r1` spent, nothing is
    due; at 4 h `.r2` is due with no tries of its own.
14. Reminding a target the notice never reached (design review 6,
    finding 3). Caught by
    `transitions::tests::no_reminder_to_a_target_the_notice_missed` (the
    notice confirmed while A was in force, a `notify_set` to B, then 1 h
    of active time under `remind: [1h]`: nothing due; the same with a
    `notify_set` that only narrows `on`: `.r1` due) and by
    `notify_remind_test::a_target_change_moves_reminders_to_the_next_request`:
    review 1 noticed at A, the target changed to B, 1 h of active time:
    no reminder at A or B; review 1 decided, review 2 requested: its
    notice goes to B and, 1 h later, its `.r1` goes to B; the connector
    stand-in for A received exactly one request.

## Surfaces

The run's journal status is `paused` throughout. Each surface reads
`PauseInfo.cause == Some(Wait)`:

| # | Surface | Decided behaviour for a park | Where |
|---|---|---|---|
| 1 | `apb runs` list, `apb runs <id>`, `apb wait` | `PauseInfo::line` for cause `wait`: ``waiting on timer `<node>` until <T>; a waker resumes the run then`` plus `, lock kept` when kept; `word()` stays `paused` | `pause/display.rs` 42; the CLI prints it through `apb-cli/src/pause.rs` 87 `marker` |
| 2 | MCP `run_status`, `runs_list`, HTTP run detail and list | the `pause` object as today, with `cause: "wait"`, `reason: "wait (<node>)"`, `until`, `keep_lock`; nothing new | `pause.rs` 145 `pause_info` |
| 3 | MCP `run_wait` | returns `Stopped` as for every paused run (`run_wait.rs` 118); its instruction for cause `wait`: "the run waits on a timer (see pause); no driver runs, its waker resumes it at pause.until. Call run_wait again after that time; run_stop ends the run" | `apb-mcp/src/tools/run.rs` 830, a branch before it |
| 4 | the HTML report | a header meta line `pause: <PauseInfo::line>` for any paused run (none is shown today), through the run redactor like every text of the page (a pause reason is a person's or a supervisor's text) | `report_html/model.rs` (a `pause: Option<String>` field next to `deadline`, 54), `report_html/sections.rs` 163 |
| 5 | the dashboard | `pauseLine` gives the same wait line; `pauseCauseLabel` gives "on a timer wait"; the buttons are main's (`canPause` false for a paused run, Resume offered) | `web/src/lib/pause.ts` 27, 53 |
| 6 | next actions | the resume action's line reads ``the run waits on timer `<node>` until <T>; a waker resumes it then (a resume before then only parks it again)``; `auto` stays `pause_until(until)` (`next_action/verdict.rs` 657), so `apb wait --follow-auto` follows it; once `until` is more than `RELEASE_GRACE_MS` (`pause.rs` 296) past and no waker holds the lease `WakerFor::Pause(seq)`, the line reads ``the wait on timer `<node>` was due at <T> and no waker resumed it; resume it``. `Clock.paused` (76) is false for a park, so the wait's remaining time is shown running | `next_action/verdict.rs` 76, 625 to 665 |
| 7 | next actions, `unattended` | a park resumes at its `until` like a quota pause: `Proceeds { ResumeAttempt }` (an unattended run never parks; the field describes the wait) | `next_action/unattended.rs` 120 |
| 8 | notify | a park is no `paused` transition: the `RunPaused` arm sets `paused = None` for an engine pause with cause `wait`. The sweep before `after_pause` (trace step 7) therefore sends nothing for it, and no reminder either (D9: a park is not a wait on a person, and no reminded wait is open while a run is parked) | `notify/transitions.rs` 129 to 133 |
| 9 | health rules | a parked run has not ended, so it has no class; rules read ended runs only. Unchanged | `apb-core/src/health.rs` 106 `class_of` |
| 10 | a second write run of the tree | refused with main's message for a parked lock ("the paused run `<id>` keeps the workdir locked (paused with --keep-lock); resume or stop it first"). Unchanged; `apb runs <id>` names the wait | `workdir.rs` 315 `busy_reason` |
| 11 | `apb doctor` | lists the parked lock (`paused_locks`, `workdir.rs` 130) and, with `--run`, the `wait_park` guard among the journal's guards. Unchanged | |
| 12 | `apb stats` interventions, the retro | a park and the engine's resume of it count as no intervention and no engine action; a person's or a supervisor's resume of a park counts as theirs (D3 "The intervention counts") | `run_stats/interventions.rs` 449 to 455 |

## Unchanged from main

Once applied, a park is an engine pause with `until` and, while the
drive held a busy lock, `keep_lock`. Its behaviour in each case below is
main's for an operator pause with `--keep-lock` and `--until` applied by
a live driver, except where D6 (how the scan applies or discards a park)
and D7 (the waker's check on each poll) say otherwise for a park, and
with every control entry this apb posts numbered by D8. The
limits in this list are documented limits of every pause, not goals of
this change.

1. Application. Every entry other than a park is applied as on main:
   in sequence order, ending at the first Abort or Pause
   (`scheduler/control_apply.rs` 68 to 86). A park is applied or
   discarded by D6.
2. The exit window of the lock. `driver.pid` goes when `drive_inner` returns (trace
   step 6), and the lock is parked afterwards in `after_pause` (step 8).
   A crash between leaves a paused journal with `keep_lock` and a lock
   naming a dead pid, which the next acquire takes as stale
   (`workdir.rs` 315, 358).
3. A stop posted in the driver's exit window. `stop_run_with`
   (`stop.rs` 171) takes the event lock best effort (194 to 196), posts
   its Abort (214) and, while the driver is still alive, returns
   `SignaledLiveDriver` (231 to 237). An Abort the exiting driver no
   longer scans stays pending. After a park, the park's waker resumes the
   run at its first poll (D7) and the resumed drive's first scan applies
   it. When
   no waker starts (Unchanged 6) or its resume is refused, it is applied
   as on main: by `apb resume --all`, a person's resume, or a person's
   stop, which finalizes a run nothing drives (255 to 273).
4. A stop of a parked run nothing drives: `run_aborted` journaled at
   once without moving the cursor (the replay option of `control.rs` 164
   to 191; the next drive's Abort arm replays the entry once and moves
   the cursor), then `run_end::record` and `release_paused` (`stop.rs`
   269 to 271).
5. A person's pause during the park: journaled at once by
   `request_pause` (`pause/request.rs` 55; the run is paused, so nothing
   is posted, 81 to 90); the lock follows the person's own `--keep-lock`
   (`sync_lock` 100, which frees the lock before the append without it,
   120 to 123); the park's waker sees another `run_paused` and exits
   (`pause/waker.rs` 60); a new waker for a new `until` (103 to 105).
6. The waker. Only the `apb` binary spawns it and a failed spawn is a
   warning (`waker.rs` 171 to 181). A waker that failed to spawn, or a
   reboot, leaves the run paused until `apb resume --all`, which resumes
   a due pause and re-arms one ahead (`pause/resume_all.rs` 283 to 304,
   355 to 403). A resume the gate refuses at `until` leaves the run
   paused with `until` past (`pause/waker.rs` 248 `resume_while`);
   next actions say so for a park (Surfaces 6). The same holds for the
   resume of D7: when it is refused, that waker ends, the pending entries
   stay pending as for any paused run on main, and the run waits for
   `apb resume --all` or a person. Without a living waker no poll runs,
   so an entry posted while the run is parked waits the same way.
7. A failed resume commit parks a kept lock again (`scheduler/resume.rs`
   980 to 1004 `give_lock_back`).
8. The run deadline counts the park as an engine pause; the pause waker
   ends the run past the deadline at `until` (`pause/waker.rs` 159), the
   deadline waker in between (323).
9. A supervisor or a person's `apb note`, patch or rebind: a park is
   posted only with no control pending (condition 3), in a run no
   supervisor watches (condition 4). One appended before the park is
   applied first and the park is discarded (D6); one appended in the exit
   window or at any time while the run is parked wakes the run within
   one waker poll (D7), and the resumed drive applies it.
10. Concurrency slots: the run stays the slot's active holder while not
    terminal (`concurrency.rs` 220 to 235); `on_busy: supersede` stops it
    through `stop_run_with`.
11. The budget: a wait spends nothing and a park is no budget pause, so
    a plain resume is never refused `budget_paused`
    (`apb-mcp/src/tools/budget.rs` 307).
12. Binaries of different versions on one live run. Two binaries of
    different versions operating on one live run at once is unsupported
    on main for every control: an older apb's `post_control` numbers
    without the lock of D8, so its entry posted while a drive of this
    apb runs can share a number with another and be skipped, as two
    writers can on main today (D8 "Writers of an older apb"). The order
    D8 gives holds among writers of this apb and later only.

## Crash table

What the change adds is a decision and one `post_control`, the scan's
discard of a park (one cursor write, alone or as the cursor write of the
entry the pass ended at) and its application under the event lock, the
waker's check on each poll (a read only), and the control lock around
the count and the append (D8); the guard is written by an existing
path. Every other row after the post is a row of an engine
pause applied by the scan and is covered by "Unchanged from main".

| ID | Write | State after a crash right after it | What recovers it | Test |
|---|---|---|---|---|
| P1 | none: the decision of D1 was made, nothing posted | journal running with the wait open, `driver.pid` names a dead process, the busy lock names a dead pid | main's driverless run: `apb resume --all` resumes it, its driver takes the stale lock, the wait branch decides again | none new (main's driverless resume) |
| P2 | `control.jsonl` += the park's `Pause` (`post_control`) | as P1, plus the park's entry after the cursor | as P1; the resumed drive's first scan applies the entry when its `until` is still the wait's due time (D6 rule 3.1: no pause was journaled in between, and the time is not past): `run_paused` with the park's `until`, then trace steps 6 to 10; an `until` already past, or one a directly journaled pause made stale (D6 "What each interleaving does now" 9), is discarded and the wait branch decides afresh (the wait ends as due, or parks again with the new due time) | `a_pending_park_is_applied_by_the_next_drive` (state written directly) |
| D6a | the control cursor moved past a discarded park (`write_control_cursor`), nothing journaled | the park is gone; the run is running with the wait open and `driver.pid` names a dead process | as P1: the resumed drive applies what follows the park and the wait branch decides afresh | `a_patch_before_the_park_discards_it` (a discard), plus P1 |
| D6b | none: a crash before the discard's cursor write | the park's entry still pending, behind or before the entries that made the scan discard it | the next drive's first scan judges it again by D6 and discards it, since it is not the last entry or another entry is consumed first | `a_pause_after_the_park_is_applied_instead` (state written directly) |
| D6f | none new: P2's pending park, then a person's `apb pause` journals `run_paused` directly (no live driver, `pause/request.rs` 81 to 90), then a person's resume | the park still pending after the cursor, its `until` older than the wait's due time by the paused interval | the resumed drive's first scan recomputes the due time under the event lock (D6 rule 3.1), finds it later than `until` and discards the park; the wait branch parks again with the new due time | `a_park_pending_behind_a_direct_pause_is_discarded` (state written directly) |
| D6c | the event lock taken for a candidate park, nothing written under it yet | the lock directory names a dead holder; the park still pending | main's stale-lock rule (`fsutil.rs` 885 to 893: a lock older than 60 s is taken over); until then a person's pause refuses as for any held event lock, a stop goes through without it; the next scan judges the park again | none new (main's lock) |
| D6e | the cursor write of an exit of the pass (rule 5) that carries the park's `seq` in place of its own entry's | the consumed entry applied (its event journaled) and the park behind the cursor | nothing: one write covers both; a crash before it leaves both pending and the next scan applies the consumed entry again, the replay main's arm has today, and its exit discards the park in the same write | `a_resume_after_the_patch_limit_pause_applies_no_stale_park`, `a_pause_queued_before_the_park_leaves_no_stale_park` |
| C1 | `control.jsonl` += an entry under the control lock (D8), any writer | the OS lock is gone with the process (`flock`); the entry is whole, or torn when the crash cut the one write | a whole entry: as on main for that entry. A torn last line: every reader of the file fails to parse it, as on main after a crash in `post_control`; not changed here | none new (main's file) |
| D6d | `run_paused` of the park journaled under the lock, cursor not yet written | the Pause arm's own crash window on main: the next drive's scan meets the same entry again | the next scan judges it again: last and alone, it journals a second `run_paused` for the same request, the replay main's Pause arm has today (the paused interval keeps its start) | none new (main's Pause arm) |
| G1 | `run_guard` naming `wait_park` after `run_provenance` (`prepare.rs` 1644) | a crash before it: no guard in the journal | the run never parks (condition 5) and keeps its driver; an older apb may resume it, which is safe because it never parked | `a_run_without_the_guard_keeps_its_driver` |
| G2 | the guard after a checkpoint, the park's `run_paused` among them | one write with its checkpoint (`event.rs` 3069 to 3080): there is no state with one and not the other | nothing | none new |

## An older apb

A park is a `run_paused` with fields every apb since the operator pause
(issue #200) reads, followed in the same write by a `run_guard` naming
`wait_park` (D4). An apb that does not know `wait_park` reads that guard
as an unknown event (`event.rs` 3753 to 3770). Because the unknown event
follows the last checkpoint, that apb refuses to resume or pause the run
with its "written by a newer apb" message (`read_all`, 3766 to 3779:
a person's `apb resume`, its own pause waker, its `apb resume --all`),
still reads it on its read-only surfaces, and still stops it
(`read_all_for_stop`, 3796). A newer apb reading an older journal finds
no engine pause with the `wait (` prefix and no `wait_park` guard, so it
never takes an older run's pause for a park and never parks a run an
older apb started.

What the guard gives, exactly: an older apb refuses to resume or pause
a parked run while the park is the run's open state, that is while the
unknown `run_guard` follows the journal's last checkpoint. This is the
run guard contract of every guarded feature on main, not a barrier of
its own.

The limit. The older apb's stop goes through (`read_all_for_stop`) and
appends `run_aborted`, a checkpoint (`event.rs` 3795), without a guard
after it, because that apb does not know the feature. The unknown
`run_guard` is then followed by a checkpoint, so `read_all` of that apb
accepts the journal (3833), and a person's explicit resume with it
(`apb resume --from-node`, `scheduler/resume.rs` 112) drives the run.
That drive counts the park as paused time, so the timer waits again,
and under a run deadline the run may be ended where this apb would have
gone on (the example of D4). This is rejected as a goal (design review
5, finding 1): it needs a person to stop the run with an older apb and
then resume it with the same older apb, and closing it would need a
barrier that survives a checkpoint the older apb writes, which no
guarded feature has. A stop by this apb writes the guard after its
`run_aborted` (D4 3), so an older apb still refuses to resume a run this
apb stopped.

## Risks, each decided

1. The clock exemption. Decided: exempt park segments in `paused_fold`,
   the one fold every reader goes through, so the drive, next actions,
   progress, stats and estimates agree. Rejected: exempt only in
   `wait_active_ms`; the drive would end the wait on time while next
   actions, progress and stats froze it.
2. The run reads `paused` for hours. Decided: keep the journal status
   `paused`, give every surface the wait line through the cause, and
   suppress the notify transition.
3. A person's pause without `--keep-lock` over a park frees the tree
   (Unchanged 5). Decided: accepted; the person asked for it, and
   `apb pause --keep-lock` keeps it. Revision 1's inheritance rule (old
   D5) is what the second review found a crash window in.
4. Over-guarding (D4): accepted, as in revision 1.
5. A park is never posted twice at once: condition 3 keeps a second
   post out while the first is pending, the scan that applies it ends the
   drive, and the scan that discards it moves the cursor past it. A wait
   posts a new park after a discard only once the entries that caused the
   discard were consumed, so every repeat follows a control someone
   posted; nothing repeats on its own.
6. The scan takes the run's event lock for a candidate park (D6). Decided:
   the lock every pause and stop already decides under, held only for one
   re-read, one append and one cursor write, as `try_release` holds it in
   the same loop today. Rejected: deciding without the lock; a person's
   pause could then land between the re-read and the append and be
   journaled over an applied park by the driverless path while the
   driver still exits, which is correct but untestable as one outcome.
7. The waker's check on each poll (D7) resumes a run that may stop or
   park again at once (a pending stop ends it; a pending note leads to a
   new park). Decided: accepted; it is one resume per waker, and every
   resume follows an entry someone posted; an entry the drive does not
   consume keeps the driver in its live wait (condition 3), so there is
   no loop. The cost is one read of `control.cursor` and `control.jsonl`
   per poll (default 5 s) for the length of the park.
8. The control lock (D8) is new on every post, not only for a park.
   Decided: it is the fix of a defect of main, a leaf lock on the
   kernel's `flock` with no stale rule, held for one count and one
   append. Rejected: taking the event lock in `post_control`; three
   callers already hold it when they post (D8 "Lock order") and
   `lock_dir` would wait on itself. Rejected: numbering from a separate
   counter file; it is a second state file to keep in step with the
   control file, where the count of the lines under the lock is already
   exact.

## Review record

Every finding of the six design review rounds, with its final
disposition in one line: fixed (and where), removed (the machinery it
was found in is gone), rejected (and why), or pre-existing on main (not
changed here). Rows marked "own sweep" were found by the author between
rounds. This table is the review findings table of the change.

| Finding | Final disposition |
|---|---|
| d1.1 the "no process" goal is unmet | fixed: the goal is "no driver process and no agent", the pause waker every pause with `until` has is named (Goals 1); the CLI test asserts no driver and exactly one waker (Tests 18) |
| d1.2 the pending-stop fix writes a second abort | removed with the waker's pending-stop handling; a pending stop is applied as on main (Unchanged 3, 4) |
| d1.3 a stop pending after a crashed hand-over | removed: there is no hand-over; a stop in the exit window wakes the run at the waker's first poll (D7, Unchanged 3) |
| d1.4 a replacing pause frees the lock before its `run_paused` | removed with the inheritance rule; main's behaviour (Unchanged 5, Risks 3) |
| d1.5 an older apb waits the timer again | fixed: the run guard `wait_park` (D4), within the guard contract and its limit ("An older apb") |
| d1.6 the pending-control test has no deterministic outcome | fixed: the hooks are gone; condition 3 is a pure input of `park_request`, unit-tested (`a_pending_control_blocks_the_park`) |
| d2.1 the waker recurses on `NeedsDrive` | removed with the waker change of revision 1 |
| d2.2 W1 recovery can lose the tree lock | removed with `park_in_place`; the lock is parked by `after_pause` as for an operator pause (Unchanged 2) |
| d2.3 a pending stop stuck behind a resume refusal | removed with the waker change of revision 1; main's behaviour (Unchanged 3, 6) |
| d2.4 a pause replacing a park does not always leave a parked lock | removed with old D5 (Unchanged 5) |
| d2.5 the exactly-once abort is not crash safe | removed with `commit_terminal`; the scan's ordering is main's for every entry but a park (Unchanged 1, 4; D6) |
| d2.6 automatic wake without a successful waker hand-over | fixed: the wake guarantee names the waker, or `apb resume --all` when no waker lives (Goals; Unchanged 6) |
| d2.7 two tasks lack pass criteria | fixed: task 5.5 names its file; the release clippy criterion is "no error, and no new warning in a file this change touches" (task 6.9) |
| d3.1 a person's pause or stop sits behind the park | fixed in D6 (a park applies only as the last entry of a pass with nothing else consumed, decided again under the event lock, else discarded) and D7 (the waker wakes the run for an entry posted after the park was applied) |
| d3.2 a patch leaves a stale park | fixed in D6 rules 2 and 5; test `a_patch_before_the_park_discards_it` |
| d3.3 the reason grammar cannot name every node | fixed in D2 3: one form, `wait (<node>)`, the node verbatim between the fixed prefix and the final `)`; the node `a, b (c)` in the cause and clock tests |
| d3.4 `keep_lock` claims more than `lock.is_some()` | fixed: `keep_lock` is true exactly when the drive holds a busy lock (D1, spec); test `a_shared_workdir_wait_parks_without_the_lock` |
| d3.5 the process test asserts an intermediate state; the counts disagree | fixed: Tests 18 waits up to 10 s for the settled state; the counts are recounted (Tests "Counts") |
| d3 pre-existing: a crash before or after the control post leaves a driverless wait or a pending park | pre-existing on main (main's driverless resume); crash rows P1 and P2 |
| d3 pre-existing: a crash between `run_paused` and the cursor can replay that pause | pre-existing on main's Pause arm; crash row D6d |
| d3 pre-existing: `driver.pid` goes before the lock is parked and the waker spawned | pre-existing on main; Unchanged 2 and 6 |
| d3 pre-existing: a stopped parked run uses the abort replay rule; an early resume parks again; retry-now finds no deferral | pre-existing on main, or as designed; Unchanged 4, D5 1 and 4 |
| d3 pre-existing: a kept lock blocks a second write start; a replacing pause without `--keep-lock` releases it | pre-existing on main; Surfaces 10, Unchanged 5, Risks 3 |
| d4.1 two control writers can pick the same sequence, so a concurrent pause can be lost | fixed in D8 for every writer of this apb and later (`control.jsonl.lock`, a leaf below the event lock); unit test `two_concurrent_posts_get_distinct_increasing_numbers`; the two orders by tests 9 and 16 |
| d4.2 a control posted after the waker started waits for the timer | fixed in D7: the check runs on every poll of the waker's loop; test `a_note_posted_after_the_waker_settled_reaches_the_run`; Goals 6 states the bound |
| d4.3 a patch that pauses the run leaves a stale park | fixed in D6 rule 5; test `a_resume_after_the_patch_limit_pause_applies_no_stale_park` |
| d4 pre-existing: a crash after `run_paused` and before the cursor replays the pause | pre-existing on main; crash row D6d |
| d4 pre-existing: `driver.pid` goes before `after_pause` parks the lock | pre-existing on main; Unchanged 2 |
| d4 pre-existing: a failed waker spawn or a refused resume leaves a timed pause | pre-existing on main; Unchanged 6, the wake guarantee (Goals) |
| d5.1 an older apb can stop a parked run, then resume it from a node | rejected: existing guard contract (a stop goes through and `run_aborted` is a checkpoint, as for every guarded feature); recorded as a limit in "An older apb", Goals 5, proposal "Impact" 5.3 |
| d5.2 the control lock cannot order an older apb's post while a new drive runs | rejected: two binaries of different versions on one live run at once is unsupported on main for every control; recorded as a limit in D8 "Writers of an older apb" and Unchanged 12 |
| d5.3 a half-written control line can end the park's only waker | fixed in D7 "The pending read": a read error is retried at the next poll, bounded log, no exit; test 13 `a_half_written_control_line_does_not_end_the_waker` |
| d5.4 parks change the intervention statistics | fixed in D3 "The intervention counts" and Surfaces 12: a park and the engine's resume of it are skipped, a person's resume of a park stays the person's; unit test `a_park_and_its_automatic_resume_are_no_intervention` |
| d5.5 the race test does not prove both orders | fixed in Tests: the race claim is gone; tests 9 and 16 post in a fixed order (park then pause, pause then park); the unit race test proves distinct numbers only |
| d5 pre-existing: the four limits repeated from round 4 and round 3 | pre-existing on main; rows above (crash row D6d, Unchanged 2, 5, 6, Risks 3) |
| own sweep: a person's pause posted right before the park, after condition 3 | fixed in D6 rule 5 for the non-park Pause arm (and the Abort arm and the salvage); test `a_pause_queued_before_the_park_leaves_no_stale_park` |
| own sweep: a park left behind an unconsumable `Retry` applied at a later node | fixed in D6 rule 2.3; test `a_park_for_another_node_is_discarded` |
| own sweep: a pending park read as `pausing` | fixed: `pending_pause` skips a park (D6 "`pending_pause`"); test `a_pending_park_is_not_a_pending_pause` |
| moved in (revision 6): reminders for open waits from before-running-alone, with its review findings | D9, capability `wait-reminders`, tasks 4a |
| before-running-alone round 1 finding 10: a reminder sent after its wait closed | fixed: openness rechecked at send-intent admission under the journal lock (D9 "The ordering boundary", item 2); tests `a_reminder_due_on_a_stale_journal_is_not_admitted`, `a_reminder_admitted_before_the_decision_completes` |
| before-running-alone round 2 major 2: a closed wait can still gain a `not_sent` reminder intent through `record_unsent` | fixed: every reminder intent, `not_sent` included, is appended under the journal lock only while the wait is open (D9 "The ordering boundary", items 3 and 4); test `a_closed_wait_gets_no_unsent_reminder` |
| owner decision (revision 6): a re-asked request restarts its reminders | D9 "Which waits, and when"; test `a_reask_restarts_the_reminders` |
| owner decision (revision 6): how a reminder treats a park | a park is not a wait on a person: no reminder (D9 "Which waits, and when", Surfaces row 8); test `a_park_is_never_reminded` |
| d6.1 a failed first reminder is never retried | fixed: `due` selects the latest due threshold's label and applies `may_try` to it; the skip that made a failed first try final is removed (D9 "Which waits, and when" steps 4 and 5); unit test `a_failed_reminder_is_tried_again` |
| d6.2 two sweeps can record two `not_sent` intents for one reminder | fixed: one predicate (wait open, same target, the label's ledger admission by `may_try`) is evaluated under the same journal lock on every path that appends a reminder intent, the unresolved target included (D9 "The ordering boundary", items 2 to 5); `two_sweeps_send_one_reminder` gains the unresolved-target variant |
| d6.3 a target change can send a reminder to a target the notice never reached | fixed: a reminder goes only to the target the request's notice was confirmed to, with the same pins (the notice intent's `target_digest`); an open request gets no reminder at a target its notice did not reach, the next request is noticed and reminded at the new target (D9 "The notice's target"); tests `no_reminder_to_a_target_the_notice_missed`, `no_reminder_to_a_recipient_the_notice_did_not_reach`, `a_child_sends_no_reminder_after_its_parent_changed_the_recipient`, `a_target_change_moves_reminders_to_the_next_request` |
| d6.4 reminder compatibility conflicts with its requirement | resolved by narrowing: reminders are best effort across binary versions, no run guard; the requirement says so and D9 "An older apb" says what an older binary driving the run does (decodes `notify_set` without `remind_ms`, sends notices only, lists earlier reminder rows, never retries them) |
| d6.5 a pending park can survive a crash and override a later direct pause | fixed: before a pending park is applied, the wait's due time is computed again under the event lock on the current clock (`wait_park::due_at_ms` over `scheduler::journal::wait_active_ms`); a park whose `until` is not that value is discarded by D6 rule 4 (D6 rule 3.1, "What each interleaving does now" 9, crash row D6f); integration test 21 `a_park_pending_behind_a_direct_pause_is_discarded`, unit test `the_due_time_moves_only_with_paused_time` |
| d6 pre-existing: a crash after `driver.pid` removal and before `after_pause` parks the busy lock leaves a dead PID lock a new start can take | pre-existing on main, not in scope; Unchanged 2 |
| d6 pre-existing: a failed waker spawn or a refused resume leaves an ordinary timed pause | pre-existing on main, not in scope; Unchanged 6, the wake guarantee (Goals) |
| d6 pre-existing: a torn final control line prevents control readers from parsing the file | pre-existing on main, not in scope; crash row C1 |
| r1 B1 a timer wait on a node id that is not a safe segment parks forever | fixed in D1 condition 1: `park_request` refuses a node whose reason does not round-trip; the wait stays live; unit test `a_wait_on_an_unsafe_node_never_parks` |
| r1 M1 a failed event-lock take ends the drive | fixed in D6 rule 4: the scan discards the park; unit test `a_park_whose_event_lock_is_busy_is_discarded` |
| r1 M2 and audit m1 the control line and its newline in two writes | fixed in D8 item 2: one buffer, one `write_all` |
| r1 M3 test `a_pending_park_is_applied_by_the_next_drive` missing | added (Tests 6), also the positive control of the discard tests |
| r1 M4 and audit M4 the send-path recheck untested | fixed: the stale-journal test moved to `notify_remind_test` with a resolving target, the args variant and both race variants added (D9 "Wrong implementations of the reminders" 7 to 9) |
| r1 audit M1 the HTML report's pause line skips the run redactor | fixed: every `meta` line of the report header goes through the writer's redactor (Surfaces); test `the_pause_line_is_redacted` |
| r1 audit M2 a stop can fail on the control lock | fixed in D8 item 4: an Abort that cannot take the lock is appended without it, the only writer allowed to; unit test `a_stop_is_posted_while_the_numbering_lock_is_held` |
| r1 audit M3 the discard tests pass on the due-time check alone | fixed: they post the real due time (Tests 6) |
| r1 audit M5 timing assertions | fixed: the wake time and the frozen wait are compared with the journal's own timestamps; the stop watcher's positive half polls to a bound |
| r1 audit m2 a park with an unbounded `until`, a discarded park posted every poll | fixed in D1: no due time, no park; `ParkRetry` holds a new park after a discard until a change or `PARK_RETRY` |
| r1 audit m3 a parked run woken by entries re-parks as fast as they arrive | recorded as a limit in D7 "How often a parked run can be woken this way" |
| r1 m4 the admission's openness check skipped the filters `due` uses | fixed in D9 "The ordering boundary" item 4: `reminder_open` |
| r1 m5 an unused journal parameter of `reminder_may_send` | removed (D9 "Which waits, and when") |
| r1 m6 and audit m9 the reminder's duration read from the run's journal | fixed in D9 "The reminder's text": the sweep passes the threshold of the cadence in force |
| r1 m7 and audit m4 `notify::send` made public for tests | reverted to crate-private; a `testing`-only re-export `notify::Notifier` |
| r1 m8 the park predicate over a journaled pause repeated four times | one `pause::cause::is_park_event` |
| r1 m11 a refused reminder admission still reached `record_unsent` | fixed in D9 "The ordering boundary" item 5: an early return on `AttemptClosed` |
| r2 M1 a child's early due check read only the cadence copied into its journal | fixed in D9 item 4: `may_be_due` reads `NotifyState::in_force`; test `a_child_follows_the_cadence_its_parent_turned_on` |
| r2 M2 a reminder selected before a person turned reminders off could still be admitted | fixed in D9 "The ordering boundary" item 4: the due reminder is computed again under the lock and must equal the selected one, same target and cadence; tests `two_sweeps_send_one_reminder`, `a_cadence_changed_before_admission_records_no_intent`, `a_target_changed_before_admission_records_no_intent` |
| r2 m1 `two_sweeps_send_one_reminder` passed with the locked recheck removed | fixed: it races over a slice read before reminders were turned off and asserts no intent; the plain race is `two_sweeps_of_an_unchanged_journal_send_one_reminder` (Tests 9) |
| r2 m2 the process test's 3 second timer was shorter than its observation bound | fixed: the hand-over test uses a 120 second timer, the resume at `until` is its own test (Tests 18) |
| r2 own sweep: a grandchild followed its parent's journaled copy, not the subscription in force at the top of the chain | fixed in D9 item 4: `in_force` follows the parent chain, bounded by `MAX_SUBPLAYBOOK_DEPTH`; test `a_grandchild_follows_the_cadence_at_the_top_of_its_chain` |
| r2 own sweep: the admission judged the due threshold on the sweep's clock | fixed in D9 "The ordering boundary" item 4: the clock is read again under the lock |

## Tests

Integration, added to the existing
`crates/apb-engine/tests/suite/wait_test.rs`, with `APB_RELEASE_AFTER_MS`
(via `common::EnvVar`) and `APB_WAKER_POLL_MS` short, over a playbook
`start -> work (script) -> pause (wait, timer 3 s) -> finish` so the run
is a write run. A test binary spawns no waker and no detached driver
(`waker.rs` 171 to 181), so these tests call `run_waker` in a thread
where a waker would run, or resume in-process the way
`operator_pause_test.rs` resumes a paused write run; the end-to-end wake
with real processes is the CLI test.

1. `a_long_timer_wait_parks_and_wakes_when_due`: the drive returns
   `Paused`; the journal holds one `run_paused` with `reason: "wait
   (pause)"`, `by: engine`, `until`, `keep_lock: true`, a `run_guard`
   naming `wait_park` before it and one right after it; no `driver.pid`;
   the lock reads `paused:<run>`; the control cursor stands at the park's
   entry. After `until` (`run_waker` in a thread) the run is resumed by
   `engine` and ends `succeeded` with one `node_started` and one
   `wait_started` for the wait and `wait_signalled` no earlier than the
   timer's active length.
2. `a_short_wait_keeps_its_driver`, `a_webhook_wait_keeps_its_driver`,
   `an_unattended_run_keeps_its_driver`: no `run_paused`.
3. `a_run_without_the_guard_keeps_its_driver`: before the run reaches
   the wait (its script node holds on a file), the test appends
   `RunGuard { requires: [] }`; the wait does not park.
4. `an_early_resume_parks_again`: a resume of the parked run well before
   `until`; the journal gains `run_resumed` and a second park whose
   `until` is within one poll of the first.
5. `a_pause_during_the_park_freezes_the_wait`: `request_pause` (with
   `keep_lock: true`) on the parked run; the park's waker exits; a
   resume later parks again with the remaining active time.
6. `a_pending_park_is_applied_by_the_next_drive` (P2, state written
   directly: the wait open on a 30 s timer, the park's `Pause` after the
   cursor with `until` set to the wait's due time of the written
   journal, a dead `driver.pid`): the next drive's first scan journals
   the park's `run_paused` with that `until`, the cursor stands at its
   entry and no second park is posted. The test binary cannot re-exec
   `__drive-run`, so the next drive is the in-process `resume`, the drive
   `resume_all` would start. It is also the positive control of the
   discard tests 9 to 21: each of those posts its park with the same real
   due time (`due_now`), so the due-time rule alone no longer discards
   it and each fails when its own rule is removed (review round 1, audit
   M3). Where two rules guard one case (a park followed by a person's
   entry is neither the last entry nor alone under the lock), the test
   fails only with both removed.

7. `a_read_only_wait_parks_without_the_lock`: `start -> pause (wait,
   timer 3 s) -> finish`, no node that takes the workdir lock
   (`NodeKind::takes_workdir_lock`, the test `takes_workdir_lock` in
   `pause/request.rs` 135 makes), so the drive holds no busy lock; it
   parks with `keep_lock: false` and leaves no lock file.
8. `a_shared_workdir_wait_parks_without_the_lock`: the write playbook of
   test 1 started with `allow_shared_workdir: true`, so the drive takes
   no busy lock (`workdir.rs` 287 to 289); it parks with `keep_lock:
   false`, no lock file exists, and after `until` the waker resumes it to
   `succeeded`.
9. `a_pause_after_the_park_is_applied_instead` (D6, and the order "park
   `N`, pause `N + 1`" of "What each interleaving does now" 7; state
   written directly: the wait open, `driver.pid` dead, then two
   `post_control` calls made one after the other, the park with the
   wait's real due time first and an operator `Pause` with `keep_lock: true` and no
   `until` second; the test asserts they got `N` and `N + 1`):
   `resume_all` resumes the run; its
   first scan discards the park and journals the operator's
   `run_paused`; the journal holds no `run_paused` with reason `wait
   (pause)`; the cursor stands at the operator's entry. 2 s later the
   run is still paused by the operator and the wait's active time
   (`journal::wait_active_ms` over the journal) has not grown since the
   operator's `run_paused`: the timer does not run while a person
   paused. A resume then parks again for the remaining time.
10. `a_stop_after_the_park_is_applied_instead` (D6, state written
    directly as in 9 with an operator `Abort` in place of the `Pause`):
    `resume_all` resumes the run; it ends `aborted` with `run_aborted`
    by the operator and no `run_paused` with reason `wait (pause)`.
11. `a_patch_before_the_park_discards_it` (D6 rule 5, state written
    directly: the wait open, `driver.pid` dead, `control.jsonl` holding a
    `Patch` to a published version whose wait has a 3600 s timer and,
    right after it, the park of the old 3 s timer): `resume_all` resumes
    the run; the journal holds `patch_applied`, then exactly one
    `run_paused` with reason `wait (pause)` whose `until` follows the new
    3600 s timer (more than 3000 s ahead), and none with the old timer's
    `until`; the old park's entry is behind the cursor.
12. `a_control_posted_in_the_exit_window_wakes_the_run_at_once` (D7): the
    run of test 1 with a 30 s timer parks (the drive returns `Paused`);
    the test then appends an operator `Abort` with `post_control`, the
    entry a stop leaves when it found the driver still claiming the run,
    and calls `run_waker` for the park's `seq` in a thread. The waker
    returns `Resumed` within 5 s, well before `until`; the run ends
    `aborted` with `run_aborted` by the operator, journaled by the
    resumed drive's scan (the cursor stands at the `Abort`'s entry); one
    `run_resumed` by `engine` precedes it.
13. `a_half_written_control_line_does_not_end_the_waker` (D7, the
    pending read): the run of test 1 with a 30 s timer parks;
    `run_waker` for the park's `seq` runs in a thread with
    `APB_WAKER_POLL_MS=50`; once the waker's lease is held the test
    appends the first half of a `ContextAppend` line to `control.jsonl`
    with no newline, the state a reader sees while a writer is in the
    middle of its append. 300 ms later (several polls) the waker has not
    returned and its lease is still held; the test then appends the rest
    of the line and its newline. Within 5 s the waker returns `Resumed`;
    the journal holds `run_resumed` by `engine`, then the note's
    `supervisor_action` (`context_append`).
14. `a_note_posted_after_the_waker_settled_reaches_the_run` (D7): the run
    of test 1 with a 30 s timer parks; `run_waker` for the park's `seq`
    runs in a thread with `APB_WAKER_POLL_MS=50`; the test waits until
    the waker's lease is held and 300 ms more (several polls), then posts
    `ContextAppend` with `post_control`. Within 5 s the waker returns
    `Resumed`; the journal holds `run_resumed` by `engine`, then the
    note's `supervisor_action` (`context_append`), then a second park
    whose `until` is within one poll of the first park's `until` (the
    time left); the cursor stands at the second park's entry.
15. `a_resume_after_the_patch_limit_pause_applies_no_stale_park` (D6
    rule 5, state written directly: the wait open, `driver.pid` dead, the
    run's patch limit exhausted (the run's `max_patches_per_run` 0, or as
    many `patch_rejected` events in the journal as its limit), and in
    `control.jsonl` a `Patch` followed right after by the park of the 3 s
    timer with the wait's real due time as `until`): `resume_all` resumes the run; its
    first scan journals the engine's `max patches per run exhausted`
    pause and the cursor stands at the park's entry. The test waits 2 s
    (past the old park's `until`), then resumes the run as a person
    (`apb resume` path, in-process): the journal holds no `run_paused`
    with reason `wait (pause)` carrying the old `until`; the wait parks
    again with an `until` no earlier than the resume plus the wait's
    remaining active time (the patch-limit pause is paused time), or ends
    as due when that is under `release_after`.
16. `a_pause_queued_before_the_park_leaves_no_stale_park` (D6 rule 5,
    the non-park Pause arm, and the order "pause `N`, park `N + 1`" of
    "What each interleaving does now" 7; state written directly as in 9,
    the two `post_control` calls made one after the other with the
    operator `Pause` first and the park second, numbered `N` and
    `N + 1`): `resume_all`
    resumes the run; the operator's `run_paused` is journaled, no park,
    and the cursor stands at the park's entry. A person's resume 2 s
    later parks again for the remaining time, never with the old
    `until`.
17. `a_park_for_another_node_is_discarded` (D6 rule 2.3, state written
    directly: the wait `pause` open, `driver.pid` dead, `control.jsonl`
    holding one park whose reason is `wait (gone)`): `resume_all`
    resumes the run; no `run_paused` with reason `wait (gone)` is
    journaled, the cursor stands past that entry, and the wait parks
    with reason `wait (pause)`.

CLI integration, `crates/apb-cli/tests/suite/pause_waker_cli_test.rs`
(real `apb` processes):

18. `a_parked_wait_holds_no_driver_and_leaves_one_waker`: `apb run
    <wait playbook> --detach` with `APB_RELEASE_AFTER_MS=300`,
    `APB_WAKER_POLL_MS=50` and a 120 second timer, longer than the
    observation bound, so a correct waker is still waiting whenever the
    checks end (review round 2, m2). Once the park's
    `run_paused` is in the journal the hand-over is still going on (the
    driver removes its claim and runs the notify sweep before
    `after_pause` spawns the waker, trace steps 6 to 9), so the test
    polls every 50 ms, for at most 10 s, until the settled state holds:
    `driver.pid` is gone, no process whose command line holds
    `__drive-run` and the run id is alive (the check `liveness.rs` 308
    makes), and the waker's lease is held
    (`apb_engine::waker::Lease::held(run_dir, WakerFor::Pause(<park
    seq>))`). A timeout fails with the state seen last. Then it asserts:
    exactly one `apb __pause-waker` process for the run is alive (the
    expected waker) and nothing resumed the run; the test then ends that
    waker. `a_parked_wait_is_resumed_by_its_waker_at_until`, the timed
    half, starts the same playbook with a 3 second timer and asserts
    only that the run is resumed by `engine` no earlier than `until` and
    ends `succeeded`.

Journal corpus, `crates/apb-cli/tests/suite/journal_corpus_test.rs`
(real `apb` processes, the old binaries v0.25.0 and v1.0.0-beta.1; run
the CI way, `APB_CORPUS_REQUIRED=1`, tasks 6.12). Both rows are written
on the unchanged tree first and pass there (tasks 0.4); their literal
lists of event types are main's journal and never change afterwards:

19. `w1_a_run_without_a_timer_wait_journals_as_main`: a script node,
    then a webhook wait that times out after 1 s. A run that never
    parks writes the same journal as main: no `run_guard`, no
    `run_paused`, no `run_resumed`, no `control.jsonl` entry, and the
    same sequence of event types. Both old binaries read the run.
20. `w2_a_short_timer_wait_adds_only_its_guard`: the same with a 1 s
    timer (under `release_after`, never parks). The journal without its
    `run_guard` lines is main's; the guard lines name exactly
    `wait_park` and stand right after `run_provenance` and every
    checkpoint (D4, the accepted cost), and nowhere else.

Integration, `wait_test.rs` again (added in revision 7, numbered after
the others so the numbers cited elsewhere stay):

21. `a_park_pending_behind_a_direct_pause_is_discarded` (D6 rule 3.1,
    "What each interleaving does now" 9, crash row D6f; design review 6,
    finding 5; state written directly: the 3 s timer wait open,
    `driver.pid` dead, one park in `control.jsonl` whose `until` is
    `wait_park::due_at_ms` of the journal at that moment): the test then
    calls `request_pause` as a person, which finds no live driver and
    journals `run_paused` directly; 2 s later it resumes the run as a
    person (in-process). The journal holds no `run_paused` with the
    park's old `until`; the cursor stands past the park's entry; the
    wait parks again with an `until` at least 2 s later than the old one
    (the paused interval), equal to `due_at_ms` of the journal at the
    second park.

Unit:

1. `scheduler/wait_park.rs`: `left_ms_takes_the_earlier_of_timer_and_timeout`,
   `left_ms_saturates`, `the_due_time_moves_only_with_paused_time`
   (`due_at_ms` of one journal at two `now` values with no pause between
   is the same value; with a `run_paused` and `run_resumed` of 90 s
   between, the later value is 90 s later; `None` before
   `wait_started`), `a_long_timer_wait_asks_for_a_park` (the request:
   reason, `by`, `Drain`, `until`, `keep_lock`), and one test per refusing
   condition: `a_webhook_wait_never_parks`, `a_scenario_wait_never_parks`,
   `a_wait_beside_other_work_never_parks`, `a_pending_control_blocks_the_park`,
   `a_run_that_may_not_release_never_parks` (short waits and the modes
   are `may_release`'s, covered by integration test 2),
   `an_unguarded_run_never_parks`.
2. `pause/clock.rs` `mod tests` (new): `a_park_is_not_paused_time` (with
   a quota counter-case that stays paused time),
   `a_person_pause_over_a_park_counts_from_its_ts`,
   `a_park_counts_toward_the_deadline`.
3. `pause/cause.rs`: `a_wait_reason_is_a_wait_only_from_the_engine`,
   `parse_wait_reason_refuses_an_unsafe_node`,
   `parse_wait_reason_round_trips_any_valid_node` (`w`, `a, inbox`,
   `x (y)`, `a)`, `a, b (c)`; and `wait ()`, `wait (w` and `wait (w) x`
   read as nothing). The cause test and `a_park_is_not_paused_time` use
   the node `a, b (c)`.
4. `pause/display.rs`: `a_park_reads_waiting_on_timer`.
5. `notify/transitions.rs`: `the_notify_sweep_sends_no_paused_notice_for_a_park`.
6. `next_action/tests.rs`: `a_park_offers_the_resume_with_its_until`,
   `an_overdue_park_says_so`, `an_unattended_park_resumes_its_attempt`.
7. `report_html_test.rs` (integration, existing file):
   `the_report_shows_a_pause_line`.
8. `apb-mcp/src/tools/run.rs`: `run_wait_explains_a_park` (the
   instruction for a view whose pause has cause `wait`).
9. `apb-cli/src/pause.rs`: `a_park_resumed_early_says_it_waits_again`.
10. `web/src/lib/pause.test.ts`: `pauseLine names a timer wait`,
    `pauseCauseLabel names a timer wait`.
11. `apb-core/src/validate/guarded.rs`: `a_timer_wait_is_a_guarded_feature`
    (a timer wait names `wait_park`, a webhook wait does not); the count
    in `a_deadline_is_a_guarded_feature` becomes 13, and
    `all_has_twelve_entries_in_order` (which #289 left ending with
    `ListPilot`) becomes `all_has_thirteen_entries_in_order` with
    `ListPilot` twelfth and `WaitPark` last.
12. `stop.rs` `mod tests`: `a_park_does_not_halt_the_drive` (a
    `StopWatcher` over a control file holding a park leaves `halt` false
    after two watch intervals; the same with an operator `Pause` sets
    it).
13. `control.rs` `mod tests` (D8, D6 "`pending_pause`"):
    `two_concurrent_posts_get_distinct_increasing_numbers` (8 threads
    released by one `Barrier` post 25 entries each into one run
    directory; the numbers are exactly 0 to 199, each once, and the
    file's lines carry them in increasing order; a race, so with main's
    `post_control` it fails often but not on every run, and the lock is
    also checked in review; it proves distinct numbers only, never an
    order of two particular posts, which tests 9 and 16 fix by posting
    one after the other), `a_pending_park_is_not_a_pending_pause` (a park
    alone: `pending_pause` is `None`; a park then an operator `Pause`:
    the operator's).
14. `run_stats/interventions.rs` `mod tests`:
    `a_park_and_its_automatic_resume_are_no_intervention` (D3 "The
    intervention counts").

Counts: 24 integration tests (20 in `wait_test.rs`: items 1 to 17 and
21, item 2 being three tests; the report test, unit item 7; the CLI
test, item 18; the two corpus rows, items 19 and 20) and 30 unit tests
(items 1 to 6 and 8 to 14: 10, 3, 3, 1, 1, 3, 1, 1, 2, 1, 1, 2, 1).

The reminders of D9 have their own tests, named in D9 "Wrong
implementations of the reminders" and tasks section 4a: 9 unit tests in
`notify/transitions.rs` (`a_reminder_has_its_own_label`,
`no_reminder_before_the_notice_is_confirmed`,
`only_the_latest_due_reminder_is_sent`,
`a_failed_reminder_is_tried_again`,
`no_reminder_to_a_target_the_notice_missed`,
`paused_time_does_not_count_toward_a_reminder`,
`no_reminder_for_a_closed_wait`,
`only_reviews_questions_and_host_tasks_are_reminded`,
`a_park_is_never_reminded`), 1 in `notify/send.rs`
(`a_closed_wait_gets_no_unsent_reminder`), 1 in `notify/notice.rs`
(`a_reminder_names_the_threshold_it_was_counted_over`), 3 in
`policy_file_test.rs`, 15 integration tests in the new
`notify_remind_test.rs` (with
`a_target_change_moves_reminders_to_the_next_request`,
`a_reminder_due_on_a_stale_journal_is_not_admitted`,
`a_closed_wait_gets_no_unsent_reminder_when_the_args_are_gone`,
`a_reminder_of_a_kind_no_longer_covered_is_not_admitted`, and the two
variants of the race, `two_sweeps_send_one_reminder` and
`two_sweeps_record_one_unsent_reminder`) and 1 in `notify_cli_test.rs`.
Review round 1 added the unit tests
`wait_park::tests::a_wait_on_an_unsafe_node_never_parks`,
`wait_park::tests::a_discarded_park_is_posted_again_only_after_a_change_or_the_backoff`,
`control_apply::tests::a_park_whose_event_lock_is_busy_is_discarded` and
`control::tests::a_stop_is_posted_while_the_numbering_lock_is_held`, and
`report_html_test::the_pause_line_is_redacted`.

## Wrong implementations

1. Journaling `run_paused` from the wait branch instead of posting it.
   That is a second way to pause beside the scan, with its own lock and
   waker handling. Caught by test 1 (the control cursor stands at the
   park's entry) and in review: the branch has exactly the two lines of
   the budget gate.
2. Calling `engine_pause` instead of `post_engine_pause`. Harmless today
   (condition 1 excludes scenario runs), but it hides that a scenario
   never parks. Caught in review.
3. Parking the lock, spawning a waker or removing `driver.pid` in the
   wait branch. Caught by the code map: the change touches no lock or
   driver file, and the waker file only for D7's check on each poll.
4. Posting without condition 3. A park behind an entry the scan does not
   consume is posted at every poll. Caught by
   `a_pending_control_blocks_the_park`.
5. Deriving the wake time inside `park_request` from the timer. Breaks
   the inbox wait's reuse. Caught by the signature (`wake_at_ms` is
   computed by the caller).
6. Exempting every engine pause from the clock. A quota pause would no
   longer freeze node timers. Caught by the existing
   `a_paused_question_deadline_is_frozen_on_the_active_clock`
   (`next_action/deadlines.rs` 171) and the quota counter-case of
   `a_park_is_not_paused_time`.
7. Deciding the cause from `by` alone or from the reason alone. Caught by
   `a_wait_reason_is_a_wait_only_from_the_engine`.
8. Writing the guard at the first park. A patch drops it again. Caught by
   test 1 (the guard is in the journal before the park) and D4.
9. Applying a park because it is the first Pause of the pass (the Pause
   arm left as it is for a park). Caught by tests 9 and 10.
10. Journaling anything for a discarded park, or not moving the cursor
    past it. Caught by tests 9 and 11 (no event for it; the cursor past
    it).
11. Deciding a candidate park without the event lock or without the
    re-read under it. Not deterministic without a test hook (revision 2
    removed them); caught in review against D6 rule 3.
12. The waker reading, applying or moving a control entry itself, or
    resuming more than once. Caught by test 12 (the `run_aborted`
    follows the engine's `run_resumed`, so the drive's scan wrote it) and
    in review: the check calls `resume_while` and returns.
13. Leaving the stop watcher latching `halt` on a park. Caught by
    `a_park_does_not_halt_the_drive`.
14. Checking for a pending control only at the waker's start (revision
    3). Caught by test 14.
15. Discarding the park only after an applied patch, or discarding it
    in a second cursor write after the exit's own. Caught by tests 15 and
    16 (the first); the second is a crash window (row D6e) caught in
    review.
16. Taking the event lock inside `post_control`, or any lock that a
    caller already holding the event lock could wait on in the other
    order. Deadlocks `request_pause` and `stop_run_with`; caught by the
    existing `operator_pause_test` and the stop tests, and in review
    against D8 "Lock order".
17. Applying a park that names another node. Caught by test 17.
18. Ending the waker on a failed pending read (`?` on the read of D7).
    One note appended while the waker polls kills the park's only waker.
    Caught by test 13.
19. Counting a park, or the engine's resume of it, as an intervention or
    an engine action, or dropping a person's resume of a park from the
    counts. Caught by `a_park_and_its_automatic_resume_are_no_intervention`.

## Code map

New (2): `crates/apb-engine/src/scheduler/wait_park.rs`;
`docs/superpowers/specs/2026-11-19-wait-park-design.md` (the public
record). The engine integration tests go into the existing
`wait_test.rs`, so `tests/main.rs` does not change.

Changed (47 files in 46 rows, row 24 being two):

| # | File | Change |
|---|---|---|
| 1 | `crates/apb-core/src/validate/guarded.rs` | `GuardedFeature::WaitPark`, `guarded_features`, its test |
| 2 | `crates/apb-engine/src/scheduler.rs` | `mod wait_park;`, the post in the wait branch |
| 3 | `crates/apb-engine/src/pause/cause.rs` | `WAIT_PREFIX`, `wait_reason`, `parse_wait_reason`, `cause_of`, tests (`PauseRequest::is_park` lives in `pause.rs`) |
| 4 | `crates/apb-engine/src/pause/kind.rs` | `PauseCause::Wait` |
| 5 | `crates/apb-engine/src/pause/clock.rs` | the exemption, its doc sentence, the clock tests in a new `mod tests` |
| 6 | `crates/apb-engine/src/pause/display.rs` | the wait line, tests |
| 7 | `crates/apb-engine/src/notify/transitions.rs` | no paused transition for a park, test |
| 8 | `crates/apb-engine/src/next_action/verdict.rs` | the resume line, the overdue line, `Clock.paused` |
| 9 | `crates/apb-engine/src/next_action/unattended.rs` | `Wait` resumes like `Quota` |
| 10 | `crates/apb-engine/src/next_action/tests.rs` | three next-action tests |
| 11 | `crates/apb-engine/src/report_html/model.rs` | `pause` |
| 12 | `crates/apb-engine/src/report_html/sections.rs` | the meta line |
| 13 | `crates/apb-engine/tests/suite/report_html_test.rs` | the pause line test |
| 14 | `crates/apb-engine/tests/suite/wait_test.rs` | integration tests 1 to 17 and 21 |
| 15 | `crates/apb-mcp/src/tools/run.rs` | the `run_wait` instruction, its unit test |
| 16 | `crates/apb-cli/src/pause.rs` | the early-resume note of a park, its unit test |
| 17 | `crates/apb-cli/tests/suite/pause_waker_cli_test.rs` | test 18 |
| 18 | `web/src/lib/api.gen.ts` | regenerated (`PauseCause`) |
| 19 | `web/src/lib/pause.ts` | `pauseLine`, `pauseCauseLabel` |
| 20 | `web/src/lib/pause.test.ts` | two tests |
| 21 | `docs/MCP.md` | "Pause and resume": the wait park, the clock bullet |
| 22 | `docs/HOWTO-authoring.md` | a `wait` section |
| 23 | `docs/GUARDRAILS.md` | "Older apb binaries": the `wait_park` guard |
| 24 | `CLAUDE.md`, `AGENTS.md` | counted as 1 here, 2 files: code map and one concept line, mirrored |
| 25 | `openspec/changes/wait-park/tasks.md` | ticks |
| 26 | `crates/apb-engine/src/scheduler/control_apply.rs` | D6: the park arm (candidate with its node check, event lock, re-read, the due time again of rule 3.1, discard), the discard of a park right after the entry every exit of a pass ends at, the salvage skipping a park |
| 27 | `crates/apb-engine/src/stop.rs` | D6: the watcher's Pause check skips a park; `a_park_does_not_halt_the_drive` |
| 28 | `crates/apb-engine/src/pause/waker.rs` | D7: the check on each poll of the `run_waker` loop, a failed read retried at the next poll |
| 29 | `crates/apb-engine/src/control.rs` | D8: `CONTROL_LOCK` and the count and append under it in `post_control`; `pending_pause` skips a park; two unit tests |
| 30 | `crates/apb-engine/src/run_stats/interventions.rs` | D3: a park and the engine's resume of it skipped in `interventions`; one unit test |
| 31 | `crates/apb-cli/tests/suite/journal_corpus_test.rs` | rows W1 and W2 (tests 19 and 20), their inline playbooks and module doc paragraph |
| 32 | `crates/apb-core/src/policy_file.rs` | D9: `NotifyPolicy.remind`, `validate(source)`, `Policies::remind_ms`, the module doc |
| 33 | `crates/apb-core/src/notify/precedence.rs` | D9: `Effective.remind_ms` |
| 34 | `crates/apb-core/tests/suite/policy_file_test.rs` | D9: three tests |
| 35 | `crates/apb-engine/src/event.rs` | D9: `NotifySet.remind_ms`; `SendIntentRecorded.target_digest` |
| 36 | `crates/apb-engine/src/notify/mod.rs` | D9: `Active.remind_ms`, `Active.target_set_seq`, the fold, `follow_parent` |
| 37 | `crates/apb-engine/src/notify/start.rs` | D9: `NotifyPlan.remind_ms`, `set_event`, a live change |
| 38 | `crates/apb-engine/src/notify/notice.rs` | D9: the reminder's subject and line |
| 39 | `crates/apb-engine/src/notify/view.rs` | D9: reminder rows |
| 40 | `crates/apb-engine/src/notify/send.rs` | D9: the predicate of a reminder (open, same target, label admitted), passed on every path, `record_unsent` under the journal lock, two unit tests |
| 41 | `crates/apb-engine/src/connector/call/request.rs` | D9: `CallContext.still_due`, `CallContext.target_digest` |
| 42 | `crates/apb-engine/src/connector/call/send_once.rs` | D9: the recheck at admission, the target digest on the draft |
| 43 | `crates/apb-engine/tests/main.rs` | D9: `mod notify_remind_test;` |
| 44 | `crates/apb-cli/src/notify.rs` | D9: the reminders line |
| 45 | `crates/apb-cli/tests/suite/notify_cli_test.rs` | D9: one test |
| 46 | `docs/NOTIFY.md` | D9: line 71 and `## Reminders` |

`crates/apb-engine/src/notify/transitions.rs` (row 7) also takes D9's
`TransitionId.reminder`, `due`, `reminder_may_send` and nine unit tests;
`docs/GUARDRAILS.md` (row 23) one sentence of D9 "Surfaces and docs" 4;
`CLAUDE.md` and `AGENTS.md` (row 24) the reminders. New with D9 (2):
`crates/apb-engine/tests/suite/notify_remind_test.rs` and
`openspec/changes/wait-park/specs/wait-reminders/spec.md`. The
notice's target digest (D9 "The notice's target") also touches
`crates/apb-engine/src/notify/record.rs` (`NotifyTargetRecord::digest`),
`crates/apb-engine/src/connector/intent/mod.rs` (`IntentDraft.target_digest`),
`crates/apb-engine/src/connector/intent/ledger/mod.rs`
(`IntentRecord.target_digest`), `crates/apb-engine/src/publish/intent.rs`
(no digest) and every literal `SendIntentRecorded` in the tests.

Total: 51 files with this change's own records (4 new, 47 changed,
`CLAUDE.md` and `AGENTS.md` counted apart): 34 for the park (2 new, 32
changed), as before revision 6, and 17 for the reminders (2 new, 15
changed). 41 of them are code, tests and generated files (rows 1 to 20,
26 to 45, and `notify_remind_test.rs`). Revision 1 estimated 38.
A file the compiler names for a literal `NotifySet` construction beyond
these is fixed where it is and counted in the final report. Untouched on
purpose: `workdir.rs`, `driver.rs`,
`scheduler/deferred.rs`, `scheduler/resume.rs`, `pause/request.rs`,
`pause/resume_all.rs`, `run_view.rs`, `deadline_hooks.rs`.

## Release notes

The change is not tagged here. List in the final report the behaviour
changes of `proposal.md` "Impact" 5.
