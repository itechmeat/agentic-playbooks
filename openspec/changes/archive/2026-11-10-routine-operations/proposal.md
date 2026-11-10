# Proposal: routine operations

## Why

apb leaves the clock to outside programs: a cron entry, a systemd timer or
a CI job starts a playbook and `apb wait` turns the outcome into an exit
code (`docs/CI.md`, "Any other runner"; the recurring-routines change kept
a scheduler out of scope on purpose). A routine started that way runs
without a person watching. Reading apb against that use found three gaps
this change closes (three more, about overlapping runs, health across runs
and retention, are the follow-up change `routine-health`):

1. A repeated start makes a second run. Every start allocates a new run
   directory (`crates/apb-engine/src/scheduler/prepare.rs` `allocate_run`,
   line 526, called at line 1070); nothing tells a retry of the same
   request from a new one. A CI job that retries a step whose first try
   timed out on the network, or a timer that fires again after a resume
   from sleep while a person also started the routine by hand, runs the
   routine twice and sends its mail twice.
2. A run has no wall-clock limit. The deadline watch exists for eval runs
   only (`crates/apb-engine/src/scheduler.rs` lines 716 to 722,
   `crates/apb-engine/src/run_config.rs` `EvalRunSettings.deadline_ms`,
   line 283), and it lives only while a drive runs. A routine stuck on a
   slow agent, a quota pause or a gate nobody answers holds its tree into
   the next morning, and a digest that arrives at noon is worse than none.
3. A run started from inside a run step is not linked to its starter. The
   launch record names the channel (`run_step:cli`, `run_step:mcp`;
   `crates/apb-core/src/launch.rs` `LaunchRecord.initiator`, lines 52 to
   54) but not which run, node or attempt started it, and nothing bounds a
   chain of runs that start runs: `MAX_SUBPLAYBOOK_DEPTH`
   (`crates/apb-core/src/schema.rs` line 1447) bounds `playbook` nodes
   only.

Every decision serves code work and routines outside code alike:

| Decision | Code example | Non-code example |
|---|---|---|
| D1 start key | a CI job retries `apb run nightly-bump --start-key "$GITHUB_RUN_ID"` after a network timeout; the retry follows the first run instead of bumping twice | a systemd timer plus a manual start after the laptop wakes send the daily digest once: both pass `--start-key digest-2026-11-09` |
| D2 deadline | a release-notes run waiting on a review past 2 hours ends itself instead of holding the tree into the next run | a daily digest with `deadline: 45m` never arrives at noon, even after a quota pause |
| D3 step-started runs | an agent in a triage run starts `fix-issue` for each issue; the fix runs name the triage run and node, and a loop of runs starting runs stops at depth 5 | a mail routine's agent starts the invoice routine for a mail; the invoice run shows which mail run started it |

## What changes

1. Start key (capability `start-key`): `apb run --start-key K`, MCP
   `playbook_run.start_key` and the HTTP start `start_key`. A repeat with
   the same key, playbook and request (every explicit execution-affecting
   argument the caller passed: params, instruction, version, worktree,
   overrides, execution, supervision, cache, parallelism, patch limit,
   compaction, deadline, budget, unattended, shared workdir,
   continued-from, and the effective `external_data` flag, captured by
   the surface before any gate, candidate or prompt changes them)
   answers the existing run
   (`already_started: true`) and starts nothing; the same key with another
   request is refused `start_key_conflict`. Only keyed MACs of the key and
   the request are stored, in a signed per-playbook file under
   `.apb/start-keys/`, for `runs.start_key_days` (default 7); a file that
   does not verify refuses keyed starts instead of forgetting them. A key
   is owned by the preparing process until its run is verifiable, so a
   repeat during a slow preparation answers that run (or
   `start_key_busy` in the moment before it has an id) instead of
   starting a second one. A failed or aborted run keeps its key until
   `--start-key-reuse` or its deletion.
2. Run deadline (capability `run-deadline`): `deadline: <duration>` in the
   playbook and as a run argument, measured from the start without the
   time a person paused the run or it sat ended. It holds in every state:
   driving, admitting work (wherever work actually starts), queued for
   the tree and parked (one waker per run ends a paused, re-paused or
   released run; `apb resume --all` ends or re-arms them after a reboot).
   At expiry no node starts, the attempts in flight get the graceful kill
   of a `--now` pause while closing as aborted, open host tasks are
   closed `cancelled`, children are stopped (also one still being
   prepared), and the run ends aborted `deadline_exceeded`. The stop and
   a resume's grant are decided under the run's one event lock. `apb resume --deadline`
   grants more time; a plain resume past the deadline is refused. A guard
   and a supervisor-patch rule keep the limit in place. Remaining time on
   every run surface and in the next actions. Validator V200.
3. Step-started runs (capability `step-started-runs`): a new start from
   inside a run step records `started_by` (the starting run and its
   workspace, node, attempt, a verified flag and the chain depth) in the
   new run's launch record; every run surface shows it, and the starting
   run lists the runs it started in its own project, derived from those
   records, saying so when the bounded scan may have missed some. A chain
   deeper than 5, across projects too, is refused `start_chain_too_deep`.
   A run started from a step of a run that processes external data
   (`external_data: true`) is flagged too, like a sub-playbook child or a
   `continued_from` successor of such a run already is, on a run, a trial
   and an executed plan alike; an explicit `false` is refused
   `external_data_immutable`.

