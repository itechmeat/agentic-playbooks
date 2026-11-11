# Routine health: design record

The change that adds a concurrency key, routine health and run retention
to apb, and fixes a torn last line of the review channel. The first part
is the proposal, the second the design as built, with its implementation
notes at the end.

## Proposal

### Why

This is the second half of the routine work; the first half,
`routine-operations` (start keys, run deadlines, step-started runs), is
merged (main 88b7fd01). A routine an outside scheduler starts every day
still has three gaps, and the review channel has one defect a routine
meets more often than anyone:

1. Two runs of one routine overlap. The only exclusion is the busy lock of
   the working tree (`crates/apb-engine/src/workdir.rs` `acquire_tree`,
   line 282, taken in `scheduler/prepare.rs` at line 1094). It is per tree
   and shared by every playbook of the project, so it is both too coarse
   (an unrelated playbook blocks the routine) and too weak: runs over
   different worktrees never meet, a paused run gives its lock back
   (`workdir.rs` `release_paused`, line 147) so a second run starts while
   the first is half done, and a queued start waits in arrival order
   (`acquire_queued_tree`, line 478) where a routine wants only the newest
   request. A start key (routine-operations) makes one event start once;
   it says nothing about two different events of one customer. Nothing
   says "at most one run per customer" or "the newest start replaces the
   waiting one".
2. A routine that keeps failing, or stops running, is silent. Every
   notification is about one run (`crates/apb-core/src/notify/mod.rs`
   `Transition`, line 25): a run that is never started sends nothing, and
   five failed nights send five separate failure notices nobody reads as
   a pattern. A run deadline now ends a stuck run `deadline_exceeded`,
   but nothing counts those ends. The history of a playbook is read for
   estimates and quality only (`crates/apb-engine/src/run_history.rs`
   `all_runs_of`, line 144).
3. Run directories pile up forever. Nothing removes a run (`apb results
   clean` removes stale result snapshots inside one run only), and the
   playbook trash cannot hold runs: `list_trash`
   (`crates/apb-core/src/versioning/mod.rs` line 1044) reads every
   `<id>-<millis>` folder in `.apb/trash/` as a deleted playbook, which is
   also the shape of a run id. Removing a run is not harmless either: a
   start-key entry bound to it would then be void, and the next repeat of
   its event would start a second run.
4. A decision posted at the wrong moment fails the run. The drive reads
   `reviews.jsonl` without the poster's lock
   (`crates/apb-engine/src/scheduler/review_gate.rs` `queued_decision`,
   line 38, through `crates/apb-engine/src/review.rs` `read_reviews_after`,
   line 988), and a half-written last line makes the read fail and the run
   end failed.

| Decision | Code example | Non-code example |
|---|---|---|
| D1 concurrency key | `concurrency: { key: "{{params.repo}}", on_busy: supersede }`: a new push to the same repository stops the stale review run | a CRM follow-up keyed by `{{params.account}}` with `queue_latest`: three web-form events for one account in a minute run once, with the newest, and with the window after the run before it |
| D2 routine health | a nightly dependency bump failing three nights in a row trips `consecutive_failures: 3` and holds the playbook until a person looks | the morning CRM follow-up silently stopped after a reinstall dropped its timer; `overdue: 26h` sends one notice |
| D3 retention | `apb runs prune` keeps the last successful build run and every run with an unknown push, and drops 300 old green runs | a daily mail digest keeps 30 days and the runs a follow-up check still waits on |
| D4 review channel | a reviewer approves a pull-request gate from the dashboard while the drive polls: the run goes on | a person answers an invoice approval gate at the moment the drive reads: the run goes on |

### What changes

1. Concurrency key (capability `concurrency-key`): a playbook block
   `concurrency: { key, on_busy: queue_latest | supersede | refuse }`. Per
   key at most one run is active and at most one waits; a newer start
   replaces the waiting one (which ends aborted `superseded:`),
   `supersede` also stops the active one gracefully, `refuse` refuses
   `concurrency_busy`. The key is rendered once at start from params,
   kept as a keyed MAC for ownership and a scrubbed display text, in a
   signed per-playbook slot file, admitted in a recoverable transaction,
   after the start-key claim and always before the tree lock (a keyed
   start takes the tree only once it holds its key), for starts and
   resumes alike. A provisional claim belongs to the live process that
   holds its owner lock, never to a timer, and every stop a slot
   promises is posted outside the slot's lock. A start from a run step never waits for or replaces
   the run that started it (`concurrency_own_run`). A refused start leaves
   no run and leaves its start key free. A queued run keeps its deadline
   and reads its previous-success window once it got the key. A run guard
   and a supervisor-patch rule keep the block in place. Validator V210 and
   V211.
2. Routine health (capability `routine-health`): a playbook block
   `health:` with `consecutive_failures`, `failure_rate` and `overdue`,
   `on_trip: notify | hold` and a `cooldown`. A `deadline_exceeded` end
   counts as a failure, a `superseded:` end and a person's stop never
   count, and runs over external data count. Evaluated after every
   terminal event of a top-level run (one hook where the journal appends
   it), by `apb health check`, by `apb resume --all` and by the dashboard
   every 10 minutes. A clear, a mute or an unmute raises a generation
   that fences every notice claimed before it. A trip and a recovery each send one notice
   through the machine's notify target (new transition `health`, in the
   default set), claimed and settled in a signed per-playbook ledger with
   the send ledger's rules and pinned to the target. `hold` refuses starts
   `playbook_on_hold` at the gate and again in preparation until a person
   runs `apb health clear`. `apb health`, `apb health check` (exit 3 when
   held), `apb health mute`; surfaces in `apb doctor`, the dashboard's
   playbook list, MCP `playbook_catalog` and `playbook_stats`, and `apb
   stats`. `policy.yaml` `health.playbooks` narrows which playbooks a
   machine watches. Validator V212.
3. Run retention (capability `run-retention`): `runs.keep_days`,
   `runs.keep_last` and `runs.keep_failed_days` in `policy.yaml`, next to
   routine-operations' `runs.start_key_days`, and `apb runs prune` (a dry
   run unless `--yes`, a person's command only). It never removes a live
   or paused run, the newest runs of a playbook, its latest production
   success, a run with a pending follow-up, a graded or open candidate
   trial run, a run with an unknown send, a run a live start key or a
   concurrency slot names (every run of a playbook whose key file does not
   verify), or a parent of a run it must keep; it touches nothing outside
   its anchors, reads journals strictly, and removes a family (a run and
   all its descendants) whole or keeps it whole. Each removed family
   leaves a small signed summary, written before its directories leave
   and read under the same lock as the directory listing, so health and
   `apb stats` count every run exactly once at every moment; one prune
   runs at a time.
4. Review channel (capability `review-channel`): a torn last line of
   `reviews.jsonl` (read as bytes, so a cut inside a character counts) is
   read as a decision not yet written, a torn line in the middle still
   fails, and a poster writes its line in one write and, before any
   check, completes a whole last entry that lost its newline or cuts a
   partial one a crashed poster left.

### Out of scope

1. A built-in scheduler; runs started by inbound events; a pilot slice
   for list nodes; review-gate scope summaries and typed review fields;
   connector doubles in evals; per-branch worktrees; quiet hours for
   notifications (`apb health mute` silences one playbook). Each is
   separate work; none is needed for the gaps above.
2. Health rules against a median of earlier runs (a run much slower or
   costlier than usual). Their false-alarm control (sample size, outliers,
   version changes) needs its own design; failures and silence are the
   signals a routine owner asked for first.
3. A concurrency key across projects or for sub-playbook children. The
   slot file lives in the project, like the start keys; a machine-wide
   key needs a store outside any project. A child's own `concurrency:` is
   ignored: its parent may hold the same key, and the child would wait
   for its own parent. A step-started run in another project is not in
   the lineage of `concurrency_own_run` for the same reason.
4. Automatic pruning. No apb process prunes on its own; deleting history
   is a person's act (`apb runs prune --yes`), and the outside scheduler
   can call it.
5. A restorable trash for runs. `.apb/trash/` belongs to playbooks (Why 3),
   and a trash that keeps the bytes saves no space. Prune shows its plan
   first and keeps a summary.
6. Health rules in `policy.yaml`. Thresholds follow the routine's cadence,
   which the author knows; the machine only says which playbooks it
   watches (`health.playbooks`), so a laptop that holds the checkout but no
   timer stays quiet.
7. Clearing a hold, muting or pruning from MCP, HTTP or the dashboard.
   They are a person's decisions and stay in the CLI, refused inside a run
   step; MCP and the dashboard show the state and the command.
8. Reusing `notify::trial::send_notice` as the health sender. It reloads
   the live target without pins and collapses outcomes to strings
   (`crates/apb-engine/src/notify/trial.rs` line 75); a health notice
   needs a pinned target and the sent, not sent, unknown outcomes.
9. Taking the reviews lock in the drive's read. The fix is a reader rule
   and a writer repair; no new lock and no new file.

### Impact

1. Crates: `apb-core` (new `concurrency.rs`, `health.rs`; schema fields
   `concurrency`, `health`; `policy_file` keys `runs.keep_*` and section
   `health`; `notify::Transition::Health`; `GuardedFeature::Concurrency`;
   validator modules `validate/concurrency.rs`, `validate/health.rs`;
   `workspace::LOCAL_ENTRIES`), `apb-engine` (new `concurrency.rs`,
   `health/`, `retention/`, `notify/health.rs`; `review.rs`; prepare,
   entry, resume, patch, forward patch, state fold, previous success, run
   history, gate, stop, connector call (`play_call_pinned`), run view,
   next actions, HTML report, run stats), `apb-mcp` (run_status,
   playbook_catalog, playbook_stats), `apb` CLI package (`apb health`,
   `apb runs prune`, `apb runs <id>`, `apb doctor`, `apb stats`, `apb
   resume --all`), `apb-server` (playbook list, run detail, the health
   sweep task, ts contract), `web` (playbook list badge, run detail notes
   and their wiring, journal lines, notify words).
2. Bundled content: none.
3. Journal: additive field `run_provenance.concurrency`; one new event
   type `previous_success_set` (not a checkpoint, safe to skip; only runs
   with the `concurrency` guard carry it). One new guarded feature name
   after `deadline`: `concurrency`.
4. New state under `.apb/` (git-ignored): `concurrency/`, `health/`,
   `runs-archive/`.
5. Behaviour changes for release notes:
   1. `notify.on` gains `health`, in the default set: a machine with a
      notify target hears about tripped routines of playbooks with a
      `health:` block. A config that lists `on` explicitly gets them only
      by adding `health`.
   2. A playbook on hold is refused at every launch surface
      (`playbook_on_hold`) until `apb health clear`; a trial still runs,
      and a repeated start key still answers its run.
   3. A start or a resume of a playbook with `concurrency:` may wait,
      replace a waiting run or stop the active one, as its `on_busy` says.
   4. `policy.yaml` accepts `runs.keep_days`, `runs.keep_last`,
      `runs.keep_failed_days` and `health:`; an older apb reads a file
      with them as invalid (it refuses launches through agent channels and
      creates no default notify subscription until updated).
   5. A review decision posted while the drive reads no longer fails the
      run.
6. Built on main 88b7fd01, with `routine-operations` merged; it uses that
   change's `stop_run_with` with `StopRequest`, the `runs:` section, the
   start-key store and its `Claim`, `signed_store::transact` with
   `Unsigned::Refuse`, the deadline guard and `run_deadline`, the
   step-start lineage (`launch.started_by`), and `run_guard(playbook,
   events)`. Validator codes V210 to V212 of the reserved V210 to V219
   (checked free on 88b7fd01).

## Design

Read `proposal.md` first. Every decision below names its files, its
functions and the line where the code is today. Built on main 88b7fd01
(routine-operations merged, its record at
`docs/superpowers/specs/2026-11-09-routine-operations-design.md`); every line
number below was checked against that tree. If a line moved, find the
site by the name given next to it, never by the number alone.

Four decisions, built in the order D0, D4, D1, D3, D2: the
review channel fix (D4) is independent and small, so it goes first;
routine health (D2) reads the archive that retention (D3) writes, so D3
comes before D2.

Nothing in this document is left to the implementer. Where a choice was
open, it is decided here and the reason is given.

### What this change takes from routine-operations

All of these exist on 88b7fd01 (checked):

| Piece | Where |
|---|---|
| `stop_run_with`, `StopRequest { by, reason, graceful, due }`, `StopOutcome::NotDue` | `crates/apb-engine/src/stop.rs` lines 171, 107, 78 |
| `RunsSection { start_key_days }` and `Policies::start_key_days` | `crates/apb-core/src/policy_file.rs` lines 59, 281 |
| `StartKeysFile`, `StartKeyEntry`, `find`, `expire` | `crates/apb-core/src/start_key.rs` lines 214, 186, 265, 272 |
| the engine's start key: `lookup`, `claim`, `Claim::attach`, `Claim::bind`, `Drop for Claim`, `doctor_line` | `crates/apb-engine/src/start_key.rs` (lookup 231, claim 308, attach 406, bind 414, Drop about 437, doctor_line 459) |
| `signed_store::transact` with `Unsigned::Refuse`, `StoreError::Unverified` | `crates/apb-core/src/signed_store.rs` lines 219, 80, 73 |
| `fsutil::try_hold_lock` | `crates/apb-core/src/fsutil.rs` line 815 |
| `GuardedFeature::Deadline` (`ALL` has 8 entries) | `crates/apb-core/src/validate/guarded.rs` lines 46, 51 |
| `run_guard(playbook, events)` and `run_guard_of(playbook, deadline)` | `crates/apb-engine/src/scheduler/prepare.rs` lines 1757, 1767 |
| the run deadline: `DEADLINE_EXCEEDED`, `Expect`, `stop_if_expired`, `in_force`, `remaining_ms`, `expired`, `patch_refusal` | `crates/apb-engine/src/run_deadline.rs` (DEADLINE_EXCEEDED 28, Expect 181, stop_if_expired 209, patch_refusal 320) |
| step-started runs: `CallerSnapshot`, `step_start::resolve`, `chain_of`, `run_view::started::{started_by, started_runs}` | `crates/apb-engine/src/step_start.rs` lines 28, 56, 97; `crates/apb-engine/src/run_view/started.rs` lines 46, 60 |
| test barriers for race tests | `crates/apb-engine/src/deadline_hooks.rs` (`Hook`, `at`, `clear`) |

### D0. Shared pieces

#### Validator codes

| Code | Severity | Decision | Rule |
|---|---|---|---|
| V210 | error | D1 | malformed `concurrency:` block |
| V211 | warning | D1 | `on_busy: supersede` on a playbook whose effective effects include `irreversible` |
| V212 | error | D2 | malformed `health:` block |

Checked on 88b7fd01: `git grep -ohE '"V2[0-9][0-9]"' crates | sort -u`
prints `"V200"`, `"V220"`, `"V221"`, `"V222"` only, so V210 to V219 are
free. V213 to V219 stay reserved for this change family. Each family is
its own module in `crates/apb-core/src/validate/` (`concurrency.rs`,
`health.rs`), declared in `validate/mod.rs` next to `mod deadline;`
(line 15, keep the list sorted: `mod concurrency;` after
`mod cloud_models` and before `mod connectors`, `mod health;` after
`mod guarded` and before `mod isolation`), and called from `validate()`
right after the deadline call (line 456) and before the cloud models
call (line 457):

```rust
deadline::check_deadline(playbook, &mut r); // V200
concurrency::check_concurrency(playbook, &mut r); // V210, V211
health::check_health(playbook, &mut r); // V212
cloud_models::check_cloud_models(playbook, ctx, &mut r); // V220-V222
```

V211 reads `crate::effects::effective` (`crates/apb-core/src/effects.rs`
line 123).

#### New state under `.apb/`

`crates/apb-core/src/workspace.rs` `LOCAL_ENTRIES` (line 27) gets
`"concurrency/"`, `"health/"` and `"runs-archive/"`, each with a one-line
comment like `"start-keys/"` (line 52). The existing gitignore tests of
that file cover them.

#### Enforcement state is signed, and a bad file refuses

The concurrency slots (D1) and the health state (D2) decide whether a
start is admitted, so a corrupt or forged file must never read as
"nothing held". Both are `signed_store::SignedDoc` files
(`crates/apb-core/src/signed_store.rs`: trait line 27, `read` line 110,
`transact` line 219), and every write and every deciding read goes
through `transact(root, scope, playbook, Unsigned::Refuse, change)`,
the same entry the start keys use. `update` (line 201, which replaces an
unsigned file) is never used for these two files. A read for display
only uses `read` (line 110) and looks at `Loaded.signed` (line 57).

A present file that does not verify or parse (`StoreError::Unverified`):

1. Concurrency: every start and resume of that playbook is refused
   `concurrency_state_unreadable: <path> does not verify (changed outside
   apb or written by another installation); move it aside to start
   again`. Unkeyed playbooks (no `concurrency:` block) never read it.
2. Health: the playbook reads as `held` with the detail `health state
   unreadable: <path>`; starts are refused `playbook_on_hold`, and `apb
   health clear <id>` (a person) renames the file to
   `<id>.json.unreadable-<now_ms>` and then writes a fresh signed one
   with `transact(.., Unsigned::Refuse, ..)` (the path is now absent, so
   it starts fresh).

Both name the file in `apb doctor` (WARN). A forged file can therefore
only refuse starts, never admit or clear one.

The store's `DirLock` is stolen after 60 s (`fsutil::lock_dir`,
`crates/apb-core/src/fsutil.rs` line 888), so a transaction never waits
on anything slow: no connector call, no stop of a run (a stop of a dead
run writes its end and runs the health observation of D2, which may
send), no wait for a run, inside `change`. A transaction only reads and
writes its own file and reads run directories; every stop a transaction
decides is recorded in the file and posted after the transaction
returned (D1 "Posting promised stops").

