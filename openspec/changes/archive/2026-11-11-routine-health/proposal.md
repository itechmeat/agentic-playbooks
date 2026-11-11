# Proposal: routine health

## Why

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

## What changes

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

## Out of scope

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

## Impact

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