## Out of scope

1. A built-in scheduler or cron. `docs/CI.md` answers scheduled runs with
   an outside scheduler and `apb wait`; this change makes such starts safe
   (D1) and bounded (D2), not timed.
2. Overlapping runs of one routine, health across runs and run retention:
   the follow-up change `routine-health` (a concurrency key, health rules
   with holds and notices, `apb runs prune`), implemented after this one
   is merged.
3. Runs started by inbound events (a webhook bridge). The HTTP start gets
   `start_key`; receiving, verifying and mapping events is a later change.
4. A pilot slice for list nodes, review-gate scope summaries, typed review
   fields: separate list and review work. A deadline only ends a waiting
   gate; it does not change it.
5. Connector doubles in evals. The eval harness is untouched: eval runs
   keep their own deadline and take no start key.
6. Per-branch worktrees. Creating a worktree per branch is the worktree
   feature's job.
7. Quiet hours for notifications: nothing here sends a new notice.
8. A start key on `apb eval`, `playbook_trial`, `playbook_execute_plan`
   and sub-playbook children. An eval repetition and a trial are a
   person's deliberate re-runs; an executed plan is already single-use by
   its nonce (`crates/apb-mcp/src/server/run.rs` lines 171 and 287); a
   child is started once by its parent's node.
9. A journal event in the starting run for each run its step started. A
   second writer into a live journal adds a lock dependency, and an event
   type an older live driver does not know refuses that driver at the
   journal's tail (`crates/apb-engine/src/event.rs` `check_unknown_settled`,
   line 3675). The new run's write-once manifest, covered by its origin stamp, holds the
   link; the reverse direction is read from it.
10. Routine deadlines in scenario runs. A scenario's timing is scripted;
    it ignores `deadline` and its digest stays as it was.
11. Changing anything for a run that has no `deadline`, starts without a
    start key and is not started from a run step: such runs behave
    exactly as before (`apb stop` keeps its immediate kill).

## Impact

1. Crates: `apb-core` (new `start_key.rs`; `signed_store::transact`;
   `fsutil::try_hold_lock`; schema field `deadline`;
   `LaunchRecord.started_by`; `policy_file` section `runs`;
   `GuardedFeature::Deadline`; `validate/deadline.rs`;
   `workspace::LOCAL_ENTRIES`), `apb-engine` (new `start_key.rs`,
   `run_deadline.rs`, `step_start.rs`; prepare, entry, stop, control,
   pause clock, `pause::interrupt` kinds, deadline watch, waker, resume,
   resume-all, node admission, child start, host transport, connector
   secret resolution, patch, forward patch, gate
   launch, events, run view, next actions, HTML report, driver args,
   `external_data::for_start` and its preparation backstop, the baseline
   read context),
   `apb-mcp` (playbook_run, playbook_trial, playbook_execute_plan,
   run_resume, run_status), `apb` CLI package
   (`apb run` and `apb resume` flags, `__drive-run` and `__pause-waker`
   arguments, `apb runs <id>`, `apb doctor`), `apb-server` (start and
   resume bodies, run detail, ts contract), `web` (run detail notes and
   their wiring, journal lines).
2. Bundled content: none.
3. Journal: additive fields `run_provenance.start_key`,
   `run_provenance.deadline`, `run_resumed.deadline`; no new event type.
   One new guarded feature name after `untrusted_input`: `deadline`. The
   control command `abort` gains an additive `graceful` field.
4. New state under `.apb/` (git-ignored through `LOCAL_ENTRIES`):
   `start-keys/`.
5. Behaviour changes for release notes:
   1. `policy.yaml` accepts the section `runs:`. An older apb reads a file
      with it as invalid (`policy_file_invalid`): it refuses launches
      through agent channels and creates no default notify subscription
      until it is updated.
   2. A run past its `deadline` ends aborted `deadline_exceeded`, also
      while paused or queued; a plain `apb resume` of such a run is
      refused (`deadline_passed`) until `--deadline` grants more time.
   3. A start from inside a run step deeper than 5 in a chain is refused
      `start_chain_too_deep`.
   4. An abort the engine posts for a deadline kills attempts with
      SIGTERM and a grace period, like `apb pause --now`, and closes open
      host tasks `cancelled`; `apb stop` keeps its immediate kill.
   5. A keyed start whose start-key file was changed outside apb is
      refused `start_key_state_unreadable` until a person moves the file.
   6. A run started from a step of a run with `external_data: true` is
      flagged as well; until now it started unflagged unless the step
      passed the flag.
   7. A run with a deadline bounds the baseline check's connector reads on
      the drive thread by the time it has left, so a read with many pages,
      a token refresh or a credential helper cannot carry the run past its
      deadline; a read it cuts short records no snapshot.
   8. A keyed start that arrives while another start with the same key is
      a few milliseconds into its preparation is refused `start_key_busy`
      and may retry.
   9. The list of runs a run's steps started covers its own project only,
      and says when it may be incomplete.
6. Built on main 7e14c9e3 (after the dsh agent, NEAR AI Cloud models with
   external data runs, and Google connectors with OAuth refresh and
   paging). Validator code V200 of the reserved V200 to V209; V210 to V219
   belong to `routine-health`; V220 to V222 are the cloud model rules and
   V223 to V229 stay reserved for them; V187 is the untrusted-output
   warning of the Google connectors change, and V188 to V199 are unused
   and untouched here.