#### The run check

Before acting on a run a stored file names (a slot's holder or waiter, a
health fact, a start-key entry read by retention), the run is checked
with one helper in the new `crates/apb-engine/src/concurrency.rs` (also
used by D2 and D3; it lives there because D1 is built first):

```rust
pub enum RunCheck {
    /// This installation's run of the playbook and scope, journal read.
    Valid(RunDir),
    /// Proven not to be such a run: the holder or fact is dropped.
    Void,
    /// The run may be such a run but its journal does not read strictly
    /// (a torn tail, an unknown event after the last checkpoint, an I/O
    /// error): never dropped, never counted, never removed.
    Unreadable,
}
pub fn checked_run(root: &Path, scope: Scope, playbook: &str, run_id: &str,
                   key_digest: Option<&str>) -> RunCheck;
```

1. `run_id` is a safe segment (`apb_core::registry::is_safe_segment`,
   `crates/apb-core/src/registry.rs` line 78), else `Void`;
2. `<root>/.apb/runs/<run_id>` is a real directory
   (`std::fs::symlink_metadata`, not a link), else `Void`;
3. it carries this installation's origin stamp
   (`apb_core::run_origin::verify`, `crates/apb-core/src/run_origin.rs`
   line 101), else `Void`;
4. the journal reads with the strict `crate::event::read_all`
   (`crates/apb-engine/src/event.rs` line 3643), else `Unreadable`;
5. its `run_started.playbook` equals `playbook`
   (`crate::run_history::started`, `crates/apb-engine/src/run_history.rs`
   line 52) and its `run_provenance.origin` label equals `scope`, else
   `Void` (an absent `run_started` or `run_provenance` is `Void`);
6. when `key_digest` is `Some(d)` (a slot holder): its
   `run_provenance.concurrency` is present and its `key_digest` equals
   `d`, else `Void`.

`RunDir` holds the path and the events read in step 4. Why the strict
reader: the lossy reader (`read_all_lossy_tail`, line 3747) is for
reporting surfaces only (its own doc), and the stop-only reader
(`read_all_for_stop`, line 3660) accepts an unknown event after the
last checkpoint because a stop only appends an abort; dropping a holder,
counting a fact or deleting a run has no such excuse. A run that cannot
be read is kept and not acted on.

#### Test barriers

`crates/apb-engine/src/deadline_hooks.rs` `Hook` (test builds only) gets
six variants, each run with `crate::deadline_hooks::at(hook, &run_dir)`
behind `#[cfg(any(test, feature = "testing"))]`, as `StopBeforeLock` is
today:

| Hook | Where |
|---|---|
| `KeyAdmitted` | `prepare_placed`, right after `admit` returned `Ok` (D1 preparation step 4) |
| `KeyBeforeLook` | `wait_turn`, before each look at the slot (D1) |
| `HealthBeforeDispatch` | health phase 2, after the claim of phase 1 and before the dispatch check (D2), with the run directory argument replaced by `<root>/.apb/health` |
| `HealthBeforeSettle` | health phase 2, after the call returned and before phase 3 (D2), same argument |
| `PruneFamilyLocked` | retention `apply`, after the event locks of a family are taken and before its recheck (D3), with the family root's run directory |
| `PruneAfterStage` | retention `apply`, after the members of a family were moved into staging and before the commit rename (D3), same argument |

A hook's scope is a project root, so a test sets it with
`deadline_hooks::set(hook, project_root, f)`; `f` usually waits on a
`std::sync::Barrier` or a channel the test holds.

#### A person's command

`apb health clear`, `apb health mute`, `apb health unmute` and `apb runs
prune --yes` are a person's: each starts with
`if let Some(why) = apb_core::step_env::StepCaller::from_env().refusal("<what>")`
(`crates/apb-core/src/step_env.rs` lines 17 and 37), prints `why` on
stderr and exits 2. None has an MCP tool or an HTTP write route.

#### Lock order

One order, everywhere: the start-key claim (its store `DirLock` only for
the transaction, its `HeldLock` reservation for the whole preparation),
then the concurrency slot reservation (its store `DirLock` only inside
one `transact`, its owner `HeldLock` from admission until the run is
confirmed or released, D1), then the working-tree busy lock. Nothing
nests inside a concurrency or health transaction: no run's event lock
(`stop::EVENT_LOCK`, `crates/apb-engine/src/stop.rs` line 74), no stop,
no send. The concurrency store lock is never taken while a run's event
lock is held: a resume calls the concurrency functions before
`try_journal_resumed` takes the event lock and after it released it.
Retention takes the prune lock, then the event locks of one family
(parent first, then the children in the order their `child_run_started`
events appear, depth first), then the archive lock (D3).

### D4. A torn last line of reviews.jsonl

#### The defect

`crates/apb-engine/src/review.rs` `read_reviews_after` (line 988) reads
`reviews.jsonl` line by line and fails on the first line that does not
parse (`EngineError::Yaml`, line 1004). The drive reads it on every poll
of a waiting gate (`crates/apb-engine/src/scheduler/review_gate.rs`
`queued_decision`, line 38, the call at line 44, from `await_decision`,
line 118). A poster appends under `reviews.jsonl.lock`
(`append_decision`, line 801; the lock at line 814) with
`writeln!(file, "{line}")` (line 887), but the drive reads without that
lock. A drive that reads while a poster is mid-append sees a partial last
line, the read fails, and the error ends the run failed: one valid
decision posted at the wrong millisecond kills the run.

#### The decision

1. Reader: `read_reviews_after` reads the whole file as bytes once
   (`std::fs::read`, never `read_to_string`: a write can stop inside a
   multi-byte UTF-8 character, and decoding the whole file first would
   fail on it), splits the bytes on `b'\n'` with `split_reviews`, and
   decodes each part on its own:
   1. Every complete line (followed by `b'\n'`): blank lines are
      skipped as today; a line that is not UTF-8 or does not parse as a
      `ReviewEntry` fails with `EngineError::Yaml`, as today. A torn
      line in the middle, and an unparsable last line that ends with
      `'\n'`, are corruption, not a write in progress.
   2. The unterminated tail (bytes after the last `b'\n'`, when there
      are any): when it is UTF-8 and parses as a `ReviewEntry` it is
      returned (a writer that wrote the line and not yet the newline;
      the drive may consume it); otherwise it is a line still being
      written and is skipped (the next poll reads it again, whole).
2. Writer: `append_decision` writes the line and its newline in one call,
   `file.write_all(format!("{line}\n").as_bytes())`, instead of
   `writeln!`, so a reader sees a whole line far more often.
3. Writer repair, `fn repair_tail(path: &Path) -> Result<(), EngineError>`
   in `review.rs`: the first thing `append_decision` does after it took
   the reviews lock (line 814), before `check_review_request`,
   `check_review_target` and the sequence read (line 871), so every
   check and the sequence number see the repaired file. When the file is
   not empty and does not end with `b'\n'`, a poster crashed mid-append
   (only posters write, and they hold the lock):
   1. the tail parses as a whole `ReviewEntry` (the reader rule 1.2
      returns it, and the drive may already have consumed it): the
      repair appends the missing `b'\n'`, so the entry stays and keeps
      its `seq`;
   2. otherwise the partial tail is cut: `File::set_len` to the length
      up to and including the last `b'\n'` (0 when there is none). The
      crashed poster never returned success, so no caller was told its
      decision was recorded.
   Without the repair the next append would glue a valid line to the
   partial one and turn it into a torn middle line that fails the run.

One helper does the split for both: `fn split_reviews(bytes: &[u8]) ->
(Vec<&[u8]>, Option<&[u8]>)` in `review.rs` (the complete lines without
their newline, and the unterminated tail, `None` when the file ends
with `b'\n'` or is empty). `post_decision`'s own reads
(`read_reviews_after` calls at lines 202, 329, 871, 949, 966, 975) get
the same rule for free.

Why not take the reviews lock in the drive's read: the lock is a
`DirLock` that the drive would take on every poll of every waiting gate,
and a stale lock is stolen only after 60 s; the reader rule needs no
lock and no new file.

Tests (`review.rs` `mod tests`, line 1017):

1. `a_torn_last_line_is_not_yet_written`: two whole entries, then a
   third entry's first half without a newline; `read_reviews_after(dir,
   None)` returns the two entries; after the rest of the line and `\n`
   are appended, it returns three.
2. `a_torn_middle_line_still_fails`: a whole entry, a half entry, `\n`,
   a whole entry; the read is `Err(EngineError::Yaml(_))`.
3. `an_unparsable_terminated_last_line_still_fails`: a whole entry and
   `garbage\n`; the read is an error.
4. `a_whole_last_line_without_newline_is_read`: one entry without the
   trailing `\n`; it is returned.
5. `a_post_cuts_a_crashed_posters_tail`: a whole entry, then a half
   entry without a newline, then `post_review` of a new decision; the
   file holds exactly two lines, both parse, and the second is the new
   decision with `seq == 1`.
6. `a_tail_split_inside_a_character_is_not_yet_written`: a whole entry
   whose `note` holds `"é"`, then the first bytes of a second such
   entry cut between the two bytes of `é`; the read returns one entry
   (a `read_to_string` implementation fails here).
7. `a_post_keeps_a_whole_entry_missing_its_newline`: a whole entry
   (`seq 0`) written without its newline, then `post_review` of a new
   decision; the file holds two lines, the first is the old entry
   unchanged with `seq 0`, the second the new one with `seq 1` (a
   repair that always cuts loses the first decision, which the drive
   may already have consumed).

And one drive test in `crates/apb-engine/tests/suite/review_torn_test.rs`:
`a_gate_waits_through_a_torn_line`: a playbook with one `human_review`
gate run in a background thread; once `review_requested` is journaled,
the test appends half of a valid decision line to `reviews.jsonl`
without the lock, waits 1.5 s (several polls), checks the run is still
running, appends the rest and `\n`, and the run ends `succeeded` with
the gate's decision consumed. Before the fix this run ends `failed`.

### D1. Concurrency key

#### Schema

`crates/apb-core/src/concurrency.rs` (new; `pub mod concurrency;` in
`crates/apb-core/src/lib.rs`, alphabetical):

```rust
/// One run per rendered key: what the playbook's `concurrency:` says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConcurrencyBlock {
    /// A template over `params.*`; absent: one key for the whole playbook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default)]
    pub on_busy: OnBusy,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnBusy { #[default] QueueLatest, Supersede, Refuse }
impl OnBusy { pub fn as_str(self) -> &'static str; } // queue_latest | supersede | refuse
/// The longest key template, in characters (V210).
pub const KEY_MAX_CHARS: usize = 500;
/// The longest key text shown, in characters.
pub const DISPLAY_MAX_CHARS: usize = 120;
/// The MAC domain of a rendered key.
pub const KEY_DOMAIN: &str = "apb-concurrency-key-v1";
/// The prefix of the reason of every run a newer start replaced.
pub const SUPERSEDED: &str = "superseded";
/// The rendered key of an absent `key`.
pub const WHOLE_PLAYBOOK_KEY: &str = "*";
```

`Playbook` (`crates/apb-core/src/schema.rs` line 139) gets, right after
`deadline` (line 207):

```rust
/// At most one active and one waiting run per rendered key (see
/// [`crate::concurrency`]). Additive to schema 2; no migration.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub concurrency: Option<crate::concurrency::ConcurrencyBlock>,
```

Every struct literal of `Playbook` the compiler lists gets
`concurrency: None`.

```yaml
concurrency:
  key: "{{params.account}}"
  on_busy: queue_latest   # or supersede, refuse
```

V210 (error), in `crates/apb-core/src/validate/concurrency.rs`
`check_concurrency`: `key` empty or whitespace only; `key` over
`KEY_MAX_CHARS` characters; a `{{...}}` reference that is not
`params.<name>` of a declared param; a reference to a param with
`secret: true` or whose name `apb_core::token_shape::name_looks_secret`
(`crates/apb-core/src/token_shape.rs` line 131). The references come from
`super::templates::template_refs`, as `check_worktree`
(`crates/apb-core/src/validate/node_io.rs` line 256) reads them. V211
(warning): `on_busy: supersede` and
`crate::effects::effective(playbook)` contains `Effect::Irreversible`.
Messages:

1. `V210: concurrency.key is empty`
2. `V210: concurrency.key is <n> characters; at most 500`
3. `V210: concurrency.key reads {{<ref>}}; it may read only declared params (params.<name>)`
4. `V210: concurrency.key reads the secret param <name>; a key is kept in a file and shown, so it may not read a secret`
5. `V211: on_busy: supersede stops a running run of a playbook with irreversible effects; a stopped run may leave such an effect half done. Prefer queue_latest`

#### Identity and display

```rust
// crates/apb-engine/src/event.rs, next to `PreviousSuccess` (line 56);
// mirrored to TypeScript
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConcurrencyRecord {
    /// `run_origin::tag(KEY_DOMAIN, rendered)`: the slot's identity,
    /// never shown.
    pub key_digest: String,
    /// The rendered key scrubbed with the run's scrubber and clipped to
    /// `DISPLAY_MAX_CHARS` characters: shown, never compared.
    pub display: String,
    pub on_busy: String,
    /// Whether the start waited for the key: the run then reads its
    /// `run.previous_success` window once it got the key.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub queued: bool,
}
```

The render: `crate::scheduler::worktree` renders its template with
`render(...)` (`crates/apb-engine/src/scheduler/worktree.rs` line 85,
inside `resolve_at_start`, line 66); the key uses the same `render` call
over the final params, with `WHOLE_PLAYBOOK_KEY` when `key` is absent.
Each param the key reads is checked with
`apb_core::run_params::sensitive(declared_secret, name, value)`
(`crates/apb-core/src/run_params.rs` line 41): a `Some` refuses the start
`concurrency_key_sensitive: param `<name>` holds a value that looks like
a credential; it cannot key a run`, naming the param, never the value.
`secret: false` is not proof that a value is harmless.

`display`: the rendered key through
`apb_core::scrub::Scrubber::for_workspace(root, Vec::new())` and then
clipped at `DISPLAY_MAX_CHARS` characters (by `char`, never inside a
UTF-8 sequence). Every message, refusal and surface uses `display` only;
the raw rendered key is never stored, journaled or formatted.

#### Store and slots

```rust
// crates/apb-engine/src/concurrency.rs (new)
/// A `signed_store::SignedDoc`: `MAC_DOMAIN = "apb-concurrency-v1"`,
/// `SCHEMA = 1`, `DIR = "concurrency"`, `ENTRIES_KEY = "slots"`.
/// `.apb/concurrency/<scope>/<playbook>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcurrencyFile {
    pub schema: u32, pub scope: String, pub playbook: String,
    /// By `key_digest`. Serialized as an array of `Slot` (the signed
    /// store counts `ENTRIES_KEY` as an array).
    #[serde(default)] pub slots: Vec<Slot>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub mac: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    pub key_digest: String,
    pub display: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub active: Option<Holder>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub waiting: Option<Holder>,
    /// Stops this file promised and that were not posted yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub stops: Vec<PendingStop>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    pub run_id: String,
    /// Set while the claim is provisional: a start not yet confirmed, a
    /// resume not yet journaled, or a waiter. Its owner holds the `flock`
    /// lock `<reservation>.lock` in the owners directory for as long as
    /// it owns the claim.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub reservation: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingStop { pub run_id: String, pub reason: String }
/// How often a waiter looks at its slot.
pub const TURN_STEP: Duration = Duration::from_millis(250);
/// The least time a keyed run waits for the working tree once it holds
/// its key.
pub const TREE_AFTER_TURN: Duration = Duration::from_secs(30);
/// The owner lock of a provisional claim, held for the claim's lifetime.
pub struct SlotOwner { /* key_digest, reservation, the HeldLock, root, scope, playbook */ }
```

`slots` is a `Vec` with at most one entry per `key_digest` (the signed
store's `read` counts an array under `ENTRIES_KEY`, line 138); empty
slots (no active, no waiting, no stops) are removed on write.

Ownership decides by the owner, never by age: the pattern the start
keys already use (`crates/apb-engine/src/start_key.rs` module doc lines
6 to 12, `claim` line 308). A reservation is
`apb_core::connector::marker::new_nonce()`
(`crates/apb-core/src/connector/marker.rs` line 135); its owner takes
`apb_core::fsutil::hold_lock(owners_dir, "<reservation>.lock")`
(`fsutil.rs` line 808) before the transaction that records it, where
`owners_dir` is `.apb/concurrency/<scope>/<playbook>.owners`. The owner
is alive while `fsutil::try_hold_lock(owners_dir, "<reservation>.lock")`
(line 815) fails with `WouldBlock`; a `flock` lock is per open file, so
the probe answers correctly inside the owner's own process too. The
`SlotOwner` value keeps the `HeldLock`; dropping it releases the lock
and removes the lock file. Why not a time limit (an earlier draft
cleared a provisional holder after 60 s): a live preparation can take
longer than any fixed limit (a worktree, a known-problems recheck), and
a second start would then take a key a live run still holds. Why not
the run check: before `run_provenance` and the origin stamp exist
(`allocate_run` at `prepare.rs` line 1113, the stamp at line 1410) a
live claim's directory is not yet a valid run.

A slot heals on every access inside a transaction, in this order
(`fn heal(root, scope, playbook, slot: &mut Slot)`); it reads only, and
writes only the slot:

1. A holder with a `reservation` whose owner is alive is kept as it is,
   whatever its directory holds.
2. A holder with a `reservation` whose owner is gone:
   1. `active`: when `checked_run(.., Some(key_digest))` is `Valid` and
      the run's claim became durable (a start: `run_provenance` is
      journaled; a resume: a `run_resumed` is newer than the journal's
      last terminal event), the reservation is cleared (the confirm was
      lost in a crash); when it is `Unreadable`, it is kept; else it is
      cleared (a crash in preparation or in a resume).
   2. `waiting`: kept while `liveness::driver_alive(run_dir, run_id) ==
      Some(true)` (a detached driver between its start and its first
      look holds the driver claim its spawner published, `entry.rs` line
      717); else cleared, and when `checked_run` is `Valid` and the run
      is not terminal, `stops += { run_id, "superseded: its driver ended
      while it waited for key `<display>`" }`: nothing will ever drive
      it (the waiter process ended or the machine rebooted).
3. A holder without a `reservation`: `checked_run(.., Some(key_digest))`
   `Void` clears it; `Unreadable` keeps it; `Valid` with a terminal fold
   (`RunState::fold(..).run_status.is_terminal()`) clears it. A paused
   active run, and an active run whose driver died, keep the slot: `apb
   stop <id>` frees it, and the `refuse` message names that command.
4. `stops` is not touched here (posting is outside the transaction,
   below), except that an entry whose run is `Void` is dropped.
5. When `active` is none and `waiting` is some, the waiter is not moved
   here: only the waiter itself moves (`wait_turn`), so it knows it got
   the turn.

#### Posting promised stops

`fn post_stops(root, scope, playbook, key_digest)`: after every
transaction that may have added a stop (`admit`, `wait_turn`'s look,
`reserve_resume`, `confirm`, `ResumeReservation::confirm`), with no lock
held:

1. Read the slot's `stops` (from the transaction's result).
2. A stop whose `run_id` is the slot's `active` or `waiting` holder with
   a `reservation` (a provisional claim: a start not confirmed or a
   resume not yet journaled) is deferred: it stays recorded and is posted
   by the `confirm` that ends the provisional state, or dropped by the
   heal that clears the holder. Why: a resume reserves its slot before it
   journals `run_resumed`; a stop posted in between finds the run still
   terminal (`stop.rs` line 203, `AlreadyTerminal`), and the resumed run
   would then run although a newer start was promised its stop.
3. Every other stop is posted with `crate::stop::stop_run_with(root,
   &run_id, &StopRequest { by: PauseBy::Engine, reason: &reason,
   graceful: true, due: None })`.
4. One more transaction removes each entry that was posted and returned
   `Ok(_)` or `Err(EngineError::NotFound(_))`, matched by `run_id` and
   `reason`, when it is still there. An entry whose post failed stays and
   is posted by the next access.

A stop of a run nothing drives finalizes it (`stop.rs` line 265) and
runs the health observation of D2, which may send; this is why no stop
is ever posted inside a transaction.

#### Admission: one recoverable transaction

```rust
pub enum Admission {
    /// The slot was free: this run is active.
    Active(SlotOwner),
    /// The run waits for `holder`.
    Queued { holder: String, owner: SlotOwner },
}
/// The key of a start: rendered, checked and recorded, or `None` for a
/// run that takes no slot (no block, a child, a scenario, an eval).
pub fn key_for_start(root: &Path, playbook: &Playbook, params: &BTreeMap<String, String>,
                     opts: &KeyScope) -> Result<Option<ConcurrencyRecord>, EngineError>;
/// Under the store's lock: heal, decide, record the provisional holder
/// and any stops it promises, release; then post the stops.
pub fn admit(root: &Path, scope: Scope, playbook: &str, record: &ConcurrencyRecord,
             on_busy: OnBusy, run_id: &str, lineage: &BTreeSet<String>) -> Result<Admission, EngineError>;
impl SlotOwner {
    /// Once `run_provenance` is durable: an active holder's reservation
    /// is cleared and the lock released; a waiter keeps its reservation
    /// (it stays provisional until its turn) and the owner is kept by
    /// `wait_turn`. Fails `concurrency_lost` when the slot no longer
    /// names this run with this reservation.
    pub fn confirm(self) -> Result<Option<SlotOwner>, EngineError>;
}
impl Drop for SlotOwner { /* releases the lock; the next heal clears the holder */ }
/// Waits until this run is the slot's active run (below).
pub fn wait_turn(root: &Path, run_dir: &Path, run_id: &str, owner: Option<SlotOwner>)
    -> Result<Turn, EngineError>;
pub enum Turn { Active, Paused }
/// A resume's reservation (below).
pub fn reserve_resume(root: &Path, run_dir: &Path, run_id: &str)
    -> Result<Option<ResumeReservation>, EngineError>;
/// Why a supervisor patch from `old` to `new` must be refused.
pub fn patch_refusal(old: &Playbook, new: &Playbook) -> Option<String>;
/// The runs a start must never wait for or stop: the run step's own run,
/// its parent chain and its starter chain, in this project.
pub fn lineage(root: &Path, started_by: Option<&apb_core::launch::StartedBy>) -> BTreeSet<String>;
```

`admit` takes the owner lock of a new reservation first, then decides,
in one `transact(.., Unsigned::Refuse, ..)`, after `heal`:

1. The slot's `active` or `waiting` run is in `lineage` (a run step
   starting its own playbook with its own key, directly or through
   started runs or a parent): refuse `concurrency_own_run: key
   `<display>` is held by run `<id>`, which started this run; a run
   cannot wait for or replace the run that started it`, whatever
   `on_busy` says. Why: with `queue_latest` the new run would wait for
   its own starter, and with `supersede` it would stop it.
2. `refuse` and the slot has an `active` or a `waiting` holder:
   `EngineError::Conflict("concurrency_busy: key `<display>` is held by
   run `<id>`; it ends on its own, or `apb stop <id>` frees it")`, `<id>`
   the active holder, else the waiter; nothing written.
3. No `active` and no `waiting`: `active = { run_id, reservation }`,
   answer `Active`.
4. No `active` and a `waiting` B (B was not yet promoted, for example
   its look is late after the holder ended), `queue_latest` or
   `supersede`: the newest start replaces the waiting one, as everywhere
   else: `stops += { B, "superseded: a newer start (run `<run_id>`)
   replaced this queued run" }`, `waiting = None`, `active = { run_id,
   reservation }`, answer `Active`. Why not let the new run wait behind
   B: B was going to run only because it was the newest; it no longer
   is. Why not let the new run overtake B and keep B waiting (an earlier
   draft): B would then run after the newer start, the opposite of
   latest wins.
5. An `active` holder, `queue_latest` or `supersede`: when the slot had a
   waiter, `stops += { waiter, "superseded: a newer start (run
   `<run_id>`) replaced this queued run" }`; the new run becomes
   `waiting = { run_id, reservation }`; for `supersede` also `stops += {
   active, "superseded: run `<run_id>` started for key `<display>`" }`.
   Answer `Queued { holder: active }`.

On a refusal the owner lock is dropped before the error returns.

`lineage`: the verified `started_by.run_id` of the start when its
`workspace_id` is `None` or this project's
(`apb_core::projects::registered_id(root)`), then for each run added,
its own manifest `launch.started_by` (same rule) and its run config's
`parent_run` (`crate::run_config::read_run_config`), at most
`apb_core::launch::MAX_START_CHAIN + apb_core::schema::MAX_SUBPLAYBOOK_DEPTH`
steps (`crates/apb-core/src/launch.rs` line 113,
`crates/apb-core/src/schema.rs` line 1453). Runs of another project are
never in it (their keys live in another project's store).

#### In preparation

`prepare_placed` (`crates/apb-engine/src/scheduler/prepare.rs` line
666), for a top-level production start: `scenario.is_none()`,
`opts.eval.is_none()`, `opts.parent_run.is_none()` and `opts.depth ==
0`. Every other start takes no slot (a child's own `concurrency:` is
ignored: its parent may hold the same key, and the child would wait for
its own parent). The key is always taken before the tree: a keyed start
never takes the tree lock in preparation at all. Why: two keyed starts
that both took the tree first would meet at the tree (`acquire_tree`,
line 1094, returns `WorkdirBusy` before any allocation), and the loser
would fail before its `queue_latest`, `supersede` or `refuse` was ever
evaluated. The steps, in code order:

1. After the worktree resolution (`resolve_at_start` call, line 1066)
   and before the start-key claim (line 1079): `let key =
   crate::concurrency::key_for_start(...)?` (render, sensitivity,
   record; nothing written), then the health hold check of D2 (below).
   Both refusals leave nothing: no key claimed, no directory.
2. The start-key claim (line 1079), unchanged.
3. The tree (line 1090 to 1108): when `key` is some, `acquire_tree` is
   not called; when `is_write`, `queued_workdir =
   Some(opts.workdir_queue_wait.unwrap_or(Duration::ZERO).max(TREE_AFTER_TURN))`
   and `queued_reason = "concurrency: the key is taken before the
   working tree"`. The run config's `workdir_queue_wait_ms` (line 1262)
   is written from that same duration, so a detached driver waits as
   long. When `key` is none, the code is unchanged.
4. After `allocate_run` (line 1113) and `Claim::attach` (line 1115),
   when `key` is some:
   1. `crate::driver::publish_driver_pid(&run_dir, std::process::id())`
      (`crates/apb-engine/src/driver.rs` line 108): from here the run has
      a live driver claim (`liveness::driver_pid_is_live` counts any apb
      process, `liveness.rs` line 279), so an `apb stop` or an `apb
      pause` of the waiting run is posted as a control command, not
      written over the run, and a waiter whose process died reads
      `driver_alive == Some(false)`. A detached start republishes the
      driver's pid as today (`entry.rs` line 717); the drive's own
      `DriveClaim` (`scheduler.rs` line 715) replaces it.
   2. `admit(...)`. On `Err(e)`: `crate::driver::let_go_unclaimed(&run_dir)`,
      remove the new run directory (it holds only the empty journal
      `allocate_run` opened and `driver.pid`;
      `std::fs::remove_dir_all` after a `symlink_metadata` check that it
      is a real directory), then return `e`. The start-key `Claim`
      drops, finds no `run_started` and frees its entry, so a refused
      start leaves no run and no key. Why not `prep_try_unstarted`: that
      writes `run_started` and binds the key to a failed run, and a
      retry of the same event would then answer `already_started` for a
      start that never ran.
   3. The test barrier `KeyAdmitted`.
   4. `record.queued = matches!(admission, Admission::Queued { .. })`;
      when queued, `queued_reason = "concurrency: key `<display>` is
      held by run `<holder>`"`.
   5. `Prepared` gets two fields: `slot_owner: Option<SlotOwner>` and
      `key_queued: bool`. Every later error of preparation drops the
      owner with the `Prepared` value (Drop releases the lock; the next
      heal clears a holder whose run never journaled `run_provenance`,
      and a `run_provenance` that was journaled leaves a run the heal
      reads as terminal once `prep_try` finalized it).
5. When `record.queued` and `opts.continued_from.is_none()`: the
   `previous_success` computed at line 1014 is replaced by `None` before
   `run_provenance` is appended; the window is read after the turn
   (below). A continued run keeps its predecessor's window either way.
6. `run_provenance` (line 1432) gets `concurrency: key.clone()`; then
   the run guard (line 1460, see "Journal and guard"); then
   `slot_owner.confirm()` (through `prep_try`: a `concurrency_lost`
   finalizes the run failed); an active holder's owner is released
   there, a waiter's owner is kept in `Prepared.slot_owner`; then the
   start-key `bind` (line 1465).
7. `prepare_supervised_background_target` (`entry.rs` line 400) skips
   its supervisor spawn (line 426) when `p.key_queued`; the supervisor
   of a queued run starts after its turn (below). Why: a supervisor is
   an agent that costs while it watches, and its billing hold journals a
   pause (`spawn_supervisor_or_hold`, `scheduler/supervisor.rs` line
   188) that a waiting run cannot apply.

The deadline of a queued run: preparation already journals
`run_provenance.deadline` (line 1432) whatever the queue; `wait_turn`
ends the run at its deadline (below).

#### Waiting for the turn

```text
wait_turn(root, run_dir, run_id, owner) -> Turn:
  read run_provenance.concurrency; none: Ok(Active) (no slot)
  loop:
    test barrier KeyBeforeLook
    inside one transact(.., Unsigned::Refuse, ..):
      heal the slot
      if active == this run: drop the reservation (active = { run_id }), write, Ok(Active)
      if active is none and waiting == this run:
          active = { run_id } (no reservation), waiting = none, write, Ok(Active)
      if waiting != this run and active != this run:
          Err(EngineError::Superseded("a newer start replaced this queued run"))
    post the slot's stops (above)
    outside the lock, read the journal (read_all_lossy_tail):
      the fold is terminal: Err(EngineError::Conflict("the run ended while it waited for its key"))
      the fold is paused (a pause journaled before the driver claim was
        published): leave the slot (one transaction: waiting = none when
        it names this run), Ok(Paused)
      run_deadline::expired(events, now):
          match run_deadline::stop_if_expired(root, run_id, Expect::Any):
            Ok(true): Err(the stop)
            Ok(false): go on waiting (a resume's grant or a person's pause won the race)
            Err(e): Err(e)
      control::pending_stop_seq(run_dir) is Some: Err(the stop)
    sleep TURN_STEP
  on every Err and on Paused: the owner is dropped
```

The stop checks are the ones `claim_queued_workdir` makes while it waits
for the tree (`crates/apb-engine/src/scheduler/entry.rs` lines 226 to
243), with one difference: `stop_if_expired` returning `Ok(false)` keeps
the run waiting. `EngineError::Superseded(String)` is a new variant in
`crates/apb-engine/src/error.rs` (Display `superseded: {0}`). A run that
sees `Superseded` settles itself aborted: the caller journals
`run_aborted { reason: "superseded: ...", by: Some(PauseBy::Engine) }`
whether or not the replacing start's stop arrived yet (the stop, when it
comes, finds the run terminal).

A posted pause (`Control::Pause`, which `request_pause` posts because
the waiter published its driver claim, `pause/request.rs` line 84) is
not applied while the run waits: it stays pending and the drive applies
it at its first boundary after the turn, exactly as for a run queued for
the tree (`request_pause` doc: "a queued run pauses at its first
boundary"). The run then holds the key while paused, like any paused
active run. Why: one rule for both queues, and no second pause
implementation in the wait loop. Only a pause that was journaled
directly (the run had no driver claim yet, or an older apb paused it)
ends the wait with `Turn::Paused`: the run gives up its place, stays
paused, and a later `apb resume` takes the key through `reserve_resume`
like any resume.

`wait_turn` is called before any tree lock, at the two drive entries:

1. `Prepared::claim_queued_workdir` (`scheduler/entry.rs` line 218),
   first thing after the `queued_workdir.take()` guard, with
   `self.slot_owner.take()`; it returns `Result<Option<RunResult>,
   EngineError>` from now on (`Some` means "do not drive, answer this"),
   and its three callers (`run` line 298, `run_resolved` line 332,
   `drive_prepared` line 449) return that answer when it is `Some`. On
   `Turn::Paused`: `crate::driver::let_go_unclaimed(&run_dir)`, answer
   `Some(RunResult { run_id, outcome: RunStatus::Paused })`. On `Err`:
   `crate::driver::let_go_unclaimed(&run_dir)`, then close out through
   `close_out_unclaimed` (line 267), which gets one new arm before its
   `pending_stop` arm: `EngineError::Superseded(_)` journals
   `run_aborted { reason: e.to_string(), by: Some(PauseBy::Engine) }`.
   After `Turn::Active`, the tree wait as today with the run's
   `queued_workdir` duration (step 3 of preparation).
2. `drive_run_from_dir` (line 535), right after the "already driven"
   check (line 545) and before the tree wait (`acquire_handover_within`,
   line 616), for every run whose journal has a concurrency record (it
   needs no slot owner: a waiter's reservation lock was the preparing
   process's, and that process ended; until this driver's first look the
   heal reads the driver claim the spawner published, see heal rule 2.2,
   which therefore checks `liveness::driver_alive(run_dir, run_id) ==
   Some(true)` before it clears a waiter whose owner is gone). The
   driver takes a fresh owner for the waiter's reservation: `wait_turn`
   with `owner: None` re-reserves under a new nonce in its first
   transaction when the slot names this run as `waiting`. On
   `Turn::Paused`: `let_go_unclaimed`, return `Ok(RunResult { run_id,
   outcome: RunStatus::Paused })`. On `Err`: `let_go_unclaimed`, then
   the same close-out as above (moved to a free function
   `close_out_unclaimed(log, run_dir, e)` both entries call; the method
   becomes a one-line wrapper).

After `Turn::Active` at either entry, when the run's mode expects a
supervisor agent (`cfg.mode.expects_supervisor_agent()`, `run_config.rs`
line 122) and its record is `queued`, the entry calls
`supervisor::spawn_supervisor_or_hold(root, run_id, &playbook, &mut log)`
before the tree wait (step 7 of preparation skipped it).

Resumes take their slot through `reserve_resume`, below.

#### A queued routine reads its window after its turn

A queued start of a playbook that reads `run.previous_success.*`
resolves its window once it got the slot: a run B queued behind A would
otherwise keep the success before A and repeat A's work. Right after
`wait_turn` returns `Turn::Active` for a run whose record has `queued: true`, whose
journal has no `previous_success_set` yet, whose run config has no
`continued_from` and whose definition reads the namespace
(`apb_core::previous_success::reads`), the drive entry calls
`look_up_previous_success` (`prepare.rs` line 1815, made `pub(crate)`)
with the run config's `params` (`RunConfig.params`,
`crates/apb-engine/src/run_config.rs` line 146) and appends:

```rust
/// The `run.previous_success` window of a run that waited for its
/// concurrency key, read once it got the key and before its first node.
PreviousSuccessSet {
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "lenient_option")]
    previous_success: Option<PreviousSuccess>,
},
```

Not a checkpoint (`EventPayload::is_checkpoint` returns false for it):
it is safe to skip, and a run that has it carries the `concurrency`
guard, so an older apb never continues it. A lookup error ends the run
the way a start-time lookup error does today (`previous_success_unreadable`
through `close_out_unclaimed`).

One reader for the window: `crate::previous_success::recorded(events:
&[Event]) -> Option<PreviousSuccess>` (new, in
`crates/apb-engine/src/previous_success.rs`): the latest
`previous_success_set`, else the first `run_provenance.previous_success`.
Every reader that today finds the first `run_provenance.previous_success`
calls it instead:

1. `RunState::fold` (`crates/apb-engine/src/state.rs` line 570): the
   `RunProvenance` arm keeps its first-wins rule, and a new
   `PreviousSuccessSet` arm sets `s.previous_success` unconditionally;
2. `run_view::previous_success` (`crates/apb-engine/src/run_view.rs`
   line 408);
3. `previous_success::patch_refusal` (line 339), so a patch on a queued
   run is judged against the window it actually read;
4. the `continued_from` copy in `prepare_placed` (lines 1032 to 1037).

A resume never resolves the window again: the journal keeps it.

#### Resumes

`resume_inner_checked` (`crates/apb-engine/src/scheduler/resume.rs` line
1074) calls `reserve_resume` right before the tree is acquired
(`acquire_tree_for`, line 1222) and keeps the reservation through
`journal_resumed` (line 829, which calls `try_journal_resumed`, line
867):

1. `reserve_resume` reads `run_provenance.concurrency`; none: `Ok(None)`.
   Else it takes the owner lock of a new reservation and, in one
   transaction, heals, then:
   1. `active` is another run, or `active` is this run with a
      `reservation` (another resume of the same run is in progress):
      refuse `concurrency_busy` (same text as admission; for the second
      case `concurrency_busy: run `<id>` is being resumed by another
      process`).
   2. `active` is this run without a `reservation` (a paused holder
      resuming): `active.reservation = Some(new)`.
   3. No `active`: `active = { run_id, reservation }` (a waiter that was
      there stays waiting).
   Write; release; post the slot's stops.
2. After `journal_resumed` returned `Ok`, `reservation.confirm()`, outside
   the event lock: one transaction clears `active.reservation` when the
   slot still names this run with this reservation, and fails
   `concurrency_lost` otherwise; then the owner lock is released and the
   slot's stops are posted, so a stop deferred against the provisional
   resume (a supersede promised while it reserved) reaches the now live
   run. A `concurrency_lost` after `run_resumed` (the slot file was moved
   aside or rewritten by hand) stops the run through `stop_run_with(..,
   StopRequest { by: PauseBy::Engine, reason: "concurrency_lost: the
   run's key slot changed while it resumed", graceful: true, due: None
   })` and returns the error.
3. Any `Err` between 1 and 2 (a busy tree, the lifecycle recheck, a
   consent, a deadline refusal) drops the reservation: `impl Drop for
   ResumeReservation` runs one transaction that, only when `active` still
   names this run with this reservation, clears `active` (case 1.3) or
   only the reservation (case 1.2), releases the owner lock and posts the
   slot's stops. A crash leaves the owner lock free, and heal rule 2.1
   decides.

#### Journal and guard

`EventPayload::RunProvenance` (`crates/apb-engine/src/event.rs` line
152) gets, after `start_key` (line 206):

```rust
/// The run's concurrency key, when its playbook has a `concurrency:`
/// block and the run is a top-level production run.
#[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "lenient_option")]
concurrency: Option<ConcurrencyRecord>,
```

Every `RunProvenance { .. }` literal the compiler lists gets
`concurrency: None`.

`GuardedFeature::Concurrency` (`"concurrency"`) in
`crates/apb-core/src/validate/guarded.rs`: after `Deadline` in the enum
(line 46) and in `ALL` (line 51, now `[GuardedFeature; 9]`), in
`as_str` and `parse`, and set in `guarded_features` (line 85) when
`playbook.concurrency.is_some()`; the module doc gets one sentence.

`run_guard_of` (`prepare.rs` line 1767) becomes `run_guard_of(playbook,
deadline: bool, concurrency: bool)`: it filters `Concurrency` out of the
definition's features like `Deadline` and adds it exactly when
`concurrency` is true. Preparation passes `key.is_some()` (line 1460);
`run_guard(playbook, events)` (line 1757) passes whether the journal's
`run_provenance` has a `concurrency` record. So a child, a scenario and
an eval, which take no slot, are not guarded for it, and no patch drops
the guard of a run that has a slot.

Supervisor patches may not change the block:
`crates/apb-engine/src/scheduler/patch.rs` calls
`crate::concurrency::patch_refusal(playbook, &loaded.playbook)` right
after the deadline one (line 123), with the text `concurrency_changed:
the run's `concurrency:` block is not a supervisor's to change`;
`crates/apb-engine/src/forward_patch.rs` `protected_fields` (line 254)
grows to `[..; 10]` with `("concurrency_changed", "the `concurrency:`
block decides which runs wait or stop: only a person may change it",
|a, b| a.concurrency == b.concurrency)`.

#### Surfaces

`crate::run_view::concurrency(root: &Path, events: &[Event]) ->
Option<ConcurrencyView>` in `crates/apb-engine/src/run_view.rs`, next to
`deadline` (line 454):

```rust
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConcurrencyView {
    pub display: String,
    pub on_busy: String,
    /// The run this one waits for, while it waits.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub waiting_for: Option<String>,
}
pub fn concurrency_line(v: &ConcurrencyView) -> String; // key "<display>" (<on_busy>)[, waiting for run <id>]
```

`waiting_for` comes from a display read of the slot file (`read`, never
a write) while the run is not terminal and the slot names it as
`waiting`. Wired like the deadline:

1. `apb runs <id>`: `crates/apb-cli/src/run.rs` after the deadline line
   (line 2136): `say!("  concurrency: {}", concurrency_line(&v))`.
2. MCP `run_status`: `crates/apb-mcp/src/tools/run.rs` after line 369:
   `out["concurrency"] = json!(v)`.
3. HTTP run detail: `crates/apb-server/src/routes/runs.rs` `RunDetail`
   (line 27) gets `concurrency: Option<ConcurrencyView>` after
   `deadline` (line 240), filled after line 623.
4. HTML report: `crates/apb-engine/src/report_html/model.rs` next to the
   deadline (line 112) and its line in `sections.rs` beside the
   deadline's.
5. Dashboard: `RunDetail` in `web/src/lib/api.gen.ts` regenerates with
   `concurrency?`; `web/src/pages/RunView.svelte` line 634 passes
   `concurrency={detail.concurrency}` to `RunDriveNotes`
   (`web/src/lib/RunDriveNotes.svelte`), which shows `Key: <display>
   (<on_busy>)` and `waiting for run <id>`; `web/src/lib/journal.ts`
   `eventNote` (the `run_aborted` case, line 129) reads a reason starting
   `superseded:` as `superseded`, and a `previous_success_set` event as
   `previous success read after the key came free`.

Docs: `docs/HOWTO-authoring.md` `## concurrency (one run per key)` right
before `## deadline (a run's wall-clock limit)` (line 4349): the YAML,
the three modes, what a queued run looks like, key before tree, the
window of a queued routine, children, `concurrency_own_run`, the V210
and V211 table in the format of the V177 row (line 249), and the
older-apb note (the guard).

Wrong implementations to avoid (each is a test in
`crates/apb-engine/tests/suite/concurrency_test.rs` unless named
otherwise). A "holder that waits for a marker" is a playbook whose first
node is a `script` node running `while [ ! -f <marker> ]; do sleep 0.1;
done` and writing `<run_dir>/started` first, so a test knows the holder
is live before it starts the next run; no test relies on threads
starting at the same moment or on reading the final slot state alone.

1. A queued key run holding the tree lock.
   `a_queued_run_holds_no_tree_lock`: holder A waits for a marker; B
   starts with the same key and `queue_latest`; while B waits,
   `workdir::lock_owner` of the tree names A's driver, never B; the
   marker is written; A's next node runs; A ends `succeeded`, then B
   runs and ends `succeeded`.
2. More than one waiter. `the_newest_start_replaces_the_waiting_one`:
   holder A waits for a marker; B, C, D start in turn with
   `queue_latest`; B and C end `run_aborted` with a reason starting
   `superseded:`; the marker is written; D ends `succeeded` after A.
3. A displaced waiter ending failed.
   `a_displaced_waiter_ends_aborted_whatever_the_stop_timing`: the slot
   file is seeded (through `ConcurrencyFile` and `transact`) with B as
   `waiting` replaced by C and no pending stop for B; `wait_turn` for B
   returns `Superseded`, and B's journal ends `run_aborted` with a
   `superseded:` reason, never `run_finished failed`.
4. `supersede` killing at once. `supersede_stops_the_holder_gracefully`:
   the holder's agent stand-in traps SIGTERM and writes a marker; B
   starts with `supersede`; the marker exists, A ends `run_aborted` with
   a `superseded:` reason, B then ends `succeeded`.
5. Two concurrent starts both active. `refuse_admits_one_of_eight`: the
   playbook's first node waits for a marker; 8 threads wait on one
   `std::sync::Barrier` and then start with one key and `refuse`; the
   test waits until 7 of them returned; exactly 7 are refused
   `concurrency_busy`, the eighth is running (its `started` file
   exists), `.apb/runs` holds exactly one directory of the playbook;
   then the marker is written and that run ends `succeeded`. A
   sequential admission (each start finishing before the next is
   admitted) fails the "exactly 7 refused while one runs" assertion.
6. A live preparation losing its key.
   `a_live_preparation_keeps_its_key`: the hook `KeyAdmitted` holds
   start A right after its admission (its directory holds only the empty
   journal and `driver.pid`: no origin stamp, no `run_started`); start B
   of the same key with `refuse` runs to its refusal; B is refused
   `concurrency_busy` naming A; the hook releases A, and A ends
   `succeeded`. A heal that runs the run check on a provisional holder,
   or clears it after a fixed time, admits B. The same assertion with
   the clock of the slot unchanged for 61 s is
   `a_seeded_provisional_holder_with_a_live_owner_is_kept`: a seeded
   slot names R with reservation `r1`, the test holds
   `fsutil::hold_lock(owners_dir, "r1.lock")`, R's directory is empty;
   a start of the key queues behind R. `a_crash_before_provenance_frees_the_key`:
   the same seed with the owner lock not held: the start is admitted
   `Active`.
7. A tree race before admission. `two_keyed_starts_meet_at_the_key_not_the_tree`:
   holder A waits for a marker and holds the tree; the hook
   `KeyAdmitted` holds start B (`queue_latest`); start C
   (`queue_latest`, same key, no `workdir_queue_wait`) is prepared and
   returns without `WorkdirBusy`; B ends `run_aborted` `superseded:`; the
   marker is written; C ends `succeeded`. A preparation that takes the
   tree before admission fails C with `WorkdirBusy`.
8. A promised stop lost to a resuming run.
   `a_supersede_during_a_resume_reaches_the_resumed_run`: run F of key k
   (`supersede`) failed; the hook `BeforeResumeCommit` holds F's resume
   after its reservation; start N of k is admitted (it queues behind F
   and promises F's stop); the hook releases; F's resume journals
   `run_resumed`, then F ends `run_aborted` `superseded:`, and N ends
   `succeeded`. A heal that posts the stop at once (it finds F terminal
   and drops it) leaves F running to its own end.
   `two_resumes_of_one_run_hold_one_reservation`: two threads resume F
   at once (a `Barrier`); one is refused `concurrency_busy`, and after
   the refusal the slot still names F with the winner's reservation.
9. A waiter overtaken by a newer start.
   `a_new_start_replaces_a_waiter_that_was_not_yet_promoted`: holder A
   waits for a marker; B queues; the hook `KeyBeforeLook` holds B's
   looks; the marker is written and A ends; C starts with
   `queue_latest`; C is admitted `Active` and ends `succeeded`; the hook
   releases B, which ends `run_aborted` `superseded:` and never runs a
   node.
10. A wait that ignores its driver and its pause. Three tests, one per
    entry: `a_sync_waiter_is_driven_by_its_own_process` (`run` in a
    thread: while it waits, `liveness::driver_alive` of B is
    `Some(true)` and `apb stop B` ends it `run_aborted` with no
    `run_finished`), `a_threaded_waiter_survives_a_pause`
    (`run_background`: `request_pause` of the waiting B answers
    `Posted`; after A ends, B takes the key and journals `run_paused`
    at its first boundary, holding the key; `resume` runs it to
    `succeeded`), and `a_detached_waiter_is_kept_through_its_handover`
    (`start_detached` for B: between spawn and its first look the slot
    still names B). `a_deadline_grant_keeps_a_waiter_waiting`: B waits
    with a deadline of 2 s; the hook `StopBeforeLock` appends to B's
    journal a later deadline the way `run_deadline_test.rs` seeds a
    resume's grant, so `stop_if_expired` answers `Ok(false)`; B keeps
    waiting and runs after A (an implementation that ends the wait on
    any expired read ends B failed).
11. A notifier blocking the slot. `a_blocked_notifier_never_holds_the_slot`
    (in `health_test.rs`: it needs a watched playbook of D2):
    a health block with a notify stand-in that answers after 5 s; a
    stop of a dead waiter is promised; while the stop's health
    observation waits on the stand-in, `admit` of another key of the
    playbook returns within 1 s. A heal that posts stops inside its
    transaction blocks it for 5 s.
12. Ownership by display text. `display_collisions_hold_separate_slots`:
    two keys that scrub to the same display (two values the scrubber
    masks alike) and two 300-character keys equal in their first 120:
    four separate slots, four active runs.
13. A sensitive key value. `a_sensitive_key_value_is_refused`: a param
    `account` whose value is `ghp_` plus 36 characters: refused
    `concurrency_key_sensitive`; the value appears in no file under
    `.apb` and not in the error text.
14. A stale window for a queued routine.
    `a_queued_routine_reads_the_window_after_its_turn`: a playbook
    reading `run.previous_success.started_at`; B queues behind A; A
    succeeds; B's rendered prompt holds A's `started_at`, and B's journal
    has one `previous_success_set`.
15. A resume stealing a slot. `a_resume_racing_a_start_leaves_one_active`:
    a failed run F of key k; the hook `KeyAdmitted` holds a new start N
    after its admission; F's resume is refused `concurrency_busy`; the
    slot names N, never F.
16. A forged or corrupt store. `a_tampered_slot_file_refuses_starts`:
    a slot file with a changed byte refuses starts
    `concurrency_state_unreadable`; `a_forged_holder_is_void`: a signed
    slot naming a run of another playbook, or a run whose
    `run_provenance.concurrency.key_digest` differs from the slot's, is
    void and a new start is admitted. `an_unreadable_holder_is_kept`: a
    signed slot naming a run whose journal holds an unknown event type
    after its last checkpoint: the next start queues (a lossy or
    stop-only read would void it).
17. A child honouring its own block. `a_child_ignores_its_own_block`:
    parent and child share a key template; the child runs, the parent
    ends `succeeded`.
18. A patch dropping the block or its guard.
    `a_patch_cannot_drop_the_block`: a supervisor patch removing
    `concurrency:` is refused `concurrency_changed`; after an allowed
    patch the latest `run_guard` still names `concurrency`.
19. A step starting its own key. `a_step_cannot_queue_behind_its_starter`:
    run A (key k) runs a script step that starts the same playbook with
    the same key; the inner start is made through the engine with
    `opts.launch` carrying a `started_by` of A (the way
    `step_start_test.rs` builds one); it is refused
    `concurrency_own_run`; A ends `succeeded`.
20. A refused start keeping its start key.
    `a_refused_keyed_start_frees_its_start_key`: holder A of key k with
    `refuse` waits for a marker; a start with `--start-key e1` is refused
    `concurrency_busy`; no run directory was left; the marker is
    written; after A ends, the same start with `e1` runs.
21. A queued run at its deadline. `a_queued_run_ends_at_its_deadline`:
    B queued with `--deadline 2s` behind a holder that waits for a
    marker; B ends `run_aborted` with a `deadline_exceeded` reason and
    the slot no longer names it.
22. A detached queued run. `a_detached_queued_run_runs_after_the_holder`:
    A and B started with `start_detached`; B's driver waits in
    `drive_run_from_dir`; B ends `succeeded` after A.
23. A dead waiter. `a_waiter_whose_driver_died_is_released`: a seeded
    slot with W `waiting` (reservation `w1`, owner lock not held), W's
    `driver.pid` naming a dead pid; the next start of the key is the
    waiter, W ends `run_aborted` `superseded:`.
24. A supervisor watching a waiting run.
    `a_queued_supervised_run_starts_its_supervisor_after_its_turn`: B is
    started with a mode that expects a supervisor agent behind holder A;
    while B waits, B's directory has no `supervisor/spawned_at` (the
    file `billing_spawn_test.rs` line 169 reads); after its turn it
    has.
25. An older journal (`event.rs` tests):
    `run_provenance_without_concurrency_decodes_none`,
    `run_provenance_with_another_concurrency_shape_decodes_none`,
    `previous_success_set_then_node_finished_reads` (modelled on
    `follow_up_observed_settles_an_unknown_event_before_it`, `event.rs`
    line 4000: the `previous_success_set` line with its type renamed to
    one this build does not know, followed by `node_finished`, reads
    with `read_all`).

### D3. Run retention

#### Settings

`RunsSection` (`crates/apb-core/src/policy_file.rs` line 59) gets:

```rust
/// Successful runs older than this many days are removed (1 to 3650,
/// default 30).
pub keep_days: Option<u32>,
/// The newest runs of each playbook always stay (1 to 3650, default 20).
pub keep_last: Option<u32>,
/// Failed and aborted runs older than this many days are removed (1 to
/// 3650, default 90).
pub keep_failed_days: Option<u32>,
```

`RunsSection::validate` (line 66) checks each range with the message
`runs.<key> is <n>; use 1 to 3650`. `Policies` gets
`pub fn retention(&self) -> RetentionSettings` with each value the
larger of machine and project (a project only lengthens), defaults when
unset. `summary` (about line 380) names each key that is set, as it
names `start_key_days` (line 408). `RetentionSettings { keep_days,
keep_last, keep_failed_days }` lives in `policy_file.rs` too.

#### The plan

`crates/apb-engine/src/retention/mod.rs` (new; `pub mod retention;` in
`crates/apb-engine/src/lib.rs`):

```rust
#[derive(Debug, Clone, Serialize)]
pub struct PruneRow { pub run_id: String, pub playbook: String, pub scope: String,
                      pub started: String, pub outcome: String, pub action: PruneAction,
                      /// Bytes of the run directory (best effort).
                      pub bytes: u64 }
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", content = "reason", rename_all = "snake_case")]
pub enum PruneAction { Remove, Keep(KeepReason) }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepReason { Unsafe, Unreadable, Live, Newest, LatestSuccess, FollowUpPending, Graded,
                      CandidateTrial, UnknownSend, StartKey, ConcurrencyKey, KeptChild, Young }
impl KeepReason { pub fn as_str(self) -> &'static str; } // "latest success", "kept child", ...
pub fn plan(root: &Path, settings: &RetentionSettings, only: Option<&str>, now_ms: u64)
    -> Result<Vec<PruneRow>, EngineError>;
pub struct PruneReport { pub removed: usize, pub kept: usize, pub bytes: u64, pub rows: Vec<PruneRow> }
pub fn apply(root: &Path, settings: &RetentionSettings, only: Option<&str>, now_ms: u64)
    -> Result<PruneReport, EngineError>;
```

`only` is a playbook id (`--playbook`). `Live` covers paused and queued
runs too (any fold that is not terminal, or a live driver claim).

Containment first. `<root>/.apb/runs` must be a real directory (not a
link; `symlink_metadata`); an absent one is an empty plan. Every entry
listed must be a directory (not a link, not a file) whose name is a safe
run id (`registry::is_safe_segment`). Anything else is `Keep(Unsafe)`
and never read, renamed or removed. Every path is built under one of the
two anchors (`<root>/.apb/runs`, `<root>/.apb/runs-archive`) and checked
with `apb_core::fsutil::ensure_no_symlink_below(anchor, path)`
(`crates/apb-core/src/fsutil.rs` line 144); archive files are written
with `fsutil::atomic_write_private_under` (line 428) and the archive
directory created with `fsutil::create_dir_under` (line 398). When
`<root>/.apb/runs-archive` exists and is a link, `apply` refuses with
`EngineError::Invalid("runs_archive_unsafe: .apb/runs-archive is a link;
nothing was pruned")` and `plan` marks every row `Keep(Unsafe)`. A child
id read from a journal (`child_run_started`) is used only when it is a
safe segment, its directory passes the same checks, and its own
`RunConfig.parent_run` names the parent; otherwise the parent is
`Keep(Unsafe)`.

Keep rules, in this order (the first that holds is the reason shown),
applied to every run of a family (a top-level run and all its
descendants, at any depth):

1. `Unreadable`: the journal does not read with the strict
   `crate::event::read_all` (`crates/apb-engine/src/event.rs` line
   3643): a torn tail, an unparsable line, or an event of an unknown
   type after the last checkpoint. An unknown event before a checkpoint
   reads (that is the journal contract) and does not keep the run. Why
   not `read_all_for_stop` (line 3660): it accepts an unknown event
   after the last checkpoint only because a stop appends nothing but an
   abort; a removal destroys whatever that event recorded.
2. `Live`: the fold is not terminal, or
   `liveness::driver_alive(run_dir, run_id) == Some(true)`.
3. `Newest`: among the newest `keep_last` runs of its playbook (scope
   and id), whatever their outcome.
4. `LatestSuccess`: the run `crate::previous_success::find(root, id,
   scope)` (`crates/apb-engine/src/previous_success.rs` line 33, made
   callable from `retention`) returns: the newest successful top-level
   production run, the one `run.previous_success` reads. An eval, a
   child or an external data success never takes its place (that
   function already skips them).
5. `FollowUpPending`: `crate::follow_up::state(playbook, events,
   production)` (`crates/apb-engine/src/follow_up/mod.rs` line 46) is
   `FollowUpState::Pending { .. }`.
6. `Graded`: `runs/<id>/grades/` (`crate::grade::store::GRADES_DIR`,
   `crates/apb-engine/src/grade/store.rs` line 11) holds a file.
7. `CandidateTrial`: the manifest's `candidate_trial` is set and
   `apb_core::candidate_pointer::read_candidate(playbook_dir)`
   (`crates/apb-core/src/candidate_pointer.rs` line 23) still names the
   run's version.
8. `UnknownSend`: `crate::run_outcome::intents::intents_from(events,
   now_ms)` (`crates/apb-engine/src/run_outcome/intents.rs` line 243)
   has an entry with `is_open_unknown()` (line 125).
9. `StartKey`: an entry of the playbook's `StartKeysFile`
   (`signed_store::read`, never a write) with `at_ms` within
   `Policies::start_key_days` of now names it (any state). A start-key
   file that does not verify keeps every run of that playbook as
   `StartKey`: a pruned run would void an entry that refuses a repeat
   of its event, and the next repeat would start a second run.
10. `ConcurrencyKey`: a slot of the playbook's `ConcurrencyFile` names
    it as active or waiting, or a pending stop names it. A slot file
    that does not verify keeps every run of that playbook as
    `ConcurrencyKey`.
11. `KeptChild`: any descendant of the run, at any depth, is kept by
    any rule (1 to 10, or 11 itself). The plan computes it bottom-up
    over the family (`fn family_kept(rows) -> bool`, deepest first), so
    a graded grandchild keeps its parent and its grandparent. Why: an
    earlier draft propagated only rules 1 to 10 one level up, and a
    graded grandchild then made its parent `KeptChild` but not the
    top-level run, which was removed with both.
12. `Young`: started less than `keep_days` ago for a success, less than
    `keep_failed_days` for a failure or an abort.

A family is removed whole or kept whole: when any member is kept, every
member is kept (the top-level run shows its own reason or `kept child`,
the descendants their own reasons or `kept child`). A child is never
removed apart from its parent. Eval runs follow the same rules; scenario
runs (`.apb/scenario-runs/`) are not touched.

Step-started runs need no keep rule: `started_by` in a run's launch
record is a run id the surfaces print and never open
(`run_view::started::started_by_line`), and `started_runs`
(`run_view/started.rs` line 60) scans the directories that exist. A
pruned starter therefore still shows as `started by run <id>` on the
runs it started, and a pruned started run drops out of its starter's
`started runs` list; the chain depth of a later start is read from the
started run's own launch record (`step_start::chain_of`, line 97), which
pruning the starter does not change.

#### The archive and its commit protocol

```rust
// crates/apb-engine/src/retention/archive.rs (new)
/// One run of a removed family; about 400 bytes; never a param value,
/// an output or a reason text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchivedRun {
    pub run_id: String, pub playbook: String, pub scope: String, pub version: String,
    /// `production`, `external_data` or `eval` (the manifest's origin).
    pub origin: String,
    /// The parent run of a child; absent for a top-level run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub started_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<u64>,
    pub outcome: String, // succeeded | failed | aborted
    /// The health class of the run when it was removed: exactly what
    /// `apb_core::health::class_of` answered over its live journal
    /// (`succeeded`, `failed` or `ignored`). Health reads this field and
    /// never re-derives a class from `abort_code`.
    pub class: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aborted_by: Option<String>,
    /// `deadline_exceeded`, `superseded`, `operator_stop`,
    /// `parent_aborted` or `other`; for display only, never the reason's
    /// text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abort_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub active_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub execution: Option<String>,
}
/// One file per removed family: `.apb/runs-archive/<root-id>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchivedFamily {
    pub format: u32, // 1
    /// The top-level run.
    pub root: String,
    /// The root first, then its descendants.
    pub runs: Vec<ArchivedRun>,
    /// `run_origin::tag(ARCHIVE_DOMAIN, <canonical JSON without mac>)`.
    pub mac: String,
}
pub const ARCHIVE_DOMAIN: &str = "apb-run-archive-v1";
/// The lock every reader of the combined view and every archive writer
/// takes, a `DirLock` in `.apb/runs-archive`.
pub const ARCHIVE_LOCK: &str = "archive.lock";
/// The one-prune-at-a-time lock, a `HeldLock` in `.apb/runs-archive`.
pub const PRUNE_LOCK: &str = "prune.lock";
pub fn abort_code(reason: &str, by: Option<PauseBy>) -> &'static str;
pub fn write_summary(root: &Path, family: &ArchivedFamily) -> Result<(), EngineError>;
/// The verified families, read under the archive lock by the caller.
pub fn read_verified_locked(root: &Path) -> Vec<ArchivedFamily>;
pub fn recover(root: &Path) -> Result<(), EngineError>;
/// The combined view: the run ids under `.apb/runs` and the verified
/// archived runs, both listed under one hold of the archive lock.
pub struct RunsView { pub dirs: Vec<String>, pub archived: Vec<ArchivedRun> }
pub fn runs_view(root: &Path) -> Result<RunsView, EngineError>;
```

`abort_code`: `by == Some(PauseBy::Operator)` is `operator_stop`; else a
reason that starts with `run_deadline::DEADLINE_EXCEEDED` is
`deadline_exceeded`, with `apb_core::concurrency::SUPERSEDED` followed
by `:` is `superseded`, equal to `stop::PARENT_ABORTED_REASON`
(`crates/apb-engine/src/stop.rs` line 68) is `parent_aborted`; anything
else is `other`. `class` comes from the same terminal event's
`outcome`, `by` and full reason through `apb_core::health::class_of`,
computed while the journal is still there; that is the one conversion,
so a live run and its summary always have the same class (an earlier
draft let health classify an archived run from `abort_code`, and
`superseded` without its colon read as a failure). The canonical JSON is
`apb_core::signed_store::bytes_of` (`signed_store.rs` line 176) of the
value without `mac`.

The summary is visible before the directories leave, and every reader
counts a run from its directory when it has one: a run is therefore
counted at every moment of a removal, once. Removing one family, in
`apply`:

1. Take the event locks of every member (`fsutil::lock_dir(run_dir,
   stop::EVENT_LOCK)`, parent first, then the children in the order
   their `child_run_started` events appear, depth first), then the test
   barrier `PruneFamilyLocked`, then recheck rules 1, 2 and 5 to 10 for
   every member under those locks (a run resumed, graded, given a send
   or named by a key since the plan); any member kept keeps the family
   and the locks are released.
2. Under the archive lock (`fsutil::lock_dir(.apb/runs-archive,
   ARCHIVE_LOCK)`):
   1. Write `<root-id>.json` (`fsutil::atomic_write_private_under`).
   2. Create `.apb/runs-archive/.removing-<root-id>/` and rename every
      member's directory into it (deepest first, the root last).
   3. The test barrier `PruneAfterStage`.
   4. Rename `.removing-<root-id>` to `.removed-<root-id>` (the commit).
3. Release the archive lock and the event locks (the event lock files
   moved with their directories; a resume racing the prune holds the
   same lock while it journals, and finds the directory gone).
4. `std::fs::remove_dir_all` of `.removed-<root-id>` (it does not follow
   links), outside every lock: a large run directory can take longer to
   delete than the 60 s after which a `DirLock` is stolen.

Recovery (`archive::recover`), at the start of every `apply`, under the
prune lock and then the archive lock:

1. `.removed-<id>`: `remove_dir_all` (a committed removal; nothing to
   undo).
2. `.removing-<id>`: undo: rename every entry back to `.apb/runs/`
   (an entry whose name exists there already is left in place and
   reported as an error), remove the empty staging directory, then
   delete `<id>.json`. The directories are back before the summary
   goes, so no reader misses the run.
3. A `<id>.json` some of whose run ids also have a directory in
   `.apb/runs` and no staging directory (a crash between steps 2.1 and
   2.2): delete it; the directories count.

One prune at a time: `apply` takes `fsutil::try_hold_lock(.apb/runs-archive,
PRUNE_LOCK)` first and refuses `EngineError::Conflict("prune_in_progress:
another apb runs prune is running in this project")` when it is held.
So recovery never undoes a live prune's staging, and two prunes never
plan the same family.

Readers: health facts (D2) and `apb stats` read the combined view
through `archive::runs_view(root)`: under one hold of the archive lock
it lists the run directory names (`read_dir` of `.apb/runs`, nothing
opened) and reads the verified families; then the lock is released and
the caller reads the journals of the listed directories. A listed
directory that is gone when its journal is read (a removal committed
after the listing) is looked up once more in a fresh `runs_view` and
counted from its summary there; when neither has it, it is skipped. An
archived run whose id is in `dirs` is dropped (counted from the
directory). Only `<id>.json` files whose MAC verifies
(`run_origin::verify_tag`, line 142) are read; `.removing-*`,
`.removed-*`, the lock files and every other name are skipped. Health
(D2) reads archived runs with `origin` `production` or `external_data`
and no `parent`; `apb stats` reads those with `origin: production` and
no `parent` (stats skip external data runs, `docs/EXTERNAL-DATA.md`).

`apb stats`: `VersionStats` (`crates/apb-engine/src/run_stats.rs` line
199) gets

```rust
#[cfg_attr(feature = "ts", ts(optional))]
#[serde(skip_serializing_if = "Option::is_none")]
pub archived: Option<ArchivedCounts>,
```

with (in `run_stats.rs`, TS):

```rust
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ArchivedCounts {
    /// Archived production top-level runs of this version.
    pub runs: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub aborted: usize,
    /// The sum over the summaries that recorded tokens; absent when none did.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    /// The sum over the summaries that recorded a cost; absent when none did.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}
```

counted apart from `outcomes` and `spend`. `run_stats::build` (line
403) gets a second argument, `archived: &[ArchivedRun]` (every caller
the compiler lists passes the combined view's archived runs, tests pass
`&[]`): the archived runs are filtered like the live ones (the filter's
playbook, `since_ms` against `started_ms`, origin `production`, no
`parent`), and the version groups are the union of the live groups and
the archived `(playbook, version)` pairs. A version that only has
archived runs gets a `VersionStats` with `runs: 0`, every live figure
at its default, and `archived: Some(..)`. `StatsReport.note` is
`NO_RUNS` only when neither live nor archived runs matched. In
`run_stats/render.rs` `render_text` (line 33), the health lines of D2
come first and are printed even with the note (an overdue routine with
no run is exactly the case the note would hide), then the note or the
versions; each version with `archived` prints `archived: 120 runs (110
succeeded, 8 failed, 2 aborted)`.

Why a summary at all: health counts failures and successes over weeks,
and stats report outcomes and spend over the same span; without it a
prune would make a routine look new. About 150 KB for a daily routine
over ten years. The estimate does not read it: it needs per-node
samples, and `keep_last` keeps the newest runs whole.

#### Command

`apb runs` (`crates/apb-cli/src/main.rs` line 365) gets
`#[command(args_conflicts_with_subcommands = true)]` and a first field
`#[command(subcommand)] action: Option<prune::RunsAction>`, the way
`apb results` does (line 350). Every existing `Command::Runs { .. }`
match arm (lines 1167, 1181, 1186 and the next ones) gets `action: None`
in its pattern, and one new arm `Command::Runs { action:
Some(RunsAction::Prune(args)), .. } => prune::prune_cmd(&root, args)`
goes first. The new module `crates/apb-cli/src/prune.rs`:

```text
apb runs prune [--yes] [--json] [--playbook <id>]
```

Without `--yes`: the plan, nothing written:

```text
remove  digest-1730000000000  digest  2026-09-01  succeeded
keep    digest-1731000000000  digest  2026-10-01  succeeded  latest success
42 runs would be removed (1.3 GB), 18 kept; run again with --yes to remove them
```

With `--yes`, a person's command (D0): `removed 42 runs (1.3 GB), kept
18`. `--json` prints the rows (`PruneRow` as JSON). Exit 0; 2 on an
error. No MCP tool, no HTTP route.

Docs: `docs/STATS.md` "Pruning runs" (what is kept and why, the
archive, the `archived` field); `docs/CI.md` a line that the outside
scheduler may run `apb runs prune --yes` weekly.

Wrong implementations to avoid (tests in
`crates/apb-engine/tests/suite/retention_test.rs` unless named):

1. Removing the latest success, or letting an eval success stand in.
   `the_latest_production_success_stays_over_an_eval_success`.
2. A child's evidence lost. `a_graded_child_keeps_its_parent`,
   `a_pending_follow_up_child_keeps_its_parent`,
   `a_graded_grandchild_keeps_the_whole_family` (an old top-level run,
   its old child, and the child's graded child: all three kept, the
   top-level run with `kept child`; a one-level propagation removes the
   top-level run).
3. Following a planted path. `nothing_outside_the_anchors_changes`: a
   run entry that is a link to a directory outside the project, a
   `child_run_started` naming `../../etc`, and a run entry that is a
   file; the rows say `unsafe` and a hash of the outside directory is
   unchanged. `a_linked_archive_refuses_the_prune`.
4. Removing a run with an unknown send. `an_unknown_send_stays`.
5. A dry run that writes. `a_plan_writes_nothing` (engine) and
   `prune_without_yes_writes_nothing` (CLI).
6. A resume racing the prune. `a_resumed_run_is_rechecked_and_kept`:
   a planned run gets `run_resumed` before `apply`; kept.
   `a_child_resumed_between_plan_and_commit_keeps_the_family`: the hook
   `PruneFamilyLocked` is set, and before it releases the test appends
   `run_resumed` to the old child's journal (the hook runs before the
   recheck, so the test plays the resume that won the lock race); the
   family is kept, parent and child directories unchanged. A recheck
   of the top-level run alone removes both.
   `a_child_graded_between_plan_and_commit_keeps_the_family`: the same
   with a file written into the child's `grades/`.
7. A summary counted twice, missing, or stale.
   `a_crash_after_the_summary_is_undone` (seeded: `<id>.json` and the
   run directory in place, no staging; the next `apply` deletes the
   summary and the run stays), `a_crash_mid_stage_is_undone` (seeded:
   `<id>.json` and `.removing-<id>/` holding the child while the parent
   is still under `.apb/runs`; the next `apply`'s recovery moves the
   child back, deletes the summary, and then plans the family again),
   `a_committed_removal_is_finished` (seeded: `.removed-<id>/`; the next
   `apply` deletes it), `a_reader_counts_a_run_once` (a summary and a
   directory of one id: counted once),
   `a_reader_between_stage_and_commit_counts_every_run` (the hook
   `PruneAfterStage` signals the test and then sleeps 1 s; on the signal
   a thread calls `health::facts::facts` and `archive::runs_view`; when
   they return, every run of the family is counted exactly once; a
   reader that does not take the archive lock, or a summary written
   after the move, misses the family).
8. Two prunes at once. `a_second_prune_is_refused_while_one_runs`: the
   hook `PruneFamilyLocked` holds the first `apply`; a second `apply`
   returns `prune_in_progress` and changes nothing; the first then
   completes.
9. A forged or eval summary counted. `a_copied_summary_is_not_counted`
   (a summary with a changed byte), `an_eval_summary_is_not_production`.
10. A secret in a summary. `the_archive_holds_no_param_value_or_reason`:
    a run aborted with a reason that holds a configured secret value and
    a param value; `abort_code` is `other`, and the summary's bytes hold
    neither.
11. A class that differs once archived.
    `an_archived_run_has_the_class_of_the_live_run`: one run each ended
    `succeeded`, `failed`, aborted `deadline_exceeded` by the engine,
    aborted `superseded:` by the engine, aborted by `apb stop`, aborted
    `parent_aborted`, and one external data run that failed; for each,
    `health::facts` before the prune and after it give the same class
    (a summary classified from `abort_code` turns the superseded run into
    a failure).
12. Unreadable journals. `an_unknown_event_after_the_last_checkpoint_keeps_the_run`
    (kept `unreadable`), `an_unknown_event_before_a_checkpoint_does_not_keep_the_run`
    (removed), `a_torn_tail_keeps_the_run` (kept `unreadable`).
13. Using `.apb/trash/`. `the_trash_is_not_used`:
    `apb_core::versioning::list_trash` (`crates/apb-core/src/versioning/mod.rs`
    line 1044) lists nothing new after an apply.
14. Pruning from a run step. `prune_yes_in_a_run_step_is_refused` (CLI):
    `APB_RUN_ID=x apb runs prune --yes` exits 2 and removes nothing.
15. Keyed runs. `start_key_and_concurrency_runs_stay`: an old failed run
    bound to a live start-key entry and an old run named by a slot are
    kept with `start key` and `concurrency key`;
    `an_unverified_start_key_file_keeps_the_playbooks_runs`.
16. External data. `an_external_data_summary_keeps_its_origin`: the
    summary of a flagged run says `origin: external_data`.
17. Stats over the archive (`crates/apb-engine/src/run_stats/tests.rs`):
    `an_archive_only_version_is_listed` (every run of version 1.0 pruned,
    1.1 live: the report has both versions, 1.0 with `runs: 0` and its
    `archived` counts), `archived_runs_follow_the_filter` (a `--playbook`
    and a `since` filter drop the archived runs they exclude),
    `archived_counts_production_summaries_only`,
    `render_text_prints_the_archived_line`.

### D2. Routine health

#### Schema

`crates/apb-core/src/health.rs` (new; `pub mod health;` in
`crates/apb-core/src/lib.rs`):

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct HealthBlock {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consecutive_failures: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_rate: Option<FailureRate>,
    /// A duration (`26h`): no successful run within it trips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overdue: Option<String>,
    #[serde(default)]
    pub on_trip: OnTrip,
    /// Least time between two delivered trip notices of one rule; default `24h`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureRate { pub at_least: f64, pub window: u32, pub min_runs: u32 }
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnTrip { #[default] Notify, Hold }
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule { ConsecutiveFailures, FailureRate, Overdue }
impl Rule { pub fn as_str(self) -> &'static str; }
pub const DEFAULT_COOLDOWN: &str = "24h";
```

`Playbook` gets, after `concurrency`, `pub health:
Option<crate::health::HealthBlock>`
(`#[serde(default, skip_serializing_if = "Option::is_none")]`). Rules
compared with a median of earlier runs are not in this change (proposal
Out of scope).

```yaml
health:
  consecutive_failures: 3
  failure_rate: { at_least: 0.5, window: 10, min_runs: 5 }
  overdue: 26h
  on_trip: hold      # or notify (default)
  cooldown: 24h
```

V212 (error), `crates/apb-core/src/validate/health.rs` `check_health`:
no rule set; `consecutive_failures` outside 1 to 100;
`failure_rate.at_least` outside (0, 1]; `window` outside 2 to 200;
`min_runs` outside 1 to `window`; `overdue` or `cooldown` not a duration
(`apb_core::duration::parse_duration_str`,
`crates/apb-core/src/duration.rs` line 14), under 10 minutes or over 90
days. One issue per broken field, each `V212: health.<field> ...`.

#### The facts a rule reads

```rust
// crates/apb-core/src/health.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFact { pub run_id: String, pub started_ms: u64, pub ended_ms: Option<u64>, pub class: RunClass }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunClass {
    Succeeded,
    /// `run_finished failed`, or aborted by the engine or the supervisor
    /// (a deadline, a quota, a budget), except a `superseded:` abort.
    Failed,
    /// Aborted by a person, superseded, or not ended: never counted.
    Ignored,
}
impl RunClass {
    /// `succeeded`, `failed` or `ignored`: the archive's `class` field.
    pub fn as_str(self) -> &'static str;
    pub fn parse(s: &str) -> Option<RunClass>;
}
pub fn class_of(outcome: &str, aborted_by: Option<&str>, reason: Option<&str>) -> RunClass;
/// `counted_since_ms`: facts started before it do not count (set by a
/// clear). `overdue_from_ms`: the `overdue` reference without a success.
pub fn evaluate(block: &HealthBlock, facts: &[RunFact], counted_since_ms: Option<u64>,
                overdue_from_ms: u64, now_ms: u64) -> Vec<RuleVerdict>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleVerdict { pub rule: Rule, pub tripped: bool, pub detail: String }
```

`class_of`: `succeeded` is `Succeeded`; `failed` is `Failed`; `aborted`
with `aborted_by` `operator` or absent (an older journal) is `Ignored`,
with a reason starting `superseded:` is `Ignored`, else `Failed` (a
`deadline_exceeded` abort by the engine is a failure: the routine did
not finish in its time); anything not ended is `Ignored`. It is the one
classification: a live run's fact and an archived run's `class` field
(D3) both come from it.

Rules, over the counted facts (`Succeeded` and `Failed`, newest start
first), each started at or after `counted_since_ms` when it is set:

1. `consecutive_failures: N` trips when the newest N counted facts are
   all `Failed`. Detail: `N failed in a row, latest <run_id>`.
2. `failure_rate` trips when, over the newest `window` counted facts and
   with at least `min_runs` of them, `failed / counted >= at_least`.
   Detail: `6 of 10 failed`.
3. `overdue: D` trips when `now - reference > D`, where `reference` is
   the newest counted `Succeeded` fact's `ended_ms`, else
   `overdue_from_ms`. Detail: `no success since <UTC>`
   (`apb_core::clock::iso_utc`).

The first evaluation of a playbook counts every fact there is (no
`counted_since_ms`): a routine's first three failed runs trip
`consecutive_failures: 3` when the third ends, although the health
state file did not exist before the first run ended. Why not "only runs
started after the first evaluation" (an earlier draft): the first
evaluation happens when the first run ends, so that run, started before
it, never counted, and the third failure did not trip. `overdue` takes
its grace from `overdue_from_ms` instead: the first evaluation's time,
or the last clear's.

Facts come from `crates/apb-engine/src/health/facts.rs` `facts(root,
scope, id) -> Vec<RunFact>`, over `retention::archive::runs_view(root)`
(D3), so a removal in progress never hides a run:

1. Each listed directory whose `concurrency::checked_run(root, scope,
   id, run_id, None)` is `Valid`, whose manifest origin
   (`crate::manifest::RunOrigin::of_read`, `manifest.rs` line 414) is
   `Production` or `ExternalData`, which was created here
   (`crate::run_history::created_here`, as `all_runs_of`, `run_history.rs`
   line 144, filters) and whose `RunConfig.parent_run` is absent. The
   class from `class_of` over the fold's status and the terminal
   event's `by` and full reason; `started_ms` from `run_started.ts`,
   `ended_ms` from the terminal event's `ts`. `Unreadable` and `Void`
   runs are not facts. A new `crate::run_history::operational_runs_of(roots,
   id)` next to `all_runs_of` does this listing, with the origin filter
   accepting `Production` and `ExternalData`.
2. Plus the archived runs of the view with `origin` `production` or
   `external_data`, no `parent`, and `playbook`/`scope` matching, their
   class read from `class` (`RunClass::parse`; an unknown word is not a
   fact); an id also listed as a directory is skipped.

A start that failed in preparation before `run_provenance` was journaled
(a refused profile, a missing connector) is not a fact: the run check
reads its absent `run_provenance` as `Void`, and nothing says in which
scope it ran. Why that is enough: the scheduler that started it got the
refusal back at once, and a routine whose starts keep failing that early
has no success either, so `overdue` trips. A run that failed after
`run_provenance` (preparation finalized it, `prepare.rs` line 119; it
was never handed to a driver, `PreparedRun::abandon`, `entry.rs` line
378; any later end) is a fact like any other.

Why external data runs count here while statistics skip them: health is
operational, like a notification or the follow-up check, which
`docs/EXTERNAL-DATA.md` keeps for flagged runs. It stores a run id, a
class and times, nothing the run read or produced, and a routine that
processes third-party data on every run (the usual reason for the flag)
would otherwise look like it never runs, and `overdue` would trip every
day.

#### The state file and its ledger

A `signed_store::SignedDoc` in `crates/apb-engine/src/health/state.rs`
(`MAC_DOMAIN = "apb-health-v1"`, `SCHEMA = 1`, `DIR = "health"`,
`ENTRIES_KEY = "notices"`; `.apb/health/<scope>/<playbook>.json`):

```rust
pub struct HealthFile {
    pub schema: u32, pub scope: String, pub playbook: String,
    /// The first evaluation's time: the `overdue` reference before any
    /// success and before any clear.
    pub first_seen_ms: u64,
    /// Set by a person's clear: facts started before it do not count,
    /// and `overdue` measures from it without a later success.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub counted_since_ms: Option<u64>,
    /// Raised by every clear, mute and unmute: a claim made under an
    /// older generation is never dispatched and never settles rule state.
    pub generation: u64,
    #[serde(default)] pub rules: BTreeMap<String, RuleState>,   // by rule name
    #[serde(default, skip_serializing_if = "Option::is_none")] pub hold: Option<Hold>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub muted_until_ms: Option<u64>,
    /// The notice ledger, oldest first, at most 50 (the oldest records in
    /// a final state dropped first).
    #[serde(default)] pub notices: Vec<NoticeRecord>,
    pub evaluated_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub mac: Option<String>,
}
pub struct RuleState {
    pub tripped: bool,
    /// When the current trip began; meaningful while `tripped`.
    pub since_ms: u64,
    pub detail: String,
    /// The trip notice of the current trip: its id, once one was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub trip_notice: Option<String>,
    /// When a trip notice of this rule was last delivered. Kept across a
    /// recovery (the cooldown spans trips); removed only by a clear.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub delivered_ms: Option<u64>,
}
pub struct Hold { pub since_ms: u64, pub rule: String, pub detail: String }
pub struct NoticeRecord {
    pub id: String,           // "h-" + `marker::new_nonce()`, immutable
    pub rule: String,
    pub kind: String,         // "trip" | "recovery"
    pub recorded_ms: u64,
    pub subject: String,      // scrubbed, immutable
    pub text: String,         // scrubbed, immutable
    /// The target when recorded, with its connector and account digests
    /// (`NotifyTargetRecord`, `crates/apb-engine/src/notify/record.rs`
    /// line 12, already carries `connector_digest` and `account_digest`).
    pub target: NotifyTargetRecord,
    /// pending | claimed | sent | failed | unknown | retired
    pub state: String,
    /// While `claimed`: the claiming process's nonce, the time and the
    /// file's generation at the claim.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub claimant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub claimed_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub claim_generation: Option<u64>,
    pub tries: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub detail: Option<String>,
}
```

The per-process nonce is `apb_core::connector::marker::new_nonce()`
(`crates/apb-core/src/connector/marker.rs` line 135), generated once per
process and kept in a `std::sync::OnceLock<String>` in
`health/state.rs`; notice ids use the same function.

#### The notice states

Every change of a notice's `state`, and nothing else, is in this table.
A final state (`sent`, `unknown`, `retired`, and `failed` with `tries ==
3`) never changes again; `detail` may be added to a `retired` record by
a late settle (row 9).

| # | From | To | When |
|---|---|---|---|
| 1 | (none) | `pending` | phase 1 records a notice (a target exists) |
| 2 | `pending` | `claimed` | phase 1, not muted |
| 3 | `failed`, `tries < 3` | `claimed` | phase 1, not muted |
| 4 | `claimed` | `unknown` | phase 1 finds `claimed_ms` older than 60 s (its sender died mid-send); never sent again |
| 5 | `claimed` | `sent` / `failed` (`tries + 1`) / `unknown` | phase 3 by its claimant, `claim_generation == generation` |
| 6 | `pending`, `claimed`, `failed` | `retired` | a clear, a mute, or an unmute (each raises `generation`) |
| 7 | `pending`, `claimed`, `failed` trip notice | `retired` | phase 1, its rule recovered |
| 8 | `claimed` | (no call) | phase 2's dispatch check fails: the record is left as it is (row 6 already changed it, or another process owns it) |
| 9 | `retired` | `retired` with `detail` | phase 3 of a request that was in flight when row 6 retired it: `detail = "outcome after retirement: sent"` (or `not sent: <why>`, `unknown: <why>`); no rule state changes |

#### One evaluation

`crates/apb-engine/src/health/mod.rs` (new; `pub mod health;` in
`crates/apb-engine/src/lib.rs`):

```rust
pub fn evaluate(root: &Path, scope: Scope, id: &str, now_ms: u64) -> Result<HealthView, EngineError>;
pub fn sweep(root: &Path, now_ms: u64) -> Vec<(String, Result<HealthView, EngineError>)>;
pub fn view(root: &Path, scope: Scope, id: &str, now_ms: u64) -> Result<Option<HealthView>, EngineError>; // no write, no send
pub fn hold_refusal(root: &Path, scope: Scope, id: &str) -> Result<(), serde_json::Value>;
pub fn clear(root: &Path, scope: Scope, id: &str, now_ms: u64) -> Result<(), EngineError>;
pub fn mute(root: &Path, scope: Scope, id: &str, until_ms: Option<u64>, now_ms: u64) -> Result<(), EngineError>; // None: unmute
pub fn doctor_line(root: &Path, now_ms: u64) -> Option<(bool, String)>; // (warn, detail)
pub(crate) fn observe_ended(run_dir: &Path);
```

"Watched": the playbook's `current` version has a `health:` block and
`Policies::health_watches(&PlaybookKey)` passes it (below). A playbook
that loses its block keeps its state; `view` says `unwatched`, and its
hold still refuses until a person clears it. `sweep` evaluates every
watched project playbook and every watched global playbook whose runs
live in `root`. "Muted": `muted_until_ms` is set and later than now.

`evaluate`, phase 1, in one `transact(.., Unsigned::Refuse, ..)` (an
`Unverified` file is answered with the held view of D0 and nothing
else); the facts are read before the transaction:

1. First evaluation (no file): `first_seen_ms = now`, `generation = 0`.
2. Per verdict of `apb_core::health::evaluate(block, facts,
   counted_since_ms, counted_since_ms.unwrap_or(first_seen_ms), now)`:
   1. `ok -> tripped`: `tripped = true`, `since_ms = now`, `detail`,
      `trip_notice = None` (`delivered_ms` kept).
   2. `tripped -> tripped`: `detail` updated.
   3. `tripped -> ok`: when not muted and the trip notice is `sent`, a
      recovery notice is recorded (step 5); an outstanding trip notice
      (`pending`, `claimed`, `failed`) is `retired` (table row 7);
      `tripped = false`, `trip_notice = None`, `delivered_ms` kept. A
      recovery while muted sends nothing.
3. A tripped rule whose `trip_notice` is `None` or names a `retired`
   notice gets a trip notice (step 5) when the playbook is not muted
   and `delivered_ms` is absent or `now - delivered_ms >= cooldown`.
   Otherwise nothing now: a later evaluation records it once the mute
   or the cooldown is over and the rule is still tripped. A recovery
   notice is never held back by the cooldown.
4. `on_trip: hold` and any rule tripped with no `hold`: set `hold`.
5. Recording a notice: build subject and text (below), scrub them with
   `apb_core::scrub::Scrubber::for_workspace(root, Vec::new())`, take the
   target (below), append `state: pending, tries: 0, recorded_ms: now`
   (row 1). A notice is recorded only when a target exists; without one
   the rule still trips. A trip notice's id goes into `trip_notice`.
6. Rows 4, then 2 and 3 (only when not muted): every `claimed` notice
   with `claimed_ms` older than 60 s becomes `unknown`; every `pending`
   notice and every `failed` one with `tries < 3` becomes `claimed` with
   this process's nonce, `claimed_ms = now`, `claim_generation =
   generation`.
7. `evaluated_at_ms = now`; write; release the lock.

Phase 2, outside the lock, for each notice this process claimed in step
6:

1. The test barrier `HealthBeforeDispatch`.
2. Dispatch check, one transaction: the record is still `claimed` by
   this nonce, `claim_generation == generation`, and the playbook is not
   muted; otherwise skip it (row 8, no call).
3. `send` (below).
4. The test barrier `HealthBeforeSettle`.

Phase 3, one more transaction per settled notice: when the record is
still `claimed` by this nonce and `claim_generation == generation`, row
5; on `sent` of a trip notice whose rule's `trip_notice` is this id, the
rule's `delivered_ms = now`. When the record is `retired`, row 9. Any
other state is left alone. A request already dispatched is never
recalled: a clear or a mute during it only decides that its outcome
changes no rule state.

This is the send ledger's discipline (recorded before it leaves, claimed
by one sender, a result that is not known never repeated) on a
per-playbook file. Why not a run's journal: `overdue` trips with no run,
a recovery may come from `apb health check` with no run either, and D3
may remove the run that tripped it.

#### Sending

`crates/apb-core/src/notify/mod.rs` `Transition` (line 25) gets `Health`
(`"health"`), appended to `ALL` (line 48, now 9 entries) and `DEFAULT`
(line 63, now 5), doc "A watched routine tripped a health rule or
recovered; never about one run, so a run's own subscription does not
narrow it." Every exhaustive `match` on `Transition` the compiler lists
gets an arm (the run-level sweep never produces `Health`).

`crates/apb-engine/src/notify/health.rs` (new; `pub mod health;` in
`notify/mod.rs`):

```rust
/// The target a health notice of `playbook` goes to: the plan of
/// `notify::start::check(root, Some(playbook), Some(&NotifyRequest::On {
/// target: None, on: None, finished: None }))` (`notify/start.rs` line
/// 91, the same request `notify::trial::send_notice` makes, so
/// `notify.default` does not matter), when its `on` contains
/// `Transition::Health`, `Policies::covers(playbook)` passes it
/// (`notify.playbooks`) and the project's notify config is not
/// `enabled: false`. The record carries the connector and account
/// digests that pin it.
pub fn health_target(root: &Path, playbook: &PlaybookKey<'_>) -> Option<NotifyTargetRecord>;
pub enum SendOutcome { Sent, NotSent(String), Unknown(String) }
pub fn send(root: &Path, target: &NotifyTargetRecord, notice: &Notice) -> SendOutcome;
```

`send` uses one new connector entry,
`crate::connector::call::play_call_pinned(root, connector, account,
function, args, pins: &TargetPins, max_secs: u64) -> (Value, bool)` in
`crates/apb-engine/src/connector/call/mod.rs` next to `play_call` (line
1033): it is `prepare_play_call` with two differences, the loaded
`LiveTarget`'s `pins()` (`connector/call/target.rs` line 171) must equal
`pins` (else a `CallError` with `CallErrorCode::Permission` and the
message `target changed since the notice was recorded`, before any
request), and `CallMode.max_secs = Some(max_secs)`. Loading the target
once for both the pin check and the call leaves no gap between them.
`send` calls it with the record's digests (as `notify/send.rs`
`pins_of`, line 338, builds them), `args` from `notify::send::args_for`
(as `notify/trial.rs` does) and `max_secs =
notify::send::SEND_TIMEOUT_SECS` (line 43). The answer is classified with
`crate::connector::effect::classify(facts, false)`
(`connector/effect.rs` line 36) over `CallFacts { outcome, http_status,
response_complete }` read from the JSON: `outcome` is `"ok"` when the
call succeeded, else `error.code`; `http_status` is `status` or
`error.http_status`; `response_complete` is `Some(false)` when
`truncated` is true or the code is `incomplete_response`, else `None`.
`WriteOk` and `WriteIncomplete` are `Sent`, `WriteUnknown` is
`Unknown`, `Failed` (and a `Permission` refusal, which includes the pin
check) is `NotSent`, as the send ledger settles them.

`notify::trial::send_notice` keeps its behaviour; it does not become the
health sender (it reloads the live target without pins and collapses
outcomes to strings).

The notice: `crate::notify::notice::health_notice(playbook: &str, rule:
&str, kind: &str, detail: &str, held: bool, id: &str) -> Notice` in
`notify/notice.rs` (next to `Notice`, line 31): subject `<playbook>:
<rule> tripped` or `<playbook>: <rule> recovered`; text: the detail,
`on hold: apb health clear <id>` when held, and `apb runs --playbook
<id>`.

#### Hold

1. The gate: `crates/apb-engine/src/gate/mod.rs` `check_run` (line 690),
   right after `check_lifecycle` (line 729):
   `crate::health::hold_refusal(root, scope, &wref.id)?` with `scope` =
   `Scope::Global` for `Origin::Global` and `Scope::Project` otherwise:

   ```json
   { "policy": "playbook_on_hold", "detail": "playbook `digest` is on hold since <UTC>: consecutive_failures (5 failed in a row, latest digest-...); a person clears it with `apb health clear digest`", "rule": "consecutive_failures", "since": "<UTC>" }
   ```

2. Preparation, authoritative: `prepare_placed`, in step 1 of D1's
   preparation order (after the worktree resolution, before the
   start-key claim), for a top-level production start (the same test as
   D1) whose `opts.launch` record's `kind` is not `"trial"`, calls
   `hold_refusal` again and refuses with `EngineError::Conflict(<detail>)`
   prefixed `playbook_on_hold: `; a hold that appeared after the gate
   therefore still stops the start, and so does any direct engine start.
   Nothing is claimed or written.
3. `next_action::start::policy_actions`
   (`crates/apb-engine/src/next_action/start.rs`, the match at line 220)
   gets an arm for `playbook_on_hold`: `vec![of.fix(Remedy::Manual {
   detail: format!("apb health clear {id}") }, <detail>)]`. The fit
   report does not report a hold (it describes this machine's setup;
   the hold is a person's decision the gate already refuses with its
   own action), so `FIT_CODES` (line 392) is unchanged.
4. `check_trial_launch` (line 1019) does not refuse it: a trial is how a
   person checks a fix before clearing. Children are not refused by
   their own hold.
5. A keyed start (`--start-key`): the surface's start-key `lookup` comes
   before the gate, so a repeat of an event that already started a run
   still answers that run while the playbook is held; a new key is
   refused `playbook_on_hold` and leaves no entry. A run step starting
   a held playbook is refused like any start.

#### Commands

`crates/apb-cli/src/health.rs` (new), `Command::Health` in
`crates/apb-cli/src/main.rs`:

```text
apb health [<playbook>] [--json]        the state and a read-only evaluation
apb health check [<playbook>] [--json]  evaluate now (notices go out); exit 0 a start would be admitted, 3 on hold, 2 error
apb health clear <playbook>             a person: drops the hold and every trip, starts the count and overdue over
apb health mute <playbook> --until <time|duration>
apb health unmute <playbook>
```

`<playbook>` is `<id>` or `global:<id>`; without it, every watched
playbook of the project. `check` prints tripped rules on stderr and
exits 0 when nothing is held: a failing routine keeps trying, so it can
recover; only a hold stops the scheduler. In one transaction each:

1. `clear`: `generation += 1`, `hold = None`, `rules` emptied (the
   cooldown starts over too), `counted_since_ms = now`, every `pending`,
   `claimed` or `failed` notice `retired` (row 6).
2. `mute --until`: `generation += 1`, `muted_until_ms` from
   `apb_core::duration::parse_until` (`duration.rs` line 73), every
   `pending`, `claimed` or `failed` notice `retired` (row 6). A rule
   still tripped when the mute ends gets a new trip notice (phase 1
   step 3).
3. `unmute`: `generation += 1`, `muted_until_ms = None`, row 6 as for a
   mute (a claim made before the unmute was made while muted rules were
   still counted as open; it is never dispatched).

#### Evaluation points

1. After every terminal event of a run is durable, never before, from
   one place: `EventLog::append_inner` (`crates/apb-engine/src/event.rs`
   line 2808). After the line is written and its journal lock released,
   when the payload was `RunFinished` or `RunAborted` and
   `self.run_dir()` (line 2625) is `Some`, it calls
   `crate::health::observe_ended(&dir)`. Why one place and not the
   callers: the terminal events are written by the drive, the stop of a
   dead run (`stop.rs` line 265), the preparation failure
   (`finalize_prepare_failure`, `prepare.rs` line 119), a run never
   handed to a driver (`PreparedRun::abandon`, `entry.rs` line 378), the
   close-out of a queued run (`entry.rs` line 267), the drive entries'
   backstops (`entry.rs` lines 463 and 652), a resume, a patch and the
   pause paths; a list of call sites misses the next one. `run_end::record`
   (`run_end.rs` line 8) is not the place: `finish_hooks` calls it
   before `run_finished` is written (`scheduler/finish_hooks.rs` line
   26). `observe_ended` returns at once, before any health read, for a
   run whose run config has `parent_run`, whose manifest origin
   (`RunOrigin::of_read`) is neither production nor external data,
   whose journal has no `run_provenance` (a preparation failure before
   it, see "The facts a rule reads"), or whose playbook is not watched;
   it prints errors on stderr and never fails the append. The stop of a
   dead run holds that run's event lock while this runs; the run is
   terminal, so only another stop or a resume of it waits, at most the
   notice sends of one playbook (`SEND_TIMEOUT_SECS`, 15 s, each).
2. `apb health check`.
3. `apb resume --all` (`crates/apb-cli/src/run.rs` `resume_all_cmd`,
   line 2733): after its loop, `apb_engine::health::sweep(root, now)`;
   one line per tripped rule, `health: <playbook>: <rule> tripped
   (<detail>)`.
4. The dashboard: a new `crates/apb-server/src/health_sweep.rs` with
   `pub(crate) fn spawn()`, started in `run_server`
   (`crates/apb-server/src/lib.rs` line 346) right after
   `cloud_refresh::spawn` (line 404) and built like it
   (`crates/apb-server/src/cloud_refresh.rs`: a `tokio::spawn` loop with
   the work in `spawn_blocking`). It sleeps 60 s, then every period runs
   `sweep_once(roots, now)` over `apb_core::projects::list_active()`
   (`crates/apb-core/src/projects.rs` line 899). The period is 10
   minutes; `APB_HEALTH_SWEEP_MS` overrides it in test builds through
   `apb_engine::env_duration::test_override`
   (`crates/apb-engine/src/env_duration.rs` line 33, made `pub` for this
   use), read as `notify/transitions.rs` line 28 reads its knob.

Concurrent evaluations (a run end, the CLI and the dashboard at once) are
safe through the claim in phase 1 and the dispatch check of phase 2.

#### `policy.yaml`

`PolicyFile` (`crates/apb-core/src/policy_file.rs` line 49) gets
`pub health: HealthSection` with

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HealthSection {
    /// `<id>` or `global:<id>`; absent: every playbook with a block;
    /// empty: none.
    pub playbooks: Option<Vec<String>>,
}
```

`Policies::health_watches(&self, playbook: &PlaybookKey) -> bool`: both
files must pass it (the project narrows), matching names the way
`NotifyPolicy::covers` (line 180) does; an empty list is valid here and
passes nothing. `summary` names `health.playbooks [..]` when set.

#### Surfaces

```rust
// crates/apb-engine/src/health/mod.rs
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HealthView {
    /// The playbook id.
    pub playbook: String,
    /// `project` or `global`.
    pub scope: String,
    /// `held` when `hold` is set (or the file does not verify), else
    /// `unwatched` when the playbook is not watched, else `tripped` when
    /// any rule is tripped, else `ok`.
    pub status: String,
    /// One entry per rule the block sets, in the order
    /// `consecutive_failures`, `failure_rate`, `overdue`; empty for an
    /// unwatched playbook and a file that does not verify.
    pub rules: Vec<HealthRuleView>,
    #[serde(skip_serializing_if = "Option::is_none")] #[cfg_attr(feature = "ts", ts(optional))]
    pub hold: Option<HoldView>,
    /// ISO UTC (`apb_core::clock::iso_utc`); absent when not muted.
    #[serde(skip_serializing_if = "Option::is_none")] #[cfg_attr(feature = "ts", ts(optional))]
    pub muted_until: Option<String>,
    /// The newest notice by `recorded_ms`: `<rule> <kind> <state> <ISO UTC
    /// of recorded_ms>`, for example `overdue trip sent 2026-11-10T06:00:00Z`;
    /// absent when the ledger is empty.
    #[serde(skip_serializing_if = "Option::is_none")] #[cfg_attr(feature = "ts", ts(optional))]
    pub last_notice: Option<String>,
}
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthRuleView {
    /// `consecutive_failures`, `failure_rate` or `overdue`.
    pub rule: String,
    pub tripped: bool,
    /// ISO UTC of `since_ms`; absent while not tripped.
    #[serde(skip_serializing_if = "Option::is_none")] #[cfg_attr(feature = "ts", ts(optional))]
    pub since: Option<String>,
    /// The verdict's detail (`3 failed in a row, latest digest-...`);
    /// empty for a rule never evaluated.
    pub detail: String,
}
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HoldView {
    /// ISO UTC of `Hold.since_ms`.
    pub since: String,
    /// The rule that set it; `unreadable` for a file that does not verify.
    pub rule: String,
    pub detail: String,
    /// The command that clears it: `apb health clear <ref>`.
    pub clear: String,
}
```

`view` of a playbook that has neither a `health:` block nor a state file
is `Ok(None)`; with a state file and no block, `status: unwatched` (a
hold still shows as `held`).

1. `apb health`: one line per playbook: `<ref>  <status>  <rules>`.
2. `apb doctor`: `crates/apb-cli/src/doctor.rs` `doctor_env` (line 136),
   after the start keys line (line 158), a `health` line: OK `N
   playbooks watched, none tripped`; WARN `2 tripped (digest,
   crm-followup), 1 on hold (digest)`; WARN for a state file that does
   not verify; nothing when none is watched. The same place gets a WARN
   `concurrency` line for a slot file that does not verify
   (`apb_engine::concurrency::doctor_line(root)`).
3. Dashboard: `GET /api/playbooks`
   (`crates/apb-server/src/routes/playbooks.rs` `list_playbooks`, line
   770) adds `health` to each entry with a view (after `tag_summary`);
   `PlaybookSummary` in `web/src/lib/types.ts` (line 15) gets
   `health?: HealthView` (imported from `api.gen.ts`);
   `web/src/pages/PlaybookList.svelte` shows a badge `on hold` or
   `tripped` from a pure `healthBadge(entry)` in
   `web/src/lib/playbooklist.ts`.
4. Notify words: `web/src/lib/notify.ts` `onWords` (line 66) gets
   `health: 'a routine that stopped working, or recovered'`; the Run
   dialog offers run-level transitions only, so `notify.ts` gets
   `RUN_TRANSITIONS = NOTIFY_TRANSITIONS.filter((t) => t !== 'health')`,
   used where it builds the per-run list (line 22 and line 57), and
   `docs/NOTIFY.md` says a run-level `on` that names `health` changes
   nothing.
5. MCP `playbook_catalog`: like `add_fit`
   (`crates/apb-mcp/src/catalog.rs` line 326), a post-pass `add_health`
   that reads each entry's `ref` and sets `entry["health"]` for a
   watched playbook, run on every full answer; the `unchanged` answer
   (line 293) adds `"held": [<ref>...]` when any playbook is held. Tool
   description: "health.status held: the playbook refuses starts until a
   person clears it; tell the person, never retry".
6. `apb stats` and MCP `playbook_stats`: `StatsReport`
   (`crates/apb-engine/src/run_stats.rs` line 321) gets
   `#[serde(skip_serializing_if = "Vec::is_empty")] health: Vec<HealthView>`
   (TS optional; the watched playbooks the report's playbook filter
   names, every watched one without a filter, whether or not they have
   runs); `run_stats/render.rs` `render_text` (line 33) prints a line
   `health: <ref> <status> (<tripped rules>)` per playbook not `ok`,
   first and also when the report's note is `NO_RUNS` (D3).

TypeScript: register `ConcurrencyRecord`, `ConcurrencyView`,
`ArchivedCounts`, `HealthView`, `HealthRuleView`, `HoldView` in
`crates/apb-server/src/ts_contract.rs` (beside
`PreviousSuccessView::decl`, line 151) and regenerate; `Transition` and
`NOTIFY_TRANSITIONS` regenerate from the existing entries.

Docs: `docs/HOWTO-authoring.md` `## health (a routine across runs)`
right after the deadline section (before the next `##` after line
4349); `docs/NOTIFY.md` the transition, its target rule and the
run-level note; `docs/CI.md` "Before a start: apb health check";
`docs/STATS.md` the field.

Wrong implementations to avoid (tests in
`crates/apb-engine/tests/suite/health_test.rs` unless named). The notify
connector stand-in counts calls and can be told to answer 200, 500, 400,
a body cut short, or to sleep past the timeout; the hooks
`HealthBeforeDispatch` and `HealthBeforeSettle` hold a sender where a
test needs it.

1. A notice per evaluation. `one_notice_per_trip`: three failed runs,
   `consecutive_failures: 2`; three evaluations send one notice.
2. Two senders of one notice. `three_simultaneous_evaluations_send_one_notice`:
   a `failed` notice; `observe_ended`, `evaluate` and `sweep` wait on
   one `Barrier` and then run; the stand-in counts one call, and only
   one settle is written.
3. A resend after a crash mid-send. `a_stale_claim_becomes_unknown`: a
   notice `claimed` 61 s ago by another nonce becomes `unknown`; no call.
4. Health a run behind, or a missed first run.
   `the_third_failure_of_a_new_routine_trips_at_once`: a fresh project,
   no health file and no sweep; three runs of a playbook whose only node
   fails, started one after another with `run`; `consecutive_failures:
   3` is tripped in the state file when the third `run` returns. A first
   evaluation that counts only runs started after it never trips here.
5. A person's stop counted. `a_person_stop_is_not_a_failure`: two failed
   runs and one `apb stop` abort, threshold 3: not tripped.
6. `overdue` tripping a new routine. `overdue_gives_a_new_routine_its_grace`:
   first evaluation at T not tripped; at T + D + 1 min tripped.
7. A hold that the gate alone enforces.
   `a_hold_set_after_the_gate_refuses_the_start`: a hold written after
   `check_run` passed and before `start_detached` prepares: the start is
   refused; a trial is not.
8. A clear that holds again. `a_clear_starts_over_overdue_included`:
   `overdue: 26h` held; `clear`; an evaluation right after is not
   tripped; five old failed runs do not count either.
9. A mute that swallows a trip. `a_trip_while_muted_is_notified_after_unmute`.
10. A recovery for an undelivered trip. `no_recovery_for_an_undelivered_trip`:
    a trip whose notice `failed` three times, then a success: no
    recovery notice.
11. A drifted target. `a_drifted_target_is_not_sent`: the account digest
    changes after the notice is recorded; `NotSent`, no call.
12. Outcome classes. `outcome_classes_settle_the_ledger`: timeout gives
    `unknown`, HTTP 500 gives `unknown`, an accepted response cut short
    gives `sent`, HTTP 400 gives `failed`.
13. A forged state. `a_forged_state_reads_as_held`.
14. An unwatched machine notifying. `an_unwatched_machine_sends_nothing`:
    `health.playbooks: []`; nothing is sent and the view says
    `unwatched`.
15. A child's end evaluated. `a_child_run_end_is_not_evaluated`.
16. Pruning resets health. `health_counts_archived_failures`: five
    failed runs trip `consecutive_failures: 5`; `retention::apply` with
    `keep_last: 2` and `keep_failed_days: 1` (runs dated two days back)
    removes three; still tripped.
17. A deadline abort. `a_deadline_abort_is_a_failure`: three runs ended
    `deadline_exceeded` trip `consecutive_failures: 3`.
18. A superseded run. `superseded_runs_are_not_failures`: two failed and
    two superseded runs, threshold 3: not tripped; the same after the
    superseded runs were pruned (`superseded_archived_runs_are_not_failures`).
19. External data. `external_data_runs_count`: three failed runs started
    with `external_data: true` trip `consecutive_failures: 3`.
20. A terminal writer that skips health.
    `every_terminal_event_is_observed`: a seeded top-level production run
    of a watched playbook (with `run_provenance`), and a `RunFinished
    failed` appended through a bare `EventLog::open(run_dir)`, as any
    writer does: the state file's `evaluated_at_ms` moved. The same for
    `RunAborted` through `stop_run` of a dead run. A hook placed only in
    `drive` fails both.
    `a_preparation_failure_before_provenance_is_not_a_fact`: a start
    refused in preparation after `allocate_run` and before
    `run_provenance` (a profile that does not resolve): no fact, and
    `overdue` still trips at T + D + 1 min.
21. A clear racing a sender. `a_clear_before_dispatch_sends_nothing`:
    the hook `HealthBeforeDispatch` holds the sender after its claim and
    before its dispatch check; the test clears, then releases: the
    stand-in counts zero calls and the notice is `retired`. A sender
    that checks only its process nonce sends it.
    `a_clear_during_a_send_changes_no_rule_state`: the hook
    `HealthBeforeSettle` holds the sender after the call; the test
    clears; on release, the notice is `retired` with `detail` naming the
    late outcome, `rules` is empty, and no `delivered_ms` was written.
    `a_mute_during_a_send_is_honoured`: the same with `mute`.
22. Retry, mute and cooldown. `a_failed_notice_is_not_retried_while_muted`:
    a trip notice answered 400 (`failed`, `tries 1`), then `mute`; two
    evaluations make no call and the notice is `retired`; after
    `unmute`, the next evaluation records a new trip notice and sends
    it. `a_retrip_within_the_cooldown_waits`: a delivered trip notice
    (`cooldown: 24h`), then a success (one recovery notice sent), then
    the rule trips again 1 h later: no trip notice; at 24 h after the
    first delivery the next evaluation sends one.
    `a_recovery_while_muted_sends_nothing`.
23. Every surface refusing a hold, each in its own crate:
    `a_held_playbook_is_refused_by_the_engine` (here: `start_detached`
    is refused `playbook_on_hold`),
    `a_held_playbook_is_refused_over_mcp`
    (`crates/apb-mcp/tests/suite/run_tools_test.rs`: `playbook_run` is
    refused `playbook_on_hold`, `playbook_trial` runs),
    `a_held_playbook_is_refused_over_http`
    (`crates/apb-server/tests/suite/launch_api_test.rs`: `POST
    /api/playbooks/{id}/run` answers 409 with `policy:
    playbook_on_hold`), and `a_held_playbook_is_refused_by_apb_run`
    (`crates/apb-cli/tests/suite/health_cli_test.rs`).

### Code map

New files (none exists on 88b7fd01; checked):

| File | Decision |
|---|---|
| `crates/apb-core/src/concurrency.rs` | D1 schema |
| `crates/apb-core/src/validate/concurrency.rs` | V210, V211 |
| `crates/apb-core/src/health.rs` | D2 schema, `evaluate`, `class_of` |
| `crates/apb-core/src/validate/health.rs` | V212 |
| `crates/apb-engine/src/concurrency.rs` | D1 store, admission, turn, resume, `checked_run`, `lineage`, `doctor_line` |
| `crates/apb-engine/src/retention/mod.rs` | D3 plan and apply |
| `crates/apb-engine/src/retention/archive.rs` | D3 summary, commit, recovery, readers |
| `crates/apb-engine/src/health/mod.rs` | D2 evaluation, views, hold |
| `crates/apb-engine/src/health/facts.rs` | D2 facts |
| `crates/apb-engine/src/health/state.rs` | D2 signed state file |
| `crates/apb-engine/src/notify/health.rs` | D2 target and send |
| `crates/apb-cli/src/health.rs` | `apb health` |
| `crates/apb-cli/src/prune.rs` | `apb runs prune` |
| `crates/apb-server/src/health_sweep.rs` | D2 dashboard sweep |
| `crates/apb-engine/tests/suite/review_torn_test.rs` | D4 |
| `crates/apb-engine/tests/suite/concurrency_test.rs` | D1 |
| `crates/apb-engine/tests/suite/retention_test.rs` | D3 |
| `crates/apb-engine/tests/suite/health_test.rs` | D2 |
| `crates/apb-cli/tests/suite/prune_cli_test.rs` | D3 |
| `crates/apb-cli/tests/suite/health_cli_test.rs` | D2 |

Integration test files are declared in the crate's `tests/main.rs` with
`#[path = "suite/<name>.rs"] mod <name>;` (as
`crates/apb-engine/tests/main.rs` lines 552 and 583 declare
`run_deadline_test` and `start_key_test`).

Changed files:

| File | What |
|---|---|
| `crates/apb-core/src/lib.rs` | `pub mod concurrency; pub mod health;` |
| `crates/apb-core/src/schema.rs` | `Playbook.concurrency`, `Playbook.health` |
| `crates/apb-core/src/validate/mod.rs` | two modules, two calls |
| `crates/apb-core/src/validate/guarded.rs` | `GuardedFeature::Concurrency` |
| `crates/apb-core/src/workspace.rs` | `LOCAL_ENTRIES` |
| `crates/apb-core/src/policy_file.rs` | `runs.keep_*`, `RetentionSettings`, `health:` |
| `crates/apb-core/src/notify/mod.rs` | `Transition::Health` |
| `crates/apb-engine/src/lib.rs` | `pub mod concurrency; pub mod health; pub mod retention;` |
| `crates/apb-engine/src/error.rs` | `EngineError::Superseded` |
| `crates/apb-engine/src/event.rs` | `ConcurrencyRecord`, `RunProvenance.concurrency`, `PreviousSuccessSet`, the `observe_ended` call after a terminal append |
| `crates/apb-engine/src/deadline_hooks.rs` | six new `Hook` variants (D0 "Test barriers") |
| `crates/apb-engine/src/state.rs` | `PreviousSuccessSet` fold |
| `crates/apb-engine/src/previous_success.rs` | `recorded`, `patch_refusal`, `find` visibility |
| `crates/apb-engine/src/run_history.rs` | `operational_runs_of` |
| `crates/apb-engine/src/review.rs` | D4 reader and writer |
| `crates/apb-engine/src/scheduler/prepare.rs` | key, hold, no tree lock for a keyed start, driver claim, admit, confirm, guard, window |
| `crates/apb-engine/src/scheduler/entry.rs` | `wait_turn`, `claim_queued_workdir` answer, close-out, window lookup, deferred supervisor, `Prepared.slot_owner` and `key_queued` |
| `crates/apb-engine/src/scheduler/resume.rs` | `reserve_resume` |
| `crates/apb-engine/src/scheduler/patch.rs` | concurrency patch refusal |
| `crates/apb-engine/src/forward_patch.rs` | `protected_fields` |
| `crates/apb-engine/src/gate/mod.rs` | hold in `check_run` |
| `crates/apb-engine/src/next_action/start.rs` | `playbook_on_hold` arm |
| `crates/apb-engine/src/connector/call/mod.rs` | `play_call_pinned` |
| `crates/apb-engine/src/notify/notice.rs` | `health_notice` |
| `crates/apb-engine/src/env_duration.rs` | `test_override` made `pub` |
| `crates/apb-engine/src/run_view.rs` | `concurrency`, `previous_success` through `recorded` |
| `crates/apb-engine/src/report_html/model.rs`, `sections.rs` | concurrency line |
| `crates/apb-engine/src/run_stats.rs`, `run_stats/render.rs` | `build(runs, archived, filter)`, union groups, `archived`, `health`, health lines before the note |
| `crates/apb-mcp/src/tools/run.rs` | `run_status.concurrency` |
| `crates/apb-mcp/src/catalog.rs` | `add_health`, `held` |
| `crates/apb-cli/src/main.rs` | `Command::Health`, `RunsAction` |
| `crates/apb-cli/src/run.rs` | concurrency line, `resume --all` sweep |
| `crates/apb-cli/src/doctor.rs` | health and concurrency lines |
| `crates/apb-server/src/lib.rs` | `health_sweep::spawn()` |
| `crates/apb-server/src/routes/runs.rs` | `RunDetail.concurrency` |
| `crates/apb-server/src/routes/playbooks.rs` | `health` on the playbook list |
| `crates/apb-server/src/ts_contract.rs` | new TS types |
| `web/src/lib/api.gen.ts`, `web/src/lib/consts.gen.ts` | regenerated |
| `web/src/lib/types.ts` | `PlaybookSummary.health` |
| `web/src/lib/RunDriveNotes.svelte` | the key line |
| `web/src/pages/RunView.svelte` | passes `concurrency` |
| `web/src/lib/journal.ts` | `superseded`, `previous_success_set` |
| `web/src/lib/notify.ts` | `health` words, run-level list |
| `web/src/lib/playbooklist.ts`, `web/src/pages/PlaybookList.svelte` | the badge |
| `docs/HOWTO-authoring.md`, `docs/NOTIFY.md`, `docs/CI.md`, `docs/STATS.md` | docs |
| `CLAUDE.md`, `AGENTS.md` | the code map lines |

### Release notes

The change is not tagged here. List in the final report: the
`concurrency:` block (`concurrency_busy`, `concurrency_own_run`,
`concurrency_key_sensitive`, `superseded:` aborts, a queued routine's
window), routine health (`health:`, `apb health`, `playbook_on_hold`,
the `health` notify transition in the default set, so a machine with a
notify target hears about playbooks with a `health:` block), `apb runs
prune` and the archive, the `policy.yaml` keys `runs.keep_*` and
`health.playbooks` (an older apb reads a file with them as invalid: it
refuses launches through agent channels and creates no default notify
subscription until updated), and the review channel fix (a decision
posted while the drive reads no longer fails the run).

### Implementation notes

Places where building the design met the code, and what was done. No
decision above was changed.

1. D3 readers in a read-only process. `archive::runs_view` takes the
   archive lock, a `DirLock` that writes a lock file; a process in
   read-only mode (`apb mcp --read-only`) may not write it, so its stats
   would fail once an archive exists. A read-only process reads the view
   without the lock. It never prunes, so it cannot race its own removal;
   a removal by another process in the same instant may make that one
   read count a run twice or miss it, which a later read corrects. A
   process that may write always takes the lock.
2. D3 readers with no archive directory. `runs_view` does not create
   `.apb/runs-archive` (a reader writes nothing, and `a_plan_writes_nothing`
   checks it): with no archive directory it lists the run directories,
   checks again that the archive is still absent, and otherwise reads the
   view again under the lock. A removal needs the directory, so a listing
   framed by two absent checks cannot have missed one.
3. D3 recovery conflict. "an entry whose name exists there already is left
   in place and reported as an error": the staging directory and the
   summary are then both kept (the left member is still counted from the
   summary), and `apply` ends with `runs_archive_conflict: ...` naming the
   entries, before planning anything.
4. D3 orphan children. A run whose `parent_run` names a run that no family
   reaches (its parent directory is gone or unreadable and does not list
   it) is kept as `unsafe`: it cannot be shown to belong to a family that
   is removed whole. A child that the parent's journal does not name but
   whose `parent_run` names the parent is put in the parent's family, so a
   family is never split by a journal that missed a `child_run_started`.
5. D3 `newest` ranks every readable run of a playbook and scope, children
   included, by `run_started` time.
6. D3 test 11 (`an_archived_run_has_the_class_of_the_live_run`) reads
   `health::facts::facts`, which is built with routine health; it is
   written with the facts instead of with the other retention tests. Test 7's reader test reads
   `archive::runs_view` first and adds `health::facts::facts` with the
   facts. The stats tests of item 17 read the report through its JSON
   form, so they compile and fail on their assertions before
   `ArchivedCounts` exists.
7. D3 `apb stats` reads the run directories first and the archived runs
   after them: a run removed between the two reads is counted from its
   summary, and a run loaded from its directory is not counted again from
   a summary.
8. D3 reader wait. `fsutil::lock_dir` gives up after about two seconds,
   and a reader of the combined view that gave up during a commit would
   fail its stats or health read. `runs_view` retries the archive lock for
   up to 10 s; a removal holds it only for its summary and its renames.
9. Gate fixes outside the plan. `code-ranker check` found a module cycle
   that the concurrency surfaces closed (`run_view` read the slots through
   `concurrency`, which reaches `run_view` again): the slot store types
   and `read_slots` moved to the leaf `concurrency/slots.rs`, re-exported
   from `concurrency` under their old paths. For the same reason
   `abort_code` lives in `retention/mod.rs`, not in `retention/archive.rs`
   (it reads `stop` and `run_deadline`, and `run_stats` reads the archive),
   and its two tests moved with it. Clippy findings in the committed
   concurrency code (collapsible `if`s, needless borrows, a useless
   `format!`) and unused test imports were fixed.
10. D2 evaluation point and module cycles. `EventLog::append_inner` is the
    one place that observes a terminal event, as decided, but `event.rs`
    may not name `crate::health`: everything depends on the journal, and
    that call closed a cycle through most of the engine (code-ranker). The
    call goes through the leaf `terminal_hook.rs` (`observe`), and
    `health::install()` registers `observe_ended` there. The `apb` binary
    calls it first in `main` (every run, driver, waker, MCP server and
    dashboard process is that binary), and the engine's integration test
    binary calls it in its start-up hook. A process that embeds the engine
    without installing it observes nothing; no shipped one does.
11. For the same reason `hold_refusal` and `reference` live in
    `health/state.rs` and the view types in `health/view.rs` (both leaves,
    re-exported from `health`), and the gate, preparation and `run_stats`
    name them by those paths. The leaf `concurrency/slots.rs` (note 9)
    is the same kind of change.
12. The `playbook_on_hold` refusal also carries `id` (the `<id>` or
    `global:<id>` reference), so the start action names the clear command
    of a global playbook correctly.
13. A recovery notice's detail is `<rule>: no longer tripped`; the verdict
    detail of a rule that is not tripped is empty for
    `consecutive_failures`.
14. A `policy.yaml` that does not read watches nothing (no evaluation, no
    notice); a hold already set still refuses starts.
15. `sweep` evaluates a global playbook only when its runs live in the
    root (a fact there) or its state file does; `apb health` without a
    playbook lists every project playbook with a view and those global
    ones.
16. Tests that could not be built as written: D2 test 12's timeout case
    would wait out the 15 s send timeout, so the timeout and the cut-short
    body are covered by the unit test `classify_maps_each_outcome`, and
    the integration test settles HTTP 500 (unknown), HTTP 400 (failed) and
    a connection closed with no answer (unknown). D2 test 7's trial half
    is `a_held_playbook_is_refused_over_mcp` (the engine has no public way
    to build a trial grant). `stats_shows_health_without_runs` reaches a
    report with no run through a `--since` window in the future: a CLI
    test cannot wait out an `overdue` grace.
