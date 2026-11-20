# Design: before running alone

Read `proposal.md` first. Every decision below names its files, its
functions and the line where the code is today. Line numbers are those of
main e76aa42e (people-in-the-loop merged as #288 and archived at
`openspec/changes/archive/2026-11-18-people-in-the-loop`, typed inbox
reads merged as #287 and archived at
`openspec/changes/archive/2026-11-18-typed-inbox-reads`), checked against
that tree. A line is a hint: find a site by the name given next to it,
never by the number alone.

This is the second of two changes cut from one draft. It holds the two
decisions that let a routine prove itself before it runs alone: the list
pilot and early stop (D1) and readiness and probation (D5). Reminders for
open waits (D4 of the draft) moved to the change wait-park in review round
2, whole; this change keeps no part of them (decision 10). The scope at a form gate (D2), typed fields on a gate (D3)
and the derived diagram (D6) are people-in-the-loop, merged before this
change. The decision numbers are kept from the draft so that both changes
name the same decision the same way.

Four sections, built in the order "Carried over from the review of
#288", D0, D1, D5 (tasks.md). The carried-over fix comes first: the
pilot gate posts and consumes decisions through the review channel it
changes. Then the pilot (D1) uses the review request, the channel and the
pending review block people-in-the-loop left on main; readiness and
probation (D5) are independent of it and come after.

Nothing in this document is left to the implementer. Where a choice was
open, it is decided here and the reason is given (section "Decisions
taken in the closure pass" lists every one); tasks.md only orders the
work. Every user-facing string below is final text: copy it.

## Size

85 changed files at most, including the six openspec files (section
"Files" lists them), inside the 70 to 90 the 1.0 plan gives a feature
PR. The closure pass left 78; the fix carried over from the review of
#288 added up to 10 (one source file each in apb-engine
(`scheduler/patch.rs`, `run_doctor/review.rs`) and apb-mcp
(`server/args.rs`), six test files that post on a form gate or hold the
new tests (`review_scope_test.rs`, `review_form_test.rs`,
`loop_edges_test.rs`, `passkey_gate_test.rs`, `review_tool_test.rs`,
`runs_api_test.rs`), and the spec delta `specs/review-scope/spec.md`);
design review round 1 added nine and removed one; round 2 moved the 14
files of the reminders to wait-park, removed the four retention files and
added eight for the probation ledger; round 3 removed one net (the
ledger's writer moved to the terminal observer, the gate walk out of
`graphutil.rs`; section "Files", closing paragraph). Two of the
carried-over test files (`review_form_test.rs`, `loop_edges_test.rs`)
are touched only if their posts on a form gate name no request, so the
count is 83 to 85. There is no cut line: a file the
compiler names beyond the list (a literal construction of a changed
struct) is fixed where it is and counted in the final report.

## Decisions taken in the closure pass

Each is applied in the section it names; the reason is given here once.

1. The pilot gate journals no `review_detail` and `ReviewDetail` gets no
   `pilot` field (D0, D1). On merged main only a `human_review` that uses
   `fields`, `show` or `max_scope` journals a detail
   (`NodeKind::needs_review_detail`, `crates/apb-core/src/schema.rs` line
   643); every other gate writes its request alone, and the channel's
   form checks, `review::form::settle`, the scope re-ask
   (`review_gate::withdraw_if_rescoped`) and the HTML report all key off
   that one predicate. Making a list node a form gate would route the
   pilot through checks built for fields it does not have. The slice, its
   round and its counts are already in the journal (`list_pilot` and the
   items' `list_item_settled`), so `PendingReview.pilot` is computed from
   `ListFold` when the pending block is built. The pilot gate shows no
   work scope, like every plain gate on main; the list progress panel and
   the pending block show the slice.
2. The pilot gate is never withdrawn by a scope change. It has no detail,
   so `withdraw_if_rescoped` returns early for it (it reads the detail's
   scope and finds none), as for every plain gate on main. A supervisor
   directive (`node_retry`, `run_continue_from`) still withdraws it like
   any gate (`ReviewWithdrawn`), and the list node reaching the gate again
   asks anew.
3. An unattended run never continues past a pilot slice with an item that
   needs a look: `pilot.unattended: continue` decides `continue` only when
   every slice item passed, and fails the run `hitl_required` otherwise.
   The point of the change is that a routine proves itself before it runs
   alone; an engine decision to go on past a bad slice would undo it.
   `stop_when` stays the brake for the rest of the visit.
4. `done_defined` counts only a `script` goal criterion under `goal.enforce:
   true`. A `marker` criterion reads the agent's own text, a `list`
   criterion only accounts for items (an item without a check passes by
   itself), and a criterion that is not enforced is reported but never
   fails the run (`scheduler/goal.rs`, `enforced`); D5 counts none of them
   as a deterministic check for a node either, so the two rules agree
   (review finding 6).
5. A scenario may answer a pilot gate (`review` answer on the list node,
   a pilot option). A capture of a run with a list node still names the
   `list_node` gap: its per-item answers are not captured, so a pilot
   decision alone would replay with every item missing. Without it every
   scenario of a playbook with a pilot fails on a missing answer, and the
   bundled scenarios rule (every shipped playbook carries scenarios)
   could never cover a pilot.
6. Next actions annotate a pilot gate's unattended outcome
   (`next_action/unattended.rs`). Without it the `DecideReview` arm reads
   the node-level `unattended:` of a `human_review` and reports
   `hitl_required` for a pilot that declares `unattended: continue`.
7. The `list_pilot` guard is a run guard only, not a manifest guard
   (`manifest::manifest_guard` stays as it is). People-in-the-loop made
   the same choice for `review_fields` and `review_max_scope`: an older
   apb refuses to resume or pause a guarded run while the guard trails
   the last checkpoint. For a list pilot there is no stop-then-resume
   path either for apb 1.0.0-beta.1 and later releases: their `ListSpec`
   denies unknown fields (`crates/apb-core/src/list/mod.rs`,
   `#[serde(deny_unknown_fields)]` on `ListSpec`), so their parser
   rejects a definition with `pilot` or `stop_when` outright; they cannot
   start such a playbook and cannot resume such a run, stopped or not
   (review finding 12). apb 0.25.0's list block does not deny unknown
   fields yet: it starts such a playbook, drops `pilot` and `stop_when`,
   and runs every item of the visit with no pilot gate and no early stop.
   That is the standing contract of an older binary meeting a new
   playbook field (the same as for the top-level `unattended:` block, D5
   "Older apb binaries"), and no guard or mode suffix is added for it
   (review fixes round 1, M8). It still refuses to resume a run another
   apb started at a pilot gate. One journal-corpus row records what each
   old binary does (section "Journal corpus").
8. The `--require-ready` refusal code is `not_ready`, a constant
   `NOT_READY` in `crates/apb-engine/src/run_config.rs` next to
   `PROBATION_REQUIRED`. The word is also the name of a next-action
   field (`next_action/types.rs` line 260, `NextAction.not_ready`); the
   two never meet (one is a refusal `policy`, the other an action field),
   and the later change start-rules-and-schedules already names the
   refusal `not_ready`. The wording of the unready items is one function,
   `fit::readiness::not_ready_detail`, shared by the CLI and that later
   change.
9. A pilot is not an approval: a supervisor directive that moves the run
   past the list node (`run_continue_from` a later node) is not refused
   by `scheduler::approval::skips_gate`, which guards approvals and
   person-only gates only. A pilot node takes no `person_only` and no
   `approves` (V155 to V158 already hold those to gates).
10. Reminders for open waits are not part of this change (review round
    2 moved them, whole, to wait-park): the pilot gate is a review
    request, so once wait-park lands it is reminded like any gate.
11. The re-dispatch of a list node after its pilot gate writes
    `node_started` again, as the resume of an open visit does on main
    (the generic arm of the drive, `scheduler.rs` line 3348): `open_visit`
    resumes the open visit, so no item and no visit is duplicated. No
    special case is added for it.

## Review record

### Round 1

The first design review of this change, each finding checked against
main e76aa42e, and its disposition (items 9 and 10 were superseded in
round 2, see below):

1. Blocker, an older apb ignores the top-level `unattended:` block:
   confirmed (`Playbook` denies no unknown field); recorded as a limit,
   not fixed, in D5 "Older apb binaries" and release note 4; `requires`
   cannot demand a minimum apb, so none is added.
2. Blocker, a parent bypasses a child's probation: confirmed; fixed, every
   pinned child is checked with its own definition at the start and every
   child again where its run is created (D5 "Where it runs").
3. Blocker, the engine check had no definition: confirmed
   (`unattended_start` runs before `reg.load`); fixed, one site in
   `prepare_placed` on the loaded definition, surfaces no longer check
   (D5 "Where it runs").
4. Blocker, a withdrawn pilot request decides a later one: confirmed
   (`review_consumed_count` skips non-scope withdrawals); fixed, pilot
   gates join the stamped consumption of form gates
   (`requires_request`), with the interleaving as a test (carried-over
   decision 1, D1 "The channel", D1 wrong implementation 9).
5. Major, `stop_when` overrides a batch result or a pilot: confirmed;
   fixed, the batch result first, the rule only outside the slice and
   after its gate, the batch cut stated (D1 "The runner", wrong
   implementations 10 and 11).
6. Major, readiness accepts self-reported checks: confirmed; fixed,
   proof is a script success check, a script item check under
   `on_needs_look: fail`, or an enforced script goal criterion (D5
   "What counts as deterministic proof", decision 4).
7. Major, boundaries ignore the effect domain: confirmed; fixed, tree and
   connector domains judged apart, isolation only when enforced (D5
   "Boundaries per effect domain").
8. Major, failure routes bypass the irreversible walk: confirmed; fixed
   through `reachable_with_policies_in` (D5 "The irreversible-gate
   walk"); that walk is replaced in round 3 (item 6).
9. Major, probation neither durable nor machine-wide: confirmed; per
   project root by design (stated in the refusal and docs). The archive
   summary fields of round 1 are withdrawn in round 2: probation now has
   its own ledger (D5 "Probation counting").
10. Major, a reminder sent after its wait closed: confirmed; fixed in
    round 1 by the recheck at send-intent admission. The reminders moved
    to wait-park in round 2 with that decision (and its extension to
    every reminder intent, round 2 major 2).
11. Major, a terminal is not proof of a person: accepted; the claim is
    narrowed to apb's initiator model, the waiver is a typed confirmation
    at an interactive terminal and refused elsewhere (D5 "The waiver").
12. Minor, the older-binary stop-then-resume path: confirmed wrong
    (`ListSpec` denies unknown fields from 1.0.0-beta.1 on); the claim is
    removed and the corpus row asserts what each old parser does,
    0.25.0 dropping the fields (decision 7, "Journal corpus").
13. Minor, a task without a pass criterion: fixed, the release clippy
    criterion is stated, and the crash and concurrent-command cases are
    tests (tasks 6.10 and 3.4).
14. Pre-existing on main, a late decision without a request answering a
    re-asked person-only gate: unchanged, already named as carried-over
    decision 10 and left for a later change.

### Round 2

The second design review, each finding checked against main e76aa42e,
and the owner's decisions on it:

1. Blocker 1, a named decision stamped with another request: confirmed
   (`check_review_request` reads the journal at `review.rs` line 1198,
   `check_bound_decision` reads it again at line 473 and stamps the open
   request it finds there; the drive appends under the event lock, not
   the reviews lock). Fixed: the named seq is passed into
   `check_bound_decision`, compared with the open request on its one
   read, and that seq is stamped; the interleaving is a unit test with a
   test-only hook (carried-over decision 2, test 2, wrong implementation
   5).
2. Blocker 2, one attended run counted twice across a prune: confirmed
   for the round 1 design. Settled by the redesign: probation is counted
   from its own signed ledger, one entry per run id written once by the
   run's end; nothing scans run directories or the archive, and every
   retention and archive edit of round 1 is removed (D5 "Probation
   counting"). Test `one_run_counts_once_across_a_prune`.
3. Blocker 3, a patched run proves the wrong definition: confirmed
   (`patch_applied.definition`, `scheduler/patch.rs` line 167). Fixed by
   the eligibility test: no `patch_applied`, no `profile_rebound`, no
   `run_migrated`, every `run_resumed` on the same definition (D5
   "Eligible: the exact test"). Test `a_patched_run_is_not_eligible`.
4. Blocker 4, `--require-ready` judged a version other than the one
   started: confirmed (the CLI assessed before `gate_run` could select a
   candidate). Fixed: `--require-ready` is `RunOptions.require_ready`,
   enforced by preparation at the probation site on the loaded
   definition; the CLI checks nothing before the gate (D5 "Where it
   runs" step 3). Test `require_ready_judges_the_loaded_version`.
5. Blocker 5, the waiver's confirmation depended on the parent's state:
   confirmed. Fixed: `--waive-probation` always asks its typed
   confirmation at a terminal; the waiver is recorded in the manifest
   (`probation_waived`) and reaches every child start of the tree
   through `unattended_start` (D5 "The waiver").
6. Major 1, a changed script kept the count: confirmed, and round 1's
   proposal text was wrong: `run_provenance.digest` is the trust digest
   (`prepare.rs` line 701, `loaded.trust_digest()`), which covers the
   scripts. Fixed: the ledger entry carries it and the check compares
   it. Test `a_changed_script_resets_the_count`.
7. Major 2, a closed wait gaining a `not_sent` reminder intent:
   confirmed (`Notifier::record_unsent`, `notify/send.rs` line 292,
   appends with `log.append`). Moved with the reminders to wait-park,
   where the journal-locked openness check applies to every reminder
   intent, the `not_sent` ones included.
8. Major 3, an unready child under a ready parent: confirmed. Fixed with
   blocker 4: preparation assesses every pinned sub-playbook and refuses
   naming the child.
9. Major 4, a prompted `finish` cannot declare a boundary: confirmed
   (V137, V170). Readiness lists it as an actor without a local boundary
   (`BoundaryDomain::FinishAgent`, its own item text); nothing is
   extended.
10. Major 5, older binaries bypass probation: rejected again, recorded:
    the format has no minimum-version field, and an older binary dropping
    a field it does not know is the standing contract (D5 "Older apb
    binaries").
11. Minor 1, overridden runs: settled by the ledger: an override makes a
    run not eligible (`run.yaml` overrides), and the check's definition
    equality refuses it too. Test `an_overridden_run_is_not_eligible`.
12. Minor 2, the corpus row had no fixed oracle: fixed. Row G1 asserts
    that 1.0.0-beta.1 refuses to start such a definition, that 0.25.0
    starts it without a pilot gate, and that neither resumes a run
    waiting at the gate or appends to its journal (section "Journal
    corpus").
13. Pre-existing on main, a decision without a request binding to a
    re-asked request: the same finding as carried-over decision 10, left
    for a later change.
14. The owner moved the reminders for open waits (draft D4) to
    wait-park, whole, with the round 1 and round 2 decisions on them;
    this change keeps decision 10 only.

### Round 3

The third design review, each finding checked against main e76aa42e,
and the owner's final decisions on it:

1. Blocker 1, an eligible entry for a run that never succeeded:
   confirmed (the finish hooks run before `run_finished` is appended,
   `scheduler.rs` line 1402, and `stop.rs` line 265 appends `run_aborted`
   for a dead run). Fixed: the entry is written only after the terminal
   event is in the journal, by the journal's terminal observer
   (`terminal_hook.rs`, a second slot beside the health observer), with
   the outcome read from that event; `run_end.rs` and its callers are
   not changed. A crash between the event and the write leaves the run
   uncounted, the safe side; no reconcile pass is added, as the owner
   decided (the review asked for one). An aborted run never has an
   eligible entry. Tests: wrong implementation 14 (D5 "The writer: after
   the terminal event").
2. Blocker 2, a patch bypasses probation and `--require-ready`:
   confirmed (`apply_patch`, `scheduler/patch.rs` line 34, loads and
   installs another version with neither check). Fixed: a patch on a run
   started unattended is checked for probation, and with
   `readiness_required` for readiness, on the replacement definition and
   the run's pins, right after `approval::patch_refusal`; a refusal is a
   `patch_rejected` with the same codes (D5 "A patch is held to the same
   checks", four tests).
3. Blocker 3, `revise` can repeat a send whose outcome is unknown:
   confirmed (`ListFold` folds an interrupted item to pending,
   `list/ledger.rs` line 207). Fixed: the revise transition branches per
   item on the same `ItemSends::unknown` check `requeue_for_retry` makes;
   such an item is never interrupted, keeps `needs_look` and stays in the
   new slice as such; a crash partway through resumes the transition
   once (D1 "The runner" step 4.4, wrong implementation 14).
4. Major 4, an older binary runs a probation-protected playbook
   unattended: rejected a third time, recorded. No machine-policy marker
   is added: an older apb reads a `policy.yaml` with an `unattended:`
   section as invalid already, which fences the machine file and not the
   playbook it would have to protect, and the playbook format has no
   minimum-version field (D5 "Older apb binaries").
5. Major 5, the scenario instructions conflict with list answers:
   confirmed (`expected_kind` returns `ITEMS` first, `taken_kinds` builds
   on it, `scenario/validate.rs` line 225), and one more conflict the
   review did not name: the node-blind V146 shape rule (`too_many_kinds`,
   line 77) refuses `items` with `review` in one visit. Fixed: `ITEMS`
   stays the primary kind, `taken_kinds` adds `REVIEW` for a pilot, the
   V146 pairs take `items` with `review` (and the triple with `judge`),
   `check_values` checks pilot options (D1 "Scenarios", task 3.5 with one
   scenario holding item and review answers).
6. Major 6, the irreversible-gate walk reports impossible paths:
   confirmed (`policy_handlers` seeds without a failing source,
   `graphutil.rs` line 121). Fixed: the walk carries whether a gate was
   passed and follows failure transitions only from reached nodes that
   can fail; `graphutil.rs` is not changed (D5 "The irreversible-gate
   walk", wrong implementation 12 with a failure before and after the
   gate).
7. Major 7, readiness calls an external actor bounded: confirmed
   (`effects.rs` line 63 infers `network` and `external` for every node
   that takes the workdir lock). Fixed: a network domain judged for every
   actor with inferred or declared network or external capability,
   bounded only by an enforced isolation with `network: none`, whatever
   its `read_only` or `writes` (D5 "Boundaries per effect domain").
8. Minor 8, the pilot field refusal cites code that returns early:
   confirmed (`check_form_unscrubbed` returns `Ok(())` for every node
   that is not a `human_review`, `review.rs` line 324). Fixed: an
   explicit refusal for a pilot gate with field values, and a post test
   (D1 "The channel", wrong implementation 15).
9. Pre-existing on main, a decision without a request binding to a
   re-asked person-only gate: the same as carried-over decision 10,
   left for a later change.

### Review fixes, round 2

The code review of the implementation, each finding checked against the
branch, fixed test first, and a sweep for each finding's class:

1. Major, a probation entry took a run with no manifest for an attended
   one: confirmed (`probation.rs` `entry_of` read `unattended` from the
   manifest only, false when absent). Fixed: one rule,
   `manifest::recorded_mode`, establishes a run's mode from a record
   that exists (the manifest, else the persisted `run.yaml`, which
   carries the flag for every run this apb started) and never answers a
   default; an entry whose mode is not established is recorded
   unattended and not eligible. Sweep, the same class: a missing
   `run.yaml` read as a config without overrides (`read_run_config`
   answers the default for a missing file), so eligibility now needs a
   `run.yaml` that exists; a patch on a run whose manifest is gone
   (`scheduler/patch.rs`) took it for an attended run's and checked
   nothing, and is now refused `probation_required` when `run.yaml` says
   unattended, since the waiver and the readiness demand live only in
   the manifest; a child start (`scheduler/prepare.rs`
   `unattended_start`) whose parent's manifest was missing or did not
   read started attended, and is now refused `probation_required` when
   the parent's mode cannot be told or the parent is unattended without
   its manifest; the parent is read from the placement's runs directory,
   where a child's parent lives. Left as it is: an entry's "started from
   a run step" is read from the manifest's launch record only; a start
   through a launch surface always writes it, and a start without a
   grant (one that bypassed every launch surface) records nothing, as
   before (tests: `probation::tests`, `manifest::tests`,
   `a_patch_on_an_unattended_run_without_its_manifest_is_refused`,
   `a_child_of_an_unattended_parent_without_its_manifest_is_refused`).
2. Major, a resumed visit skipped its opening steps: confirmed
   (`scheduler/list/mod.rs` `open_visit` returned for an open visit
   before the carry of `rerun: not_passed`, the parking of unknown sends
   and a reopen's requeue, so a drive that died after `list_started`
   piloted and ran the passed items again). Fixed for every list, a
   pilot or not: `complete_opening` runs the same `opening_steps` when no
   batch, pilot slice or stop follows the visit's latest `list_started`,
   on the journal as it stood before it (the previous visit and the
   sends the first pass saw), skipping every item settled since. Sweep:
   the reopen's requeue had the same gap and is covered by the same
   step; the revise transition already resumes once, and the too-large
   settlement already ran on resume (tests:
   `a_crash_before_the_carry_still_carries_before_the_slice`,
   `a_crash_before_the_requeue_still_reruns_the_items_that_need_a_look`).
3. Minor, printed commands for a gate that requires its request named
   none: confirmed for the pilot's journaled instruction, the pending
   block and the paused-run hint built from it, and the `decide with:`
   line `apb wait` prints for a form gate. Fixed: one helper,
   `people::instruction::review_command`, renders every such command;
   `gate_how_to_decide` takes the request; the pending review carries
   `request` (the open request's seq, only for a gate that must name
   it); the drive writes the request into its own journaled instruction
   at the seq the append settles (`EventLog::append_named`, and
   `append_pair` with the same naming step), so the text needs no
   guess. Sweep: the person-only refusal printed `--request` only for a
   bound request, so a person-only form gate's refusal printed a command
   that was refused `decide_again`; it now goes through the same helper
   and always names the open request (tests:
   `the_pilot_instruction_names_its_request`,
   `a_paused_pilot_gate_hint_names_its_request`,
   `a_waiting_gate_prints_its_scope_show_and_fields`,
   `a_person_only_form_gate_refusal_prints_a_command_that_is_accepted`).

### Review fixes, round 3

1. Minor, a finished pilot with nothing after it read as running:
   confirmed (`list/view.rs` `PilotView::of` read a settled slice
   without a decision as `continued` only once an item outside the
   slice had started, so a dynamic list that yielded no more items than
   the slice under `then: continue` finished clean and every surface
   still showed its pilot as `running`). Fixed: a visit whose list node
   finished `succeeded` went on too. Every surface (the CLI, MCP, the
   HTTP API, the dashboard, the run outcome) reads the state from
   `PilotView::of` only, so the one rule covers them (test:
   `a_finished_clean_pilot_with_no_rest_reads_as_continued`).

## Carried over from the review of #288

Two findings of the review of people-in-the-loop (#288), checked against
main e76aa42e. They are fixed first (tasks section 1), because the pilot
gate of this change posts, counts and consumes decisions through the same
channel. Both are confirmed by the code; neither changes
a plain gate.

### What the code does today

1. A late decision answers a newer request. A gate that uses a form
   feature (`NodeKind::needs_review_detail`,
   `crates/apb-core/src/schema.rs` line 643) consumes its decisions under
   the count rule: the drive picks `Consume::Nth` for every gate that is
   neither person-only nor bound (`crates/apb-engine/src/scheduler.rs`
   line 2440, `review::is_stamped_gate`, `crates/apb-engine/src/review.rs`
   line 421), and `queued_decision` takes the channel entry at
   `review_consumed_count` (`crates/apb-engine/src/scheduler/review_gate.rs`
   line 49), which counts one per `review_decided` and one per
   `scope_changed` withdrawal (`crates/apb-engine/src/event.rs` line
   3404). The post check stamps such an entry with its channel only
   (`check_review_target`, `review.rs` line 270), and checks a request
   only when the caller named one (`append_decision`, `review.rs` line
   1114, into `check_review_request`, line 1195); every surface makes the
   request optional. So: request A of a form gate is open, work lands,
   the drive withdraws A `scope_changed` and opens request B; a person who
   read A posts a decision after B opened and names no request. The open
   count is 1 and nothing is queued past the consumed count, so the post
   is accepted, and the drive takes it as B's decision. `max_scope` is
   checked again at consumption, but it refuses only withheld options: an
   option still allowed on B is consumed although the person saw A's
   scope. Posted with A's seq the same decision is refused today
   (`check_review_request`: "is over; the node now asks at seq N"), so the
   hole is the post without a request.
2. A form gate read as a plain one. When the run's definition snapshot
   (`runs/<id>/playbook.yaml`, `legacy_snapshot::load_run_playbook`) does
   not read, `check_review_target` (the `None` arm, `review.rs` line 177)
   treats the node as a form gate only when the journal holds its
   `review_detail` (line 189). A request whose paired write stopped after
   `review_requested` has none, so a decision without field values is
   queued with no form check, and the resumed drive writes the detail over
   the scope then and consumes that earlier decision by count. With
   decision 5 below such an entry carries no request stamp and is never
   consumed; what is left is a post that answers success for an entry no
   drive takes. Decision 6 makes that post refuse.

### What the stamped path does today, and what is reused

A person-only or bound gate (`is_stamped_gate`: `person_only`, or
`approves`) already consumes only entries stamped with its open request:

1. The post check `check_bound_decision` (`review.rs` line 465) reads the
   open request (`review::open_request`), refuses a second entry that
   already answers it, checks the binding nonce, and returns
   `Stamps { request: Some(open), channel, nonce }`.
2. The drive's `Consume::Stamped` (`review_gate.rs` lines 34 and 61)
   consumes the first entry for which `answers_stamped` (`review.rs` line
   542) holds.
3. `check_review_request` (`review.rs` line 1260) refuses a duplicate on a
   named request by the same predicate.
4. The doctor's `reviews` row (`crates/apb-engine/src/run_doctor/review.rs`)
   lists every entry such a gate never takes ("no request stamp", a
   request it never opened, another binding).

This change reuses all four for a form gate and adds no second mechanism.
The stamped path differs from what a form gate needs in two places, and
the decision covers both: it stamps the open request at post time whether
or not the caller named one (a late decision would be stamped with B
too), and `answers_stamped` also demands the passkey stamp
(`passkey_stamp_ok`), which a form gate that is neither person-only nor
bound must not demand (`passkey_required` is false for it, `review.rs`
line 958).

### The decision

1. Two predicates in `review.rs`. `pub fn requires_request(gate: &Node)
   -> bool`: `gate.kind.needs_review_detail() || gate.list().is_some_and(|l|
   l.pilot.is_some())`, the gates whose decision must name its request (a
   form gate here, a list pilot gate of D1). `pub fn consumes_stamped(gate:
   &Node) -> bool`: `is_stamped_gate(gate) || requires_request(gate)`.
   `is_stamped_gate` keeps its meaning (the gates the passkey and channel
   rules hold) and its callers. One mechanism, three users: person-only
   and bound gates (stamped without being named, as on main), form gates
   and list pilot gates (stamped and named). The pilot gate is added here,
   not in D1, so that the review finding on withdrawn pilot requests
   (review finding 4) is closed by the same code and tests; D1 "The gate
   in the drive" and "The channel" only name it. The pilot clause of
   `requires_request` and the guard name `list_pilot` in item 6 are added
   in tasks section 3, once `ListSpec.pilot` and the guard exist; until
   then the predicate holds for form gates only.
2. A form gate's decision names its request, and the request it names
   is the one it is stamped with, decided on one journal read (review
   round 2, blocker 1). Today `append_decision` (`review.rs` line 1114)
   checks a named request in `check_review_request` on one journal read
   (`read_all_lossy_tail`, line 1198) and then `check_bound_decision`
   (line 465) stamps whatever request is open on a second read
   (`read_all`, line 473); the drive appends to the journal under the
   event lock, not the reviews lock, so it can withdraw request A and
   open B between the two reads, and a post that named A is stamped B.
   The change:
   1. `check_bound_decision` gets a parameter `named: Option<u64>` (the
      post's `request`), passed through `check_review_target`, which
      takes it from `append_decision`. It reads the journal once and
      decides on that read alone: `open_request(&events, node)` gives
      the open request; when `named` is `Some(s)` and `s` is the open
      request's seq, the entry is stamped `s`; when `named` is `Some(s)`
      and `s` is not the open one, `s` is classified on the same events
      (no `review_requested` at `s`: `NotFound`; a later request of the
      node: `decide_again` replaced; `s` decided: `AlreadyAnswered`;
      `s` withdrawn: `decide_again` withdrawn) and nothing is stamped;
      when `named` is `None` on a gate where `requires_request` holds and
      the poster is not the engine (`Poster.engine`; every engine post
      names its request), `decide_again` none named; when `named` is
      `None` on another stamped gate, the open request is stamped as on
      main. The duplicate check and the nonce check run on the same read,
      for the stamped seq. The stamp is therefore always the seq the
      check compared, never a second read's.
   2. In `check_review_target`, the branch that today takes
      `is_stamped_gate` (line 153) takes `consumes_stamped`, so a form
      gate's and a pilot gate's post go through `check_bound_decision`.
   3. `append_decision` calls `check_review_request` only for a gate
      where `consumes_stamped` does not hold (a plain gate, a publish
      node, or the `None` arm of an unreadable definition): for the
      others `check_bound_decision` does the whole check on its one read.
      Both run under the reviews lock before the append, as today.
      `check_review_request` keeps its `is_stamped_gate` branch for
      journals where the gate is not known.
   Person-only and bound gates without a form feature keep today's
   behaviour when the post names no request (the open request stamped
   without being named; carried-over decision 10).
3. The refusal is typed and retryable: `EngineError::Conflict` whose
   message starts with the code `decide_again` (constant
   `review::DECIDE_AGAIN`), as `hitl_required` starts a run failure.
   `Conflict` is what every surface already treats as "the request
   changed": HTTP 409 (`gate_refusal`,
   `crates/apb-server/src/routes/runs.rs` line 711), MCP
   `ToolError::Conflict` (`crates/apb-mcp/src/tools/mod.rs` line 112),
   `apb review` prints `review failed: conflict: decide_again: ...` and
   exits 2, and the dashboard reloads the run on any refusal
   (`web/src/pages/RunView.svelte` line 341). A new `EngineError` variant
   was rejected: it adds a match arm in every surface for a refusal they
   already render. The texts, final (each built from definition text, so
   it passes `review::form::refusal_text` with the run's redactor, as the
   form's refusals do):
   - none named: ``decide_again: gate `{node}` takes a decision only for the request it answers, and this decision names none; read its open request (seq {open}) and decide on it``
   - replaced: ``decide_again: the request of gate `{node}` at seq {seq} was replaced by the request at seq {latest}; read that request and decide on it``
   - withdrawn: ``decide_again: the request of gate `{node}` at seq {seq} was withdrawn; read the gate's request again when it asks``
   - definition unreadable (decision 6): ``decide_again: the playbook of node `{node}` cannot be read right now and this run uses review forms or a list pilot; read its request again and decide on it``
4. `answers_stamped` gets a `passkey: bool` parameter: its passkey clause
   holds only when `passkey` is true (the caller passes
   `is_stamped_gate(gate)`), the request, binding and person-only clauses
   are unchanged. `Consume::Stamped` becomes `{ person_only: bool,
   passkey: bool }`. The callers: `review.rs` lines 489 and 1267,
   `review_gate.rs` line 66.
5. The drive consumes only an entry stamped for the open request:
   `scheduler.rs` line 2440 picks `Consume::Stamped { person_only,
   passkey: is_stamped_gate(g) }` for every gate where `consumes_stamped`
   holds. A `scope_changed` withdrawal then leaves A's entry stamped with
   A, which never answers B; `withdraw_if_rescoped` is unchanged.
   `review_consumed_count` keeps its `scope_changed` term (a plain gate
   never meets one; it stays for journals main builds wrote since #288).
6. An unreadable definition under a form guard refuses. In the `None` arm
   of `check_review_target`, once the journal reads: when its latest
   `run_guard` names `review_fields`, `review_max_scope` or `list_pilot`
   (D0, the third user of item 1) and the node is
   not a publish node (`publish::latest_plan` is none), the post is
   refused with the definition-unreadable text. The guard is per run, not
   per node, so a plain gate of a run that also has a form gate is refused
   too while the snapshot does not read; it is a retry, and the snapshot
   is write-once, so it reads again unless the run directory is damaged.
   The reader: `scheduler::patch::latest_guard`
   (`crates/apb-engine/src/scheduler/patch.rs` line 200) moves to
   `event.rs` as `pub(crate) fn latest_run_guard(events: &[Event]) ->
   Option<&[String]>`, and `patch.rs` calls it. A journal that does not
   read keeps today's acceptance (the comment above `check_review_target`,
   `review.rs` line 100): such an entry has no request stamp, so a form
   gate's drive never consumes it and the doctor lists it; a plain gate's
   is consumed by count as today.
7. The doctor's `reviews` row filters by `consumes_stamped`, so an
   unstamped entry of a form gate (an older apb's post, a hand-written
   line) is listed as "no request stamp" like a stamped gate's.
8. Plain gates keep today's behaviour and journal shape: the count rule,
   no request required, no request stamp, the request journaled alone.
   A form gate's journal shape is unchanged too: `review_decided` has no
   request field, and the stamp lives in `reviews.jsonl`, whose `request`
   field exists since approvals.
9. Compatibility: no released apb knows form gates (no tag contains
   e76aa42e; `v1.0.0-beta.7` is the latest). An older apb that posts to
   such a run writes an unstamped entry; this drive never takes it and the
   doctor lists it.
10. Seen while checking, not changed here: a person-only or bound gate
    stamps the open request when the post names none, so after a
    supervisor directive withdraws its request and the gate asks again, a
    late decision without a request answers the new request. A bound gate
    is held by its binding nonce for an approving option; a person-only
    gate without `approves` is not. The fix is the same rule (require the
    request when a withdrawal happened), and it changes the behaviour of
    released gates, so it is left to a later change and named in the
    final report.

### Callers that post a decision for a form gate

| # | Caller | Where (main e76aa42e) | Request today | Change |
|---|---|---|---|---|
| 1 | `apb review` | `crates/apb-cli/src/run.rs` line 3214 `review_cmd`, `--request` parsed at line 3239, posted at 3273 | when the person passes `--request review:<seq>`; every printed hint carries it (`crates/apb-engine/src/next_action/command.rs` lines 273 to 296) | none in code; the refusal prints through the existing `Err(e)` arm |
| 2 | `apb wait --follow-auto` and notice commands | `crates/apb-cli/src/actions.rs` lines 444, 479, 518; `crates/apb-engine/src/notify/notice.rs` line 190 | from the next action's `target.request` | none |
| 3 | MCP `review_decide` | `crates/apb-mcp/src/server/run.rs` line 1358, `crates/apb-mcp/src/tools/run.rs` lines 1349 and 1378, posted at 1390; argument `crates/apb-mcp/src/server/args.rs` line 612 | when the agent passes `request` (each next action's arguments carry it) | the `request` doc in `args.rs` says it is required for a gate with `fields`, `show` or `max_scope` and that `decide_again` means read the request again |
| 4 | HTTP `POST /api/runs/{id}/review` | `crates/apb-server/src/routes/runs.rs` line 732, `body.request` at 783, posted at 836 | from the body | none (409 through `gate_refusal`) |
| 5 | Dashboard review panel | `web/src/pages/RunView.svelte` lines 324 and 808, `web/src/lib/rungates.ts` line 193 `gateRequest`, `web/src/lib/runposts.ts` line 112 | the seq of the gate's own next action | none |
| 6 | Unattended declared decision, enforced recommendation | `crates/apb-engine/src/scheduler/review_gate.rs` line 191 (`await_decision`), lines 378 and 398 (`post_unattended`), into `review.rs` line 991 `post_engine_decision` | always | none; the entry is now stamped |
| 7 | Scenario scripted decision | `crates/apb-engine/src/scheduler.rs` line 585 `scripted_review`, posted at line 595 with `request: None` | no | pass the open request's seq, `open.map(\|(seq, _)\| seq)` |
| 8 | Not a form gate | `apb publish approve\|skip` (`crates/apb-cli/src/publish.rs`), the publish panel | nonce | unchanged |

### Tests

Written first (decision numbers in brackets). Each is seen failing
before its code, except the four that guard a wrong implementation and
pass on main as well: `a_plain_gate_takes_a_decision_without_a_request`,
`an_unreadable_definition_without_a_form_guard_accepts`,
`a_passkey_enrolled_leaves_a_plain_form_gate_to_the_terminal` and
`a_scripted_decision_on_a_form_gate_names_its_request` (which fails once
the post check of decision 2 is in and passes again after callers row 7).

1. `crates/apb-engine/tests/suite/review_scope_test.rs`:
   - `a_late_decision_does_not_answer_the_request_asked_again` [2, 5]:
     the run of `work_landing_while_the_request_waits_asks_again_on_the_new_scope`
     with `max_scope: { files: 5 }` (nothing withheld on either scope):
     a decision naming the first request, stamped with it, is what makes
     the drive measure again and ask with the second (the drive measures
     the scope only when it consumes a decision); that entry never
     answers the second request. Then a decision
     without a request and one with the first request's seq are each
     refused `decide_again`, one with the second's is accepted, and the
     run's single `review_decided` follows the second request.
   - `a_form_gate_decision_without_a_request_is_refused` [2, 3]: a gate
     with `show`, a post with `request: None` through the terminal
     channel is refused with a message that starts `conflict:
     decide_again:` and names the open seq; `reviews.jsonl` stays empty.
   - `an_unstamped_entry_never_decides_a_form_gate` [5, 7]: a line
     without `request` appended to `reviews.jsonl` by hand while the gate
     waits is not consumed (the run keeps waiting until a stamped post),
     and `apb_engine::run_doctor::diagnose_run` reports it under
     `reviews` as "no request stamp".
   - `a_plain_gate_takes_a_decision_without_a_request` [8]: a plain gate
     takes a post without a request, its entry has no `request` field and
     the journal holds no `review_detail`.
   - `an_unattended_decision_on_a_form_gate_is_stamped` [6 of the callers
     table]: an unattended run with a `show` gate declaring
     `unattended: { decide: approve }` finishes succeeded, and its entry
     carries the request's seq and `by: unattended`.
   - `an_unreadable_definition_under_a_form_guard_refuses` [6]: the
     crash fixture of
     `an_open_request_without_its_detail_fails_closed_until_the_resume_writes_it`
     with `runs/<id>/playbook.yaml` renamed away: a post without fields
     is refused `decide_again`; with the file back and the run resumed,
     the detail is written and a post naming the request is consumed.
   - `an_unreadable_definition_without_a_form_guard_accepts` [6]: a run
     with a plain gate only and its snapshot renamed away accepts a post
     as today.
   - `a_scripted_decision_on_a_form_gate_names_its_request` [callers
     row 7]: a scenario quick run over a gate with `fields` decides
     through its script and ends succeeded (today the scripted post names
     no request and is refused).
2. `crates/apb-engine/src/review.rs` `mod tests`
   `a_request_reasked_between_the_check_and_the_stamp_keeps_the_named_stamp`
   [2, review round 2 blocker 1]: a run directory whose journal holds a
   `show` gate's open request A; a `#[cfg(test)]` thread-local hook
   (`AFTER_BOUND_READ`, called in `check_bound_decision` right after its
   journal read and nowhere else) appends `review_withdrawn` for A and
   `review_requested` B to the journal; a post naming A is stamped A,
   never B; `answers_stamped` for B is false on that entry, so the drive
   would not consume it for B; a second post naming B is accepted and
   stamped B. Before the change the first post is stamped B (it fails).
   The hook is a test-only function of the module, absent from every
   build without `cfg(test)`.
3. `crates/apb-engine/tests/suite/passkey_gate_test.rs`
   `a_passkey_enrolled_leaves_a_plain_form_gate_to_the_terminal` [4]:
   under `with_fake_passkey_enrolled`, a gate with `show` (neither
   person-only nor bound) takes a terminal post that names its request,
   and the drive consumes it.
4. `crates/apb-mcp/tests/suite/review_tool_test.rs`
   `review_decide_on_a_form_gate_needs_the_request` [3]: without
   `request` the tool returns a conflict naming `decide_again`; with the
   next action's `request` it posts.
5. `crates/apb-server/tests/suite/runs_api_test.rs`
   `a_form_gate_review_without_request_is_409` [3].
6. `crates/apb-cli/tests/suite/people_cli_test.rs`
   `review_without_request_on_a_form_gate_is_refused` [3]: `apb review`
   without `--request` exits 2 and prints `decide_again`; the command the
   pending block prints (with `--request review:<seq>`) succeeds.

Existing tests that post on a form gate without a request are changed to
pass the open request's seq (`review::open_request`), not weakened:
`review_scope_test.rs`, `review_form_test.rs`, `loop_edges_test.rs` (its
`fields` gate), `review_tool_test.rs`, `runs_api_test.rs` and
`people_cli_test.rs`. A file whose posts already name the request is left
alone.

### Docs

1. `docs/HOWTO-authoring.md`, section "What a decision covers (scope,
   max_scope)", after the paragraph "A decision is taken on the scope it
   covers": "A decision on a gate that uses `max_scope`, `fields` or
   `show` names the request it answers (`apb review --request
   review:<seq>`, the `request` argument of MCP `review_decide` and of the
   HTTP route; every printed command and next action carries it). One
   that names none, or names a request the gate asked again since, is
   refused `decide_again`: read the gate's request again and decide on
   it. So a decision made on a scope the gate no longer asks about never
   answers the new request. A plain gate takes a decision without it, as
   before."
2. The MCP argument doc of `review_decide.request` (callers table row 3).

### Wrong implementations

1. Stamping a form gate's entry with the open request when the post names
   none (the stamped path as it is): the late decision is stamped with B.
   Caught by `a_late_decision_does_not_answer_the_request_asked_again`.
2. Reusing `answers_stamped` with its passkey clause for a form gate: on
   a machine with a passkey every CLI and MCP decision on a plain form
   gate is queued and never consumed. Caught by a passkey variant inside
   `a_form_gate_decision_without_a_request_is_refused` (with
   `apb_core::passkey` test store holding one passkey, a post naming the
   request through the terminal is consumed).
3. Requiring the request on every gate: the plain gate's journal and
   surfaces change. Caught by `a_plain_gate_takes_a_decision_without_a_request`.
4. Refusing every post while the snapshot does not read: a plain run's
   decisions fail where they pass today. Caught by
   `an_unreadable_definition_without_a_form_guard_accepts`.
5. Checking the named request on one journal read and stamping the open
   request from a second (review round 2, blocker 1): a request asked
   again between the reads is stamped onto a decision made for the one
   before. Caught by
   `a_request_reasked_between_the_check_and_the_stamp_keeps_the_named_stamp`.

## D0. Shared pieces

### What this change takes from people-in-the-loop

These exist on main e76aa42e; this change uses them and redefines none of
them (tasks 0.2 checks each):

1. `review_gate::await_decision(d, consume, request, detail)`
   (`crates/apb-engine/src/scheduler/review_gate.rs` line 125) with
   `GateDrive` (line 109, `workdir` included) and `GateRequest { event,
   auto, detail }` (line 75). For a node whose kind is not a form gate it
   journals the request alone (line 173) and never calls `detail`.
2. `GateDetail` (`crates/apb-engine/src/review/form.rs` line 13, derives
   `Default`): a pilot request passes `GateDetail::default()`.
3. `review_gate::post_unattended` (line 378), the unattended post every
   gate uses.
4. In `check_review_target` (`crates/apb-engine/src/review.rs` line 122):
   the node check (line 147) and the nonce refusal (line 169); and the
   field refusal `gate \`{node}\` takes no fields` (line 335, in
   `check_form_unscrubbed`), which today answers a `human_review` only:
   the pilot gate gets its own explicit refusal with the same text (D1
   "The channel").
5. `PendingReview` (`crates/apb-engine/src/progress.rs` line 84) and
   `pending_gate` (line 260).
6. `crates/apb-core/src/validate/review_form.rs`, called in `validate()`
   right after the cloud models call (`validate/mod.rs` line 463), and
   the guarded features `ReviewFields` and `ReviewMaxScope`
   (`GuardedFeature::ALL` of 11 entries, `validate/guarded.rs` line 63).
7. The CLI test file `crates/apb-cli/tests/suite/people_cli_test.rs`.

### Validator codes

| Code | Severity | Decision | Rule |
|---|---|---|---|
| V240 | error | D1 | malformed `list.pilot` |
| V241 | error | D1 | malformed `list.stop_when` |
| V242 | warning | D1 | `pilot.then: continue` on a list without an item `check` |
| V249 | error | D5 | malformed top-level `unattended:` |
| V250 | warning | D5 | an acting `agent_task` without a deterministic check in a playbook that declares `unattended:` |

Taken on main e76aa42e: `git grep -ohE '"V2[0-9][0-9]"' crates | sort -u`
prints `"V200"`, `"V210"` to `"V212"`, `"V220"` to `"V222"` and `"V243"`
to `"V247"` only. V243 to V247 are people-in-the-loop's, V248 stays
reserved; V251 to V259 stay reserved for this change family. The five
codes above are free (tasks 0.3).

Two new modules in `crates/apb-core/src/validate/`, each with a module doc
line: `pilot.rs` (V240 to V242) and `readiness.rs` (V249, V250).
Declared in `crates/apb-core/src/validate/mod.rs` in alphabetical place:
`mod pilot;` after `mod params;` (line 34), `mod readiness;` after `mod
read_only;` (line 38). Called from `validate()` right after the
`review_form` call (line 463) and before `if r.is_valid()` (line 464):

```rust
cloud_models::check_cloud_models(playbook, ctx, &mut r); // V220-V222
review_form::check_review_form(playbook, &mut r); // V243-V247
pilot::check_pilot(playbook, &mut r); // V240-V242
readiness::check_readiness(playbook, &mut r); // V249, V250
```

Each function has the signature `pub(super) fn check_x(playbook:
&Playbook, r: &mut ValidationReport)` and reports with `r.error(code,
Some(&node.id), message)` or `r.warn(code, Some(&node.id), message)` (the
playbook-level V249 passes `None`, V250 `Some(&node.id)`), as
`validate/list.rs` does.

### The journal: two new events

Added to `EventPayload` in `crates/apb-engine/src/event.rs`, neither a
checkpoint (`is_checkpoint`, line 3723, stays as it is). Each is written
by the drive only, between a node's start and its `node_finished` or a
`run_paused`, so an older apb skips it up to that checkpoint
(`event::read_all`); a run that uses either carries the run guard
`list_pilot` (below), so no older apb continues it. Every field is
`#[serde(default)]` (decision record 0001).

Put `ListPilot` and `ListStopped` right after `ListItemSettled` (which
ends at line 2163):

```rust
/// The pilot slice of a list visit (`list.pilot`): the ids run before
/// the pilot gate, in source order, and the pilot round (1 for the first
/// slice, one more after each `revise`). Written before the slice's first
/// `list_batch_started`. Safe to skip up to the node's `node_finished`
/// or a `run_paused`; a run that writes it carries the run guard
/// `list_pilot`. Additive variant.
ListPilot {
    #[serde(default)]
    node: String,
    #[serde(default)]
    round: u32,
    #[serde(default)]
    items: Vec<String>,
},
/// A list visit ended before every item ran: `reason` is `pilot` (the
/// pilot gate was answered `stop`) or `stop_when` (the early-stop rule
/// held), `detail` the sentence the surfaces show. Written before the
/// node's `node_finished`. Safe to skip like `list_pilot`. Additive
/// variant.
ListStopped {
    #[serde(default)]
    node: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    detail: String,
},
```

`RunState::fold` (`crates/apb-engine/src/state.rs`) matches every
variant: add `| EventPayload::ListPilot { .. } | EventPayload::ListStopped
{ .. }` to the list arm (lines 834 to 837, "A list visit's records"). No
other match in the crate is exhaustive over `EventPayload` (only
`state.rs` lists `ListBatchStarted` in a match without `_`).

`review_detail` is unchanged (decision 1). No event gets a new field:
the probation ledger is a file, not a journal record (D5).

The journal compatibility fixture
`crates/apb-engine/tests/fixtures/journal/known_events.jsonl` gets one
line of each new event inside a list visit (before its `node_finished`),
so `journal_compat_test::a_journal_without_unknown_events_round_trips_byte_for_byte`
covers them. This file is counted in "Files".

### One new guarded feature

`crates/apb-core/src/validate/guarded.rs` `GuardedFeature` gets one
variant, appended after `ReviewMaxScope` (the last on main), with its doc
line:

```rust
/// A list node runs a pilot slice and stops at a gate, or ends early
/// (`list.pilot`, `list.stop_when`).
ListPilot,
```

`as_str` names it `list_pilot`; `ALL` (line 63) becomes
`[GuardedFeature; 12]` with `ListPilot` last. `guarded_features` (line
103) inserts `ListPilot` when `node.list().is_some_and(|l|
l.pilot.is_some() || l.stop_when.is_some())`. `run_guard_of`
(`crates/apb-engine/src/scheduler/prepare.rs` line 1906) passes every
feature except `deadline` and `concurrency` through and sorts them, so it
needs no change. `manifest::manifest_guard` (`manifest.rs` line 579)
stays as it is (decision 7). Update the two tests that count:
`a_deadline_is_a_guarded_feature` (line 170) asserts 12, and
`all_has_eleven_entries_in_order` (line 326) is renamed
`all_has_twelve_entries_in_order` with `GuardedFeature::ListPilot` last.
Add `a_list_pilot_is_guarded`: a one-node playbook with `pilot` and
another with `stop_when` only each name `list_pilot`, and `parse`
round-trips the name.

The module doc of `guarded.rs` (lines 1 to 14) gets one clause after
"one that does not know a gate's `fields`, `show` or `max_scope` would
decide the gate as a plain one": "; one that does not know `list.pilot`
or `stop_when` would run every item of a list without its pilot gate or
its early stop".

The wait-park change (untracked draft) also appends a variant
(`WaitPark`). Whichever lands second appends after the other, takes the
next count (13) and renames the count test for it; nothing else
conflicts (section "Overlap with wait-park" in the closure report).

### Test helpers

Engine integration tests of this change reuse the list helpers of
`crates/apb-engine/tests/suite/list_run_test.rs` (`poll_until` line 26,
`playbook` line 40, `inline_items` line 54, `seed` line 61, `stub` line
118, `plan` line 145, `invocations` line 160, `run_list` line 215,
`events` line 219, `settled` line 235, `work_output` line 893), as
`list_resume_test.rs` does (`use crate::list_run_test::{...}`, line 21),
and the gate helpers of `crates/apb-engine/tests/suite/review_test.rs`
(`run_in_background` line 101, `latest_run_dir` line 111, `decide` line
122, `wait_result` line 139): copy those four into the new test file
when a test needs a background run, since they are private there.


## D1. Pilot slice and early stop for list nodes

### Schema (apb-core)

In `crates/apb-core/src/list/mod.rs`, after `MAX_BATCH_RETRIES` (line 25):

```rust
/// The most items a pilot slice holds (V240).
pub const PILOT_MAX_ITEMS: u32 = 50;
/// The options of a pilot gate, in the order they are offered.
pub const PILOT_CONTINUE: &str = "continue";
pub const PILOT_REVISE: &str = "revise";
pub const PILOT_STOP: &str = "stop";
pub const PILOT_OPTIONS: [&str; 3] = [PILOT_CONTINUE, PILOT_REVISE, PILOT_STOP];
/// `stop_when.min_settled` when `needs_look_share` is given without it.
pub const DEFAULT_MIN_SETTLED: u32 = 10;
```

`LIST_OUTPUT_FIELDS` (line 27) becomes `[&str; 8]` with `"stopped"`
after `"filtered_out"`.

`ListSpec` (line 62) gets, after `rerun` (line 76):

```rust
/// Run the first items of a visit, then stop at a gate (V240, V242).
#[serde(default, skip_serializing_if = "Option::is_none")]
pub pilot: Option<PilotSpec>,
/// End a visit early when too many items need a look (V241).
#[serde(default, skip_serializing_if = "Option::is_none")]
pub stop_when: Option<StopWhen>,
```

New types in the same file:

```rust
/// A list's pilot slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PilotSpec {
    /// How many items the slice runs, the first in source order that
    /// still need to run.
    pub items: u32,
    #[serde(default)]
    pub then: PilotThen,
    #[serde(default)]
    pub unattended: PilotUnattended,
}

/// What follows a settled pilot slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PilotThen {
    /// Always ask a person.
    #[default]
    Review,
    /// Go on by itself when every slice item passed; ask otherwise.
    Continue,
}

/// What an unattended run does where a pilot gate would ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PilotUnattended {
    /// The run fails (`hitl_required`).
    #[default]
    Fail,
    /// The engine decides `continue` (journaled `decided_by: unattended`)
    /// when every slice item passed; with an item that needs a look the
    /// run fails (`hitl_required`) as under `fail`.
    Continue,
}

/// When a visit ends early.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StopWhen {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_look_at_least: Option<u32>,
    /// A share of the settled items, above 0 and at most 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_look_share: Option<f64>,
    /// The settled items `needs_look_share` waits for (default 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_settled: Option<u32>,
}

impl StopWhen {
    /// The sentence the visit stops with when a rule holds over
    /// `processed` settled items of which `needs_look` need a look.
    pub fn holds(&self, processed: u32, needs_look: u32) -> Option<String>;
}
```

`StopWhen::holds`: `needs_look_at_least: Some(k)` and `needs_look >= k`
gives `"{needs_look} of {processed} settled items need a look
(stop_when: needs_look_at_least {k})"`; else `needs_look_share: Some(s)`,
`processed >= min_settled.unwrap_or(DEFAULT_MIN_SETTLED)` and
`needs_look as f64 >= s * processed as f64` gives `"{needs_look} of
{processed} settled items need a look, {pct}% (stop_when:
needs_look_share {s})"` with `pct` the rounded whole percent; else
`None`. `ListSpec` keeps `PartialEq` without `Eq` (it has no `Eq` on
main either), so the `f64` field is allowed.

YAML:

```yaml
- id: rename
  type: agent_task
  prompt: Apply the rename to one file.
  list:
    items: { from: { node: find, field: files } }
    check: { script: check-file.sh }
    pilot: { items: 3, then: review }
    stop_when: { needs_look_at_least: 5 }
```

### V240, V241, V242

`validate/pilot.rs`, for every node with `list:`:

1. V240 (error): `pilot.items` is 0 or above `PILOT_MAX_ITEMS`:
   `list.pilot.items is {n}; use 1 to 50`; `pilot.items` is not below
   `items.max_items` (`ItemSource.max_items`, line 102, always set,
   default `default_max_items`): `list.pilot.items {n} is not below
   list.items.max_items {m}: the pilot would be the whole list`; an
   inline list with at most `pilot.items` items: `the list has {k} inline
   items; a pilot of {n} runs them all`.
2. V241 (error): neither rule: `list.stop_when sets no rule; give
   needs_look_at_least or needs_look_share`; `needs_look_at_least: 0`:
   `list.stop_when.needs_look_at_least is 0; use 1 or more`; a share that
   is not finite or not in (0, 1]: `list.stop_when.needs_look_share is
   {s}; use a share above 0 and at most 1`; `min_settled` without a
   share: `list.stop_when.min_settled needs needs_look_share`;
   `min_settled: 0`: `list.stop_when.min_settled is 0; use 1 or more`.
3. V242 (warning): `pilot.then: continue` on a list without `check`:
   `list.pilot.then: continue goes on by itself when every pilot item
   passes, and without an item check every item passes; add a check or use
   then: review`.

### The journal and the fold

The slice is journaled as `list_pilot` (D0) and the gate is an ordinary
plain review request on the list node: `review_requested` with the options
`continue`, `revise`, `stop`, written alone (no `review_detail`, decision
1), and the `review_decided` that answers it. The early end of a visit is
`list_stopped`.

`ListFold` (`crates/apb-engine/src/list/ledger.rs` line 75) gets two
fields and folds them in `fold` (line 130):

```rust
/// The pilot of the visit, from its `list_pilot` events and the list
/// node's review events after the latest one.
pub pilot: Option<PilotState>,
/// The visit ended early: the `reason` and `detail` of its
/// `list_stopped`; a reopen clears it.
pub stopped: Option<(String, String)>,
```

```rust
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PilotState {
    /// The latest slice's round (1 for the first).
    pub round: u32,
    /// The latest slice's ids, in source order.
    pub items: Vec<String>,
    /// The seq of the latest `list_pilot`.
    pub seq: u64,
    /// A review request of the node after `seq` is open.
    pub open: bool,
    /// The latest `review_decided` of the node after `seq`.
    pub decision: Option<String>,
    /// The note of the visit's latest `revise` with a non-blank note:
    /// every later item run of the visit is told it.
    pub guidance: Option<String>,
}
```

Fold rules, in `fold`'s match (events of `node` only):

1. `ListStarted` (line 138, both a new visit and a reopen): `stopped =
   None`. A new visit starts from `ListFold::default()` anyway; a reopen
   keeps `pilot`.
2. `ListPilot { round, items, .. }`: `pilot = Some(PilotState { round,
   items, seq: e.seq, open: false, decision: None, guidance: <the previous
   guidance, if any> })`.
3. `ReviewRequested` of the node with `e.seq > pilot.seq`: `open =
   true`. `ReviewWithdrawn`: `open = false`. `ReviewDecided { decision,
   note }`: `open = false`, `decision = Some(decision)`, and when the
   decision is `revise` and `note.trim()` is not empty, `guidance =
   Some(note)`.
4. `ListStopped { reason, detail }`: `stopped = Some((reason, detail))`.

Two methods: `pilot_settled(&self) -> bool` (every id of the latest slice
has a terminal record) and `pilot_needs_look(&self) -> u32`.

The one pure rule, in `ledger.rs`:

```rust
/// Where a visit stands with its pilot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PilotPhase {
    /// No pilot, or it passed: the rest runs.
    Done,
    /// No slice yet: the slice starts (round 1).
    Start,
    /// The slice has items left to run.
    Running,
    /// The slice settled and a person decides (the gate is due or open).
    Gate,
    /// `revise` decided: the slice runs again (one round more).
    Revise,
    /// `stop` decided.
    Stop,
}

pub fn pilot_phase(spec: &apb_core::list::PilotSpec, fold: &ListFold) -> PilotPhase;
```

1. `fold.pilot` is `None`: `Start`.
2. `decision` `continue`: `Done`; `revise`: `Revise`; `stop`: `Stop`
   (a decision of another word: `Gate`, the drive refuses it, see "The
   gate in the drive").
3. No decision: `Running` while `!pilot_settled()`; else `Done` when
   `spec.then == PilotThen::Continue` and `pilot_needs_look() == 0`;
   else `Gate`.

### The runner

`crates/apb-engine/src/scheduler/list/pilot.rs` (new; `mod pilot;` in
`scheduler/list/mod.rs` after `mod check;`, line 13) holds the runner's
pilot steps and the drive's gate check; `batch.rs` calls it.

1. `End` (`batch.rs` line 38) gets `PilotGate` ("the pilot slice
   settled and a person decides; the visit stays open") and `Stopped`
   ("the visit ended early: a pilot `stop` or `stop_when`").
2. The `while` loop of `run_batches` (lines 77 to 108) moves into `fn
   run_queue(run, queue: VecDeque<String>, batch_no: &mut u32, stop:
   Option<&StopRule>) -> Result<Option<End>, EngineError>`, where
   `StopRule { when: StopWhen, outside: Vec<String> }` is the visit's
   `stop_when` and the ids of the latest pilot slice (empty without a
   pilot). The loop body is unchanged except for two steps, in this order
   (review finding 5):
   1. Before a batch is admitted, after `stopped(run)` and before the
      budget gate: with `stop` set, read the fold and evaluate the rule
      (step 3 below). This catches a visit that already meets the rule
      when it is resumed, so a resume never admits one more batch first.
   2. After a batch: `let ended = run_batch(run, ids, &mut queue)?;
      progress(run)?;` then `if let Some(end) = ended { return
      Ok(Some(end)); }` exactly as on main: a batch that ended `Halted`,
      `Pause`, `Cancelled` or `Exhausted` is honoured first and the rule is
      not evaluated (its unsettled items go back to pending as today, and
      the resumed visit evaluates the rule in step 1). Only a batch that
      ended with `None` is followed by the rule.
   3. The rule: `let c = fold.counts_excluding(&stop.outside);` (a new
      `ListFold::counts_excluding(&self, ids: &[String]) -> Counts` in
      `ledger.rs`, `ListFold::counts` (line 255) over the item records whose
      id is not in `ids`); when `stop.when.holds(c.processed,
      c.needs_look)` is `Some(detail)`, append `ListStopped { node, reason:
      "stop_when", detail }` and return `Some(End::Stopped)`. A batch's
      items all settle before the rule is read, so a stop never leaves an
      item running.
   The slice is excluded from the count because a person who answered
   `continue` read it: the rule judges the items run after that decision.
   Without a pilot nothing is excluded.
3. `run_batches` (line 68) becomes:
   1. Read the fold. `fold.stopped.is_some()`: return `End::Stopped` (a
      drive that crashed between `list_stopped` and `node_finished`).
   2. With `run.spec.pilot` set, `pilot::step(run, spec, &fold,
      &sends)?`, `sends` being `batch::ItemSends::of(&events, node)`
      over the journal read the fold came from:
      1. `Step::Slice(ids)`: `run_queue(run, ids, &mut batch_no, None)`:
         the queue holds the slice ids only, and `stop_when` is never
         evaluated inside the slice (`stop` is `None`). An `End` from it
         returns as it is (`Halted`, `Pause` and the others keep the
         visit open; the slice continues on resume). Then read the fold
         again and return `End::PilotGate` unless `pilot_phase` is now
         `Done`.
      2. `Step::Wait`: return `End::PilotGate` (the gate is due or open:
         `stop_when` is not evaluated while it is).
      3. `Step::Stop`: return `End::Stopped`.
      4. `Step::Go`: go on.
   3. The remaining ids of a fresh fold, through `run_queue(run, ids,
      &mut batch_no, stop)` with `stop` built from `run.spec.stop_when`
      and `fold.pilot`'s slice ids; `None` from it is `End::Done`.
   How a batch is cut: a batch takes `min(batch.size, queue length)` ids
   from the front of the queue it runs over, as on main. Inside the pilot
   the queue holds only the slice ids (and the slice's own retries, which
   `requeue_for_retry` pushes back onto that same queue), so a batch never
   crosses the slice boundary: a slice of 3 under `batch.size: 2` runs a
   batch of 2 and a batch of 1, and the gate is asked after exactly the
   slice whatever the batch size. The first batch after `continue` starts
   from the first id after the slice.
4. `pilot::step` by `pilot_phase`:
   1. `Start`: the first `spec.items` ids of
      `fold.remaining(run.items ids in order)` (`ListFold::remaining`,
      line 247); none: `Step::Go`; else append `ListPilot { node, round:
      1, items }` and `Step::Slice`.
   2. `Running`: the slice ids `fold.remaining` keeps, `Step::Slice`.
   3. `Gate`: `Step::Wait`.
   4. `Revise` (review round 3, blocker 3): `pilot::step` uses the
      visit's `sends` (step 3.2 above; `scheduler/list/mod.rs` line 132
      reads them the same way for `requeue_for_retry`). For each slice id in order,
      one branch per item, decided against the existing unknown-send
      check:
      1. `sends.unknown(run.key(id)).is_some()` (`batch.rs` line 469,
         the same call `requeue_for_retry` makes at
         `scheduler/list/mod.rs` line 192): nothing is written for the
         item. It is never interrupted, so it keeps its `needs_look`
         record and its unknown send is never repeated.
      2. Else the item's record is already `Pending` (an interrupt
         written before a crash, below): nothing is written.
      3. Else `run.settle(Settlement { id, state: settled::INTERRUPTED,
         reason: Some(PILOT_REVISE_REASON), round: 0,
         ..Settlement::default() })` with `pub const PILOT_REVISE_REASON:
         &str = "pilot_revise"` in `ledger.rs` next to `RECONCILE` (line
         25): the item runs again with its whole `batch.retries`.
      Then append `ListPilot { node, round: pilot.round + 1, items: <the
      same ids> }` and `Step::Slice`. The new slice keeps every id,
      the unknown-send items included: they stay in the slice as
      `needs_look`. `fold.remaining` returns only pending ids, so they
      are not run; the slice settles when the others do
      (`pilot_settled`), and the gate counts them in `pilot_needs_look`
      and in its `{n} need a look` line, so an unattended `continue`
      fails (decision 3) and a person sees them until a person resolves
      the send (`apb intent resolve`) and a later `revise` runs them.
      A crash partway through the transition (some items interrupted,
      no `list_pilot` of the next round yet) leaves the latest decision
      `revise` after the latest `list_pilot`, so `pilot_phase` is still
      `Revise` on resume: step 2 skips the items already interrupted,
      step 1 still skips the unknown-send ones, and exactly one
      `list_pilot` of round `pilot.round + 1` is appended.
   5. `Stop`: when `fold.stopped` is `None`, append `ListStopped { node,
      reason: "pilot", detail: format!("the pilot gate was answered stop;
      {rest} of {total} items did not run") }`; `Step::Stop`.
   6. `Done`: `Step::Go`.
5. The reviewer's note: `run_item` (`batch.rs` line 256) appends
   `crate::list::prompt::guidance_section(note)` to `ItemExec.section`
   (`scheduler/outcome.rs` line 133, set at `batch.rs` line 265) when
   `fold.pilot` carries `guidance`. In
   `crates/apb-engine/src/list/prompt.rs`: `pub fn guidance_section(note:
   &str, channel: &str) -> String` returning `"\n\n## reviewer
   note\n\nThe reviewer of the first items of this list asked for this
   change (decided through {channel})."` followed by `note`, cut by
   `clip(note, 2000)` (line 87), inside the same untrusted data fence the
   item section uses: the note is text an agent did not author, and the
   section names the decision's channel (`review_decided.via`) instead of
   claiming a person (review fixes round 1, A7). It is outside the
   rendered template, like the item section, so the node's cache key and
   the shared prompt prefix do not change.
6. `execute` (`scheduler/list/mod.rs` line 32, its `match outcome` at
   line 96): `End::PilotGate` maps to `AttemptOutcome::Halted { events:
   Vec::new() }` (no `node_finished`; the drive goes back to the loop top,
   where the gate branch takes it), `End::Stopped` to `compose(&run)`.
7. `compose` (line 281): `fold.stopped` set means the node fails,
   whatever `on_needs_look` says, and the output object gets `"stopped":
   {"reason": <reason>, "detail": <detail>}`; without it the output has no
   `stopped` key (every existing output stays byte for byte).

### The gate in the drive

In `crates/apb-engine/src/scheduler.rs`, a new branch of the chain right
before `} else if is_interactive(&playbook, &current) {` (line 2789),
after the sub-playbook branch:

```rust
} else if let Some(pilot) = list::pilot::gate_due(&playbook, &read_all(run_dir)?, &current) {
```

`pub(crate) fn gate_due(playbook: &Playbook, events: &[Event], node: &str)
-> Option<PilotGate>` in `scheduler/list/pilot.rs`: the node has
`list.pilot`, `ListFold::of(events, node)` (`ledger.rs` line 118) is a
visit that is not `finished`, and `pilot_phase` is `Gate`; `PilotGate {
spec, fold }`.

The branch:

1. In an unattended run (`cfg.unattended`): under `PilotUnattended::Fail`,
   or under `Continue` while `fold.pilot_needs_look() > 0` (decision 3),
   it ends the run with `unattended_stop(log, run_dir, &current,
   unattended::hitl("list pilot", &current), &mut frontier)` (`scheduler.rs`
   line 353; `hitl` in `scheduler/unattended.rs` line 58). Under
   `Continue` with a clean slice it first calls
   `review_gate::post_unattended(run_dir, &current, &(PILOT_CONTINUE.into(),
   String::new()))` like the human review branch (line 2431), then asks
   with `auto: Some(AutoDecision { decision: "continue", note: "", by:
   DecidedBy::Unattended })`.
2. `review_gate::await_decision` with a `GateDrive` built as the human
   review branch builds it (line 2447: `run_dir`, `workdir`, `log`,
   `playbook`, `current`, `control_cursor`, `prompt_overrides`,
   `frontier`), `Consume::Stamped { person_only: false, passkey: false
   }` (the pilot gate is a `consumes_stamped` gate through
   `requires_request`, carried-over decision 1; never `Consume::Nth`:
   review finding 4), the request
   `list::pilot::request(&playbook, &current, &pilot, events, auto)` and
   the detail closure `|| Ok(GateDetail::default())` (never called for a
   list node: it is not a form gate). The request:
   1. `event`: `EventPayload::review_requested(node, PILOT_OPTIONS as
      Strings, instruction)` (`event.rs` line 2452) with `title` set to
      the node's title.
   2. `instruction`: `crate::people::instruction::review_instruction_for(
      node, title, &options, Some(&line), false)` (`people/instruction.rs`
      line 62) where `line` is `Pilot of list \`{node}\`: {k} of {total}
      items ran, {passed} passed, {n} need a look. continue runs the other
      {rest}; revise runs these {k} again with your note in their prompt;
      stop ends the list with {rest} not run.` (`1 needs a look` for one).
   3. `detail`: `GateDetail::default()`.
   4. `auto`: the unattended `continue` when item 1 asked for it.
3. `GateStep::Decided(entry)`: a decision outside `PILOT_OPTIONS` ends
   the run with `stop_with_reason(log, &current, format!("list
   \`{current}\`: \`{d}\` is not a pilot option (continue, revise,
   stop)"), &mut frontier)` (`scheduler.rs` line 319); else append the
   `review_decided` exactly as the human review branch builds it (lines
   2564 to 2583: `EventPayload::review_decided` with `decided_by`,
   `binding` and `via` from the entry). Factor that construction into
   `pub(crate) fn decided_event(node: &str, entry: &ReviewEntry) ->
   EventPayload` in `review_gate.rs` and use it in both places; then
   `continue`: the next turn dispatches the list node, whose visit is
   open (decision 11), and the runner reads the decision from the fold.
   No `review_fields` is ever written for a pilot gate (it takes no
   fields).
4. `GateStep::Moved`: `continue`. `GateStep::Waiting`: the scenario check
   `scripted_review(run_dir, &current)?` (line 585) and the poll sleep,
   exactly as the human review branch does (lines 2597 to 2600).

No `node_started` or `node_finished` is written by this branch; the
list node's own dispatch writes them.

### The channel

`check_review_target` (`crates/apb-engine/src/review.rs` line 122): the
node check (line 147) also accepts a node with `n.list().is_some_and(|l|
l.pilot.is_some())`, and the message (line 149) becomes `node \`{node}\`
is not a human_review, publish or list pilot node of this run's playbook
\`{id}\``. For such a node: a decision outside `PILOT_OPTIONS` is
`EngineError::Invalid(format!("node \`{node}\` is a list pilot gate;
decide continue, revise or stop"))`; a nonce is refused like a human
review's (line 169) with the message `node \`{node}\` is a list pilot
gate; a nonce answers a publish node only`; field values are refused
explicitly (review round 3, minor 8): `check_form_unscrubbed`
(`review.rs` line 314) returns `Ok(())` for every node that is not a
`human_review` before its `takes no fields` check (line 333), so a list
pilot gate would take fields silently; its `let ... else` arm becomes,
before that `return Ok(())`, `if gate.list().is_some_and(|l|
l.pilot.is_some()) && !fields.is_empty() { return
Err(EngineError::Invalid(format!("gate \`{node}\` takes no fields"))); }`,
the same text a `human_review` without `fields` gives (other node kinds
keep main's answer); and the generic open and queued checks apply. The pilot
gate is never a person-only or bound gate (`is_stamped_gate`, line 421: a
list node has no `person_only` or `approves`, so no passkey is asked) and
never a form gate, but `requires_request` holds for it (carried-over
decision 1): a decision that names no request, or names a request the
gate replaced or withdrew, is refused `decide_again` with the texts of
carried-over decision 3, and the entry of an accepted decision is stamped
with the open request. Every surface already names it: the printed
command and every next action carry `--request review:<seq>` or
`request` (callers table rows 1 to 5), the engine's unattended
`continue` posts through `post_unattended`, which names the request (row
6), and the scenario script names it (row 7).

Why the count rule cannot hold a pilot gate (review finding 4, confirmed
on main: `queued_decision` with `Consume::Nth` takes the entry at
`review_consumed_count`, `event.rs` line 3399, which counts one per
`review_decided` and one per `scope_changed` withdrawal only): a pilot
request A is open; the drive finds no queued decision; a person posts
`continue`; the drive then applies a supervisor `run_continue_from` (or
`node_retry`) and writes `review_withdrawn` for A with another reason;
later the list reaches its gate again with request B. The consumed count
is still 0, so the count rule takes A's `continue` as B's decision,
unseen. Under the stamped rule A's entry carries A's seq and never
answers B; `apb doctor --run` lists it under `reviews`.

### Scenarios

1. `crates/apb-core/src/scenario/validate.rs` (review round 3, major
   5). The pilot gate is on the list node, inside the list node's open
   visit, so one visit answer carries both the items and the pilot
   decision. Four changes, no other:
   1. `expected_kind` (line 225) is unchanged: `ITEMS` stays the primary
      kind of every list node, a pilot included (making it `REVIEW` would
      refuse the item answers, and `check_visits`, line 483, names the
      primary kind in its V147 text).
   2. `taken_kinds` (line 249) also pushes `REVIEW` for a node whose
      `list` has a `pilot`, after the `JUDGE` of a judge item check; its
      doc names it. A `review` answer on a list node without a pilot
      still does not fit (V147, `it takes \`items\``).
   3. The file-level shape rule of V146 (`too_many_kinds`, line 77,
      checked at line 183 for every visit before any node is known)
      allows today only one kind, `items` with `judge`, or `judge` with
      `attempts`; it would refuse `items` with `review` before V147 is
      reached. `PAIRS` (line 74) gets `[ITEMS, REVIEW]`, and
      `too_many_kinds` accepts the one triple `ITEMS`, `JUDGE`, `REVIEW`
      (a pilot over a judge item check) through a new `const TRIPLE:
      [&str; 3]` compared as a set; the V146 text (line 185) becomes
      `{at} carries {kinds} answers; a visit takes one kind, or \`items\`
      with \`judge\`, or \`judge\` with \`attempts\`, or \`items\` with
      \`review\` on a list pilot (and \`judge\` too when its item check
      is a judge)`. The node-level fit stays V147's: a pair the shape
      rule now lets through on a node that does not take it is refused
      there.
   4. `check_values` (line 397, V148) gets a second review arm: for a
      node whose `list` has a `pilot`, a `review` answer whose decision
      is not in `PILOT_OPTIONS` (`continue`, `revise`, `stop`) is V148
      with its own text `{at}: review decision \`{d}\` is not a pilot
      option of \`{node}\` (continue, revise, stop)`; the existing arm
      matches `NodeKind::HumanReview` only, so without it a pilot answer
      would be checked by nothing.
   The answer book (`scenario/book.rs` `review`, line 295) hands the same
   review answer at every ask of one visit: a scripted `revise` is
   answered `revise` again at every later gate of that visit, so it does
   not script one revision followed by `continue`. The scenarios this
   change writes answer `continue`; scripting a sequence of pilot
   answers is left out of this change.
2. `crates/apb-engine/src/scenario/capture.rs` keeps the `list_node` gap
   for every list node, a pilot list included: the per-item answers are
   not captured, so a capture that kept only the pilot decision would
   replay with every item missing (review round 1, M7). Capturing item
   answers is left out of this change.
3. `scripted_review` (`scheduler.rs` line 585) needs no change: it keys
   by node.

### Surfaces

1. `PilotView` in `crates/apb-engine/src/list/view.rs`, `ts`, `Serialize,
   Deserialize`, named by its path `crate::list::view::PilotView`
   everywhere (`list::view` is a public module, so no re-export in
   `list/mod.rs` is needed):

```rust
pub struct PilotView {
    pub round: u32,
    pub items: Vec<String>,
    pub passed: u32,
    pub needs_look: u32,
    /// Items of the visit outside the slice.
    pub rest: u32,
    /// `running`, `waiting`, `continued`, `revised` or `stopped`.
    pub state: String,
}
impl PilotView { pub fn of(fold: &ListFold) -> Option<PilotView>; }
```

   `state` from `fold.pilot`: `open` (the gate's request is open):
   `waiting`; decision `continue`: `continued`; `revise`: `revised`;
   `stop`: `stopped`; no decision, the slice settled and the visit went
   on (an item outside the slice started, or the list node finished
   `succeeded`, which covers a visit with no item outside the slice):
   `continued`; else `running`.
2. `RunList` (line 20) and `ListProgress` (line 74) get
   `#[serde(skip_serializing_if = "Option::is_none")] pilot:
   Option<PilotView>` and `stopped: Option<String>` (the detail), both
   TypeScript optional, set in `RunList::with_open_sends` (line 92) and
   `ListProgress::of` (line 171). The literal `RunList` constructions in
   tests get `pilot: None, stopped: None` (`view.rs` lines 126 and 139,
   `crates/apb-cli/src/run_items.rs` lines 156, 192 and 219).
3. `ListOutcome` (`crates/apb-engine/src/run_outcome/lists.rs` line 20)
   gets the same two fields from its `RunList` (the construction at line
   124); `line()` (line 46) appends `; pilot round {r} waits for a
   decision` when the pilot is `waiting` and `; stopped early: {detail}`
   when stopped. The CLI's `apb runs <id>`, the HTML report's outcome
   section (`report_html/sections.rs` line 368, which already prints
   `ListOutcome::line` and needs no change) and the outcome print this
   line.
4. `progress.rs` `compute_with`: `open_gates` (line 1071) gets an arm for
   a node with `list.pilot` whose review is pending (`review_pending`,
   line 1070) as `WaitingKind::HumanReview`, and `pending_reviews` (line
   1084) builds it with `pending_gate(events, n, &PILOT_OPTIONS as
   Strings, None)` (line 260) and then sets `pr.pilot =
   ListFold::of(events, &n.id).as_ref().and_then(PilotView::of)`.
   `PendingReview` (line 84) gets `pilot: Option<crate::list::view::PilotView>`
   with `#[serde(skip_serializing_if = "Option::is_none")]` and
   TypeScript optional, `None` in `pending_review` where every other
   optional field starts empty. `run_status`, `run_wait` and next actions
   then report the pilot gate like any gate.
5. `crates/apb-engine/src/next_action/unattended.rs` `outcome` (line 44),
   arm `ActionCode::DecideReview`: for a node with `list.pilot`, `Fail`
   gives `fails(HITL_REQUIRED)`; `Continue` gives `Decides { option:
   "continue" }` when the run's `ListFold` of the node has
   `pilot_needs_look() == 0`, else `fails(HITL_REQUIRED)` (decision 3).
6. Dashboard: `web/src/lib/ListProgressPanel.svelte` shows the pilot line
   (`pilot round 1: 3 items, 2 passed, 1 needs a look, waiting for a
   decision`) and the stopped detail, from new pure helpers
   `pilotLine(p)` and `stoppedLine(s)` in `web/src/lib/listprogress.ts`;
   `ReviewPanel.svelte` shows the slice's ids and counts for a pending
   review that carries `pilot`. Plain CSS over the existing CSS
   variables only.

### Docs

`docs/HOWTO-authoring.md` `### List processing (list:)` (line 1252) gets
`#### A pilot slice first (pilot)` and `#### Ending a visit early
(stop_when)`: the YAML, the three options and what each does, `then:
continue`, unattended (decision 3), the guidance section, resume and
retry behaviour, scenarios answering the gate, that a pilot decision
names its request (`decide_again` otherwise), how batches are cut around
the slice, that `stop_when` counts only items outside the slice and is
read after a batch's own pause or stop, the `stopped` output, V240 to
V242. `docs/GUARDRAILS.md` "Older apb binaries" names the `list_pilot`
guard with the other run guard names and says that apb 1.0.0-beta.1 and
later releases without this change reject a definition with `pilot` or
`stop_when`, while 0.25.0 drops the two fields and runs every item
ungated.

### Wrong implementations

1. Counting pilot items by attempts instead of settled items. Caught by
   `list_pilot_test::a_rejected_pilot_item_retries_inside_the_slice`
   (`batch.retries: 1`, one pilot item rejected once: the gate opens only
   after its second run settles, and the slice is still 3 ids).
2. Keeping pilot state outside `ListFold`. Caught by
   `list_pilot_test::a_resume_after_the_pilot_continues_without_rerunning_it`
   (pause the run at the gate, resume, decide `continue`: no pilot item
   runs twice) and
   `list_pilot_test::a_resume_inside_the_slice_finishes_the_slice_first`.
3. Running the pilot again on a supervisor retry or a `rerun: not_passed`
   visit. Caught by
   `list_pilot_test::a_supervisor_retry_keeps_the_decided_pilot` (a
   reopened visit after a failed node keeps `continue`) and
   `list_pilot_test::a_new_visit_under_rerun_not_passed_pilots_the_remaining_items`
   (carried items are not in the new slice).
4. `revise` that only reruns rejected items or forgets the note. Caught by
   `list_pilot_test::revise_reruns_the_slice_with_the_note` (all three
   slice items run again and their prompts end with the `## reviewer
   note` section; the rest also carry it).
5. A stop that kills items in flight or leaves the node succeeded. Caught
   by `list_pilot_test::stop_when_ends_the_visit_between_batches_failed`
   (`batch.size: 2`, `needs_look_at_least: 2`: the node finishes
   `failed` after the batch, the output has `stopped.reason ==
   "stop_when"`, the other items are `pending`).
6. A gate that waits in an unattended run, or an unattended run that
   continues past a bad slice. Caught by
   `list_pilot_test::an_unattended_pilot_fails_or_continues_as_declared`
   (`fail` ends `hitl_required`; `continue` with a clean slice runs the
   rest with `decided_by: unattended`; `continue` with a rejected slice
   item ends `hitl_required`).
7. Deciding the pilot through an option the channel does not check.
   Caught by `list_pilot_test::the_channel_refuses_an_approve_on_a_pilot_gate`.
8. Journaling a `review_detail` for a pilot gate (a form-gate path).
   Caught by `list_pilot_test::the_pilot_gate_shows_its_slice` (the
   journal has no `review_detail` of the list node, and the pending
   review carries `pilot` with the round and the three ids).
9. Consuming a pilot decision by count (review finding 4). Caught by
   `list_pilot_test::a_decision_for_a_withdrawn_pilot_request_never_answers_the_next`:
   the run waits at pilot request A and its drive is paused there
   (`--keep-lock`); a `continue` naming A is posted; the test appends
   `review_withdrawn { node, reason: "supervisor" }` for A through
   `EventLog` (what a `run_continue_from` writes) and resumes; the drive
   asks again with request B and keeps waiting (A's entry is never
   consumed, `apb_engine::run_doctor::diagnose_run` lists it under
   `reviews`); a `continue` with no request is refused `decide_again`
   naming B; a `continue` naming B is consumed. Under `Consume::Nth` the
   run would continue on A's entry: the test fails on main's rule.
10. Evaluating `stop_when` over an incomplete pilot (review finding 5, the
    review's example). Caught by
    `list_pilot_test::stop_when_waits_for_the_pilot_gate`: five inline
    items, `pilot: { items: 3 }`, `batch: { size: 2, retries: 0 }`,
    `stop_when: { needs_look_at_least: 1 }`, the first item rejected:
    two batches run (2 ids, then 1), the gate opens after the third item
    and no `list_stopped` is journaled; after `continue` the fourth and
    fifth items run (the slice's needs-look item is not counted) and the
    node succeeds; a variant whose fourth item is rejected ends with
    `list_stopped` after that batch, the fifth item pending.
11. Overriding a paused batch with an early stop. Caught by
    `list_pilot_test::a_paused_batch_is_honoured_before_stop_when`:
    `batch.size: 2`, `stop_when: { needs_look_at_least: 1 }`, the first
    item rejected and a `--now` pause posted while the second runs: the run
    pauses, no `list_stopped` is written and the second item is pending;
    on resume the rule holds before the next batch is admitted:
    `list_stopped` follows with no new `list_batch_started`.
12. Losing the stop or the slice on a crash. Caught by
    `list_pilot_test::a_crash_after_list_stopped_ends_the_node_stopped`
    (a journal cut after `list_stopped`, before `node_finished`: the
    resumed drive finishes the node failed with the same `stopped` and
    runs no item) and
    `list_pilot_test::a_crash_after_list_pilot_runs_the_same_slice` (a
    journal cut after `list_pilot`, before its first
    `list_batch_started`: the resumed drive runs the same three ids and
    writes no second `list_pilot`).
13. Taking two decisions for one pilot request, or one from a run step
    for its own run. Caught by
    `list_pilot_test::a_second_decision_for_the_same_pilot_request_is_refused`
    (two posts naming the open request: the second is refused by
    `check_bound_decision`'s duplicate check, and exactly one `review_decided` follows) and by
    the existing own-run refusal (`review::refuse_own_run`), which a
    pilot gate goes through unchanged
    (`list_pilot_test::a_run_step_cannot_decide_its_own_pilot`).
14. `revise` that repeats a send whose outcome is unknown, or loses its
    item from the slice (review round 3, blocker 3). Caught by
    `list_pilot_test::revise_never_reruns_an_item_with_an_unknown_send`:
    a pilot of three items under a `none` send, item 2's send left
    unknown as `list_resume_test::an_unknown_none_send_is_never_resent_until_a_person_resolves_it`
    leaves it; `revise` is decided: items 1 and 3 get `list_item_settled
    { state: interrupted, reason: pilot_revise }` and run again, item 2
    gets no settlement and no attempt, the `list_pilot` of round 2 names
    all three ids, and the gate after it counts item 2 as needing a
    look; the send ledger holds one intent for item 2, never a second.
    And by `list_pilot_test::a_crash_inside_revise_resumes_the_transition_once`:
    the same run with its journal cut after item 1's `pilot_revise`
    settlement and before item 3's (a crash partway through the
    transition): the resumed drive writes item 3's settlement only (no
    second one for item 1, none for item 2), exactly one `list_pilot` of
    round 2, and then runs items 1 and 3.
15. Taking field values on a pilot gate (review round 3, minor 8).
    Caught by `list_pilot_test::a_post_with_fields_on_a_pilot_gate_is_refused`:
    `review::post_decision` of `continue` naming the open pilot request
    with one field value is refused `gate \`{node}\` takes no fields`,
    `reviews.jsonl` is unchanged, and the same post without fields is
    accepted.


## D5. Readiness and probation before running alone

### The deterministic-check rule (apb-core)

`crates/apb-core/src/readiness.rs` (new, `pub mod readiness;` in
`lib.rs`), pure:

```rust
/// The most attended successes a probation may ask for (V249).
pub const MAX_PROBATION: u32 = 100;

/// The top-level `unattended:` block: how a playbook may run alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UnattendedPolicy {
    /// Attended successful runs of the exact definition, in this project
    /// on this machine, before an unattended start is admitted.
    pub after_attended_successes: u32,
}

/// Whether `node` has a check apb runs itself and whose failure fails
/// the node or the run: a `success_check` of the script forms, a `script`
/// list item check under `on_needs_look: fail`, or a `script` goal
/// criterion under `goal.enforce: true` (which checks the whole run).
pub fn has_deterministic_check(playbook: &Playbook, node: &Node) -> bool;

/// The acting `agent_task` nodes (`effects::node_acts`) without one, in
/// definition order.
pub fn unchecked_actors(playbook: &Playbook) -> Vec<&str>;

/// An acting node and the effect domain it has no boundary in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unbounded<'a> {
    pub node: &'a str,
    pub domain: BoundaryDomain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundaryDomain {
    /// The working tree the node runs in.
    Tree,
    /// One connector the node binds (its name).
    Connector(String),
    /// The network and external services outside the node's connector
    /// bindings: the `network` or `external` capability apb infers for
    /// it (`effects::node_inferred`) or that it declares
    /// (`Node::own_effects`); review round 3, major 7.
    Network,
    /// A `finish` node with a prompt: it runs an agent, and the format
    /// lets it declare no local boundary (review round 2, major 4).
    FinishAgent,
}

/// What this machine enforces of one node's isolation contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Enforced {
    /// The contract is allowed and runs in the sandbox.
    pub tree: bool,
    /// Also: the sandbox closes the network (`network: none` holds).
    pub network_closed: bool,
}

/// The acting nodes without a boundary, per effect domain, in definition
/// order (a node may appear once per domain). `isolation(id)` says what
/// this machine enforces of node `id`'s isolation contract; the engine
/// answers it, the rule stays pure.
pub fn unbounded_actors<'a>(
    playbook: &'a Playbook,
    isolation: &dyn Fn(&str) -> Enforced,
) -> Vec<Unbounded<'a>>;

/// Nodes whose own effects include `irreversible` and that a run can
/// reach without passing a `human_review` or `publish` node, along edges
/// and along the failure transitions of the nodes it reaches.
pub fn ungated_irreversible(playbook: &Playbook) -> Vec<&str>;
```

#### What counts as deterministic proof (review finding 6)

Only a check apb runs itself, outside the agent's own text, and whose
failure fails something, counts. `has_deterministic_check(playbook,
node)` is true when one of these holds:

1. `node.success_check` is `SuccessCheck::Script(_)` or
   `SuccessCheck::Scoped { .. }` (`schema.rs` line 1888). A failing
   script rejects the attempt, and the node fails when its retries run
   out.
2. `node.list()` has a `check` whose `source()` is `CheckSource::Script`
   (`list/mod.rs` lines 218 and 239) and `on_needs_look` is
   `OnNeedsLook::Fail` (`list/mod.rs` line 182): an item the script
   rejects makes the node fail. Under the default `on_needs_look:
   continue` the node succeeds with items that need a look, so the check
   proves nothing about the run.
3. `playbook.goal` has `enforce: true` (`schema.rs` line 301) and a
   criterion `GoalCheck::Script { .. }` (`schema.rs` line 269): a failed
   criterion fails the run (`scheduler/goal.rs`, `enforced`). It counts
   for every node, since it checks the whole run.

Not proof, each with its reason (confirmed on main):

1. `SuccessCheck::Marker`: the agent's own text.
2. A list item check of `CheckSource::Output`: it compares a field of the
   agent's own output (`scheduler/list/check.rs` `output_mismatch`, line
   85).
3. A list item check of `CheckSource::Connector` or `Judge`: a judge is a
   model's verdict; a connector check is not one of the three forms this
   change counts (a later change may add it with its own failure rule).
4. A list without a `check`: every item passes by itself
   (`scheduler/list/check.rs` line 72).
5. A `GoalCheck::List` criterion: it accounts for items, and an unchecked
   item counts as passed (`scheduler/goal.rs` `list_status`, line 96).
6. Any goal criterion while `goal.enforce` is false: it is reported and
   never fails the run (`scheduler/goal.rs` line 195).

`unchecked_actors`: nodes with `NodeKind::AgentTask`,
`effects::node_acts(node)` (`crates/apb-core/src/effects.rs` line 35)
and not `has_deterministic_check`.

#### Boundaries per effect domain (review finding 7)

A boundary is judged per effect domain of the actor. A local bound never
bounds a connector, and a connector bound never bounds the tree: a CRM
node with `read_only: true` that writes through a connector is unbounded
in that connector's domain. The domains:

1. Tree (the working tree the node runs in). Bounded by `node.read_only`
   (`schema.rs` line 1560), by `node.writes` (line 1570), or by an
   isolation whose contract is enforced on this machine. Enforced means
   `apb_core::isolation::contract::evaluate` gives a contract that is
   `allowed()` and whose `backend` is `Backend::Bwrap`: `full` on a
   machine that can isolate it, and `best_effort` only when the sandbox is
   really used. A `best_effort` node that falls back to no sandbox
   (`contract::without_sandbox`, `contract.rs` line 444, backend
   `Backend::None`) is not bounded. `max_changed_files` alone is not a
   boundary: it caps a count, not what is written.
2. Connector (one per binding, `NodeKind::connector_bindings`,
   `schema.rs` line 798; only an `agent_task` binds connectors). Bounded
   only by what the playbook declares for that binding
   (`ConnectorBinding`, `schema.rs` line 1323): `functions: read_only`
   (`FunctionsAllow::ReadOnly`), or `functions` given as a list of names
   (`FunctionsAllow::List`) together with `max_calls`. `functions: all`,
   or a list without `max_calls`, is unbounded. `read_only`, `writes` and
   isolation never bound this domain. Whether an irreversible call is
   behind a gate is the separate readiness item `ungated_irreversible`.
3. Network (review round 3, major 7). apb already infers that every
   acting `agent_task`, `script` and prompted `finish` can reach the
   network and external services (`effects.rs` `node_inferred`, line 63:
   a node that takes the workdir lock gets `ACTING`, which holds
   `Effect::Network` and `Effect::External`), whatever it declares: a
   `read_only` script can send an HTTP request and leave its working tree
   unchanged. An actor whose `node_inferred(node)` or `own_effects()`
   (`schema.rs` line 569) holds `Network` or `External` is judged in this
   domain, whatever its `read_only`, `writes` or connector bindings, and
   is bounded only by a network boundary this machine enforces: an
   isolation contract that is `allowed()`, whose `backend` is
   `Backend::Bwrap` and whose `holds` contain `Code::NetworkNone`
   (`isolation/contract.rs` line 109; declared as `isolation: { level,
   network: none }`). Nothing else bounds it: `read_only` and `writes`
   bound the tree only, a connector bound limits that connector's calls
   only, and a script takes no isolation, so every acting script is
   unbounded here. `node_inferred` becomes `pub fn node_inferred(n:
   &Node) -> BTreeSet<Effect>` (its body unchanged) so the rule and the
   run policy read the same inference. This replaces round 1's "declared
   external" domain, which judged a declared `external` effect only.

What counts for each actor kind:

| Actor kind | Tree | Connector | Network |
|---|---|---|---|
| `agent_task` | `read_only`, `writes`, enforced isolation | each binding: `read_only` functions, or named functions with `max_calls` | enforced isolation with `network: none` only |
| `script` | `read_only`, `writes` (no isolation on a script, `NodeKind::isolation` line 653) | none (a script binds none) | none: always listed |
| `finish` with a prompt (runs an agent, `takes_workdir_lock` line 721) | none can be declared: V137 refuses `read_only` and V170 refuses `writes` on a finish node (`validate/read_only.rs` line 18, `validate/write_scope.rs` line 18), and it takes no isolation; always listed with the domain `FinishAgent` (review round 2, major 4: nothing is extended) | none | none: always listed |
| `playbook` (a sub-playbook) | not judged here: the child's own report judges it, and probation counts the child on its own (below) | | |
| `publish` | not an actor here: it is a gate itself, held by a person's consent | | |
| `human_review`, `judge`, `wait`, `start`, `finish` without a prompt | do not act (`effects::node_acts` false) | | |

An actor's own network reach outside connectors (a shell command inside
an agent, an HTTP request from a script) is judged in the network
domain above: only an enforced isolation with `network: none` closes
it, so on most machines most playbooks list their scripts and agents
there. That is the honest answer: apb does not read what a script or an
agent sends, and readiness does not claim a bound it does not enforce.

`unbounded_actors` walks the nodes in definition order and, for each
actor of the table, adds one `Unbounded` per domain without a bound.

#### The irreversible-gate walk (review finding 8, round 3 major 6)

A failure route is an entry path only from a node that can fail and that
a run reaches, and whether a gate was passed on the way is carried along
it (review round 3, major 6). Round 1's version seeded every failure
handler as if a run could start there; for `start -> human_review ->
script` with an irreversible `defaults.on_failure` handler it listed the
handler as ungated, although every failure that routes there happens
after the review. `graphutil.rs` is not changed: V07 and the run graph
keep `reachable_with_policies` as it is.

`ungated_irreversible` in `readiness.rs` walks states `(node, gated)`
breadth first, `gated` meaning a gate was passed on the way: a
`publish` node, or a `human_review` that no decision model may decide.
A `human_review` with `auto_decide` and `auto_decide_ok: true` can be
decided by a decision model in enforce mode, so it is not a gate in
this walk (review fixes round 1, A8):

1. Seed: `(start, false)`. A playbook without exactly one start node
   lists nothing (V03 refuses it, `validate/graph.rs` line 32).
2. Edges: from a state `(u, g)`, every edge `u -> v` of
   `graphutil::adjacency(playbook)` (line 22, all edges, failure edges
   included) gives `(v, g || u is a gate)`: leaving a gate by any of
   its edges passes it.
3. Failure transitions: from a state `(u, g)` whose node can fail (every
   kind except `start`), every target of `failure_targets(playbook, u)`
   gives `(t, g)`: a node that fails has not passed itself, so a gate
   that fails carries the `g` it was reached with. `failure_targets` (a
   private function of `readiness.rs`) returns, in this order:
   1. `playbook.defaults.on_failure.target_for(u)` (`schema.rs` line
      1164; `None` for `route`, `stop` and the handler's own failure)
      when it names a declared node;
   2. for every node with `fork: { on_branch_failure: fail_fast,
      on_failure: Some(t) }` whose `crate::fork::fork_region(playbook,
      fork)` (`fork.rs` line 121) has `u` in `branches`: `t`.
   It is conservative: a node whose failure an edge of its own handles
   still gets the `defaults.on_failure` transition, which can only list
   more, never less.
4. Each state is visited once (at most two per node). A node is listed
   when a state `(n, false)` is reached, its `own_effects()` contain
   `Effect::Irreversible` and it is not a `publish` node; listed once,
   in definition order.

### The top-level block

`Playbook` (`schema.rs` line 139) gets, after `health` (line 215):

```rust
/// How the playbook may run alone (before-running-alone D5). Absent: no
/// probation from the playbook (`policy.yaml` may still ask for one).
/// An apb older than this change drops the block (section "Older apb
/// binaries" below).
#[serde(default, skip_serializing_if = "Option::is_none")]
pub unattended: Option<crate::readiness::UnattendedPolicy>,
```

(The node-level `unattended` of a gate, `Node.unattended` at `schema.rs`
line 1590, is a different field on `Node`; the two never meet.) Every
literal `Playbook { .. }` construction the compiler names gets
`unattended: None`.

### V249, V250

`validate/readiness.rs`:

1. V249 (error, playbook-level): `after_attended_successes` 0 or above
   `MAX_PROBATION`: `unattended.after_attended_successes is {n}; use 1 to
   100`.
2. V250 (warning, on the node), only when `playbook.unattended` is
   `Some`: one per `unchecked_actors` id: `acting agent_task \`{id}\` has
   no deterministic check, and this playbook means to run alone; add a
   script success_check, a script list item check with on_needs_look:
   fail, or a script goal criterion with goal.enforce: true`.

### Probation counting (apb-engine)

Probation is counted from its own ledger, never from run directories
(review round 2, finding 2 and the owner's redesign): a run that ends
leaves one entry in a signed per-playbook, per-project file, and the start
check reads only that file. Pruning a run changes nothing, a run is
counted once by its id by construction, and the retention archive is not
touched by this change (its summary and its MAC stay as on main).

#### Scope: per project root

Probation is counted per project root, by design: the ledger lives under
the project the run ran in, and only runs of that project on this
machine are in it. A global playbook earns its probation in each project
it runs in: an attended success in project A says nothing about the
data, connectors and working tree of project B. The refusal text and the
docs say so. A machine-wide record was rejected: it would let a playbook
proven on one project run alone on another, which is what probation
exists to prevent.

#### The ledger (apb-core)

`crates/apb-core/src/probation.rs` (new, `pub mod probation;` in
`lib.rs`), on `apb_core::signed_store` like the corrections ledger
(`corrections/store.rs`) and the known-problems store. The file is
`<root>/.apb/probation/<scope>/<id>.json` (`signed_store::file`, `DIR`
`probation`), 0600, written atomically under the lock of its directory,
and git-ignored: `probation/` is added to `workspace::LOCAL_ENTRIES`
(`crates/apb-core/src/workspace.rs` line 27, after `corrections/`, with
the comment "The probation ledger of each playbook, signed by this
installation (`apb_core::probation`).").

```rust
/// The file format this build reads and writes.
pub const SCHEMA: u32 = 1;
/// The most entries a file keeps; the oldest go first (dropping an
/// entry can only lower a count).
pub const MAX_ENTRIES: usize = 1000;

/// The probation ledger of one playbook in one project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbationFile {
    pub schema: u32,
    pub scope: String,
    pub playbook: String,
    /// The project the ledger belongs to (review fixes round 1, A5): the
    /// workspace id registered at the project root, else the canonical
    /// project root. Part of the identity and the MAC and compared on
    /// read, so a ledger copied or linked from another project does not
    /// verify.
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub entries: Vec<ProbationEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}

/// One ended run, written once after its terminal event and never changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbationEntry {
    /// The run id: one entry per run.
    pub run: String,
    pub version: String,
    /// The trust digest the start verified and journaled
    /// (`run_provenance.digest`, `LoadedPlaybook::trust_digest`): the
    /// definition and its scripts. Empty when the journal has none.
    pub trust_digest: String,
    /// The digest of the definition the run started with
    /// (`run_started.definition`). Empty when the journal has none.
    pub definition: String,
    /// Whether the run ran unattended (its manifest's `unattended`).
    pub unattended: bool,
    /// `succeeded`, `failed` or `aborted`.
    pub outcome: String,
    /// The run counts toward probation (section "Eligible" below).
    pub eligible: bool,
    pub ended_ms: u64,
}

/// Adds `entry` unless an entry for its run is there; drops the oldest
/// beyond `MAX_ENTRIES`. Returns whether the file changed.
pub fn add(file: &mut ProbationFile, entry: ProbationEntry) -> bool;

/// The eligible attended successes whose trust digest is `trust` and
/// whose definition is `definition`.
pub fn counted(file: &ProbationFile, trust: &str, definition: &str) -> u32;
```

`impl SignedDoc for ProbationFile`: `MAC_DOMAIN` `apb-probation-v1`,
`SCHEMA` 1, `DIR` `probation`, `ENTRIES_KEY` `entries`, `fresh`,
`identity`, `mac`, `set_mac`, `set_schema` as `CorrectionFile` has them,
with the project in the identity: a file whose `project` is not the
reading project's reads as not verifying. The probation file is opened
through `fsutil::beneath` (no link followed at any component), so a
link to another project's ledger is refused as well. Only the probation
ledger changes; the corrections and problems ledgers keep their
identity.

#### The writer: after the terminal event (apb-engine)

The entry is written only after the run's terminal event is in the
journal, and its outcome is read from that event (review round 3,
blocker 1): a run that never reached `run_finished` cannot be written as
a success. The site is the journal's terminal observer,
`crates/apb-engine/src/terminal_hook.rs`, chosen over the four end paths
(`scheduler/finish_hooks.rs` line 26, `scheduler.rs` lines 1051 and
3603, `stop.rs` line 269) because:

1. The finish hooks run before `run_finished` is appended, so a writer
   there cannot read the outcome from the terminal event; it would have
   to be told it, which is the defect round 3 found.
2. The observer is called from the one append path
   (`EventLog::append_inner`, `event.rs` line 3043) once a `run_finished`
   or `run_aborted` line is in the file, synced
   (`syncs_before_release`, line 3058) and the journal lock is released,
   whoever wrote it. That covers every end path, the four above and the
   ones that do not call `run_end::record` (a superseded waiting run's
   `run_aborted` and an unclaimed or abandoned preparation's
   `run_finished`, `scheduler/entry.rs` `close_out_unclaimed` line 372
   and `abandon` line 494).
3. It can carry a second observer without ordering trouble: the health
   observer and the probation writer share no file and no lock (the
   health state and the probation ledger are separate signed files
   under their own locks), neither reads what the other writes, and both
   run after the journal lock is released.

The change to `terminal_hook.rs`: a second slot beside the health one,
`static LEDGER: OnceLock<fn(&Path)>` with `pub fn install_ledger(f:
fn(&Path))` (first registration wins, as for `install`), and `observe`
calls the health observer first and the ledger observer second, each
when registered; the module doc names both. `crate::probation::install()`
registers `crate::probation::observe_ended`; the `apb` binary calls it
next to `apb_engine::health::install()` (`crates/apb-cli/src/main.rs`
line 1083), and so does the engine's test harness
(`crates/apb-engine/tests/main.rs` line 84), as both do for health
today. `run_end.rs` and its four callers are not changed: `run_end::record`
keeps recording known problems and corrections as on main.

`crates/apb-engine/src/probation.rs` (new, `pub mod probation;` in
`lib.rs`) holds `pub fn install()`, `fn observe_ended(run_dir: &Path)`
and `pub fn doctor_line(root: &Path) -> Option<String>`. Nothing else
writes the ledger: no MCP tool, HTTP route or CLI command, so a run
step's request can never add an entry; only a terminal event does.

`observe_ended`, best effort (a fault is one stderr line and changes
nothing about the run), in this order:

1. The journal is read strictly (`event::read_all`); a journal this apb
   reads only in part writes nothing.
2. The outcome: the last `run_finished` or `run_aborted` of the journal.
   None, or a `run_resumed` after it (a later append made the run live
   again before this observer read): nothing is written; the end that
   follows calls the observer again. `run_finished { outcome }` gives
   `outcome` as journaled (`succeeded`, `failed`, and any other word as
   it is); `run_aborted` gives `aborted`.
3. `known_problems::started(&events)` gives the playbook id and the scope
   (the `run_provenance.origin` label); none: nothing.
4. `external_data::learns_from(run_dir)` false (a scenario, an eval, an
   external-data run, a manifest that does not read): nothing is written.
   Such a run could never be eligible, and an external-data run keeps
   nothing in a learning store (docs/EXTERNAL-DATA.md). A scenario run
   never reaches it in practice (its process is a scenario child), and
   this step holds it all the same.
5. The entry: the run id (the directory name), `run_started.version`,
   `run_provenance.digest`, `run_started.definition`, the manifest's
   `unattended`, the outcome of step 2, `eligible` (below) and the time.
6. `signed_store::transact::<ProbationFile, _, _>(root, scope, &id,
   Unsigned::Refuse, |f| Ok(((), probation::add(f, entry))))`, with
   `root` from `run_config::project_root_of(run_dir)` as the health
   observer finds it. A file that does not verify is refused, nothing is
   written, and the line
   ``apb: the probation ledger of `{id}` does not verify; this run is not recorded in it``
   goes to stderr. A run whose id has an entry already (a run resumed
   after it ended, which ends again) keeps its first entry: written once.

A crash between the terminal event and the ledger write leaves the run
without an entry: it is not counted, which is the safe side (probation
asks for more attended runs, never fewer). No reconcile pass is added:
nothing reads the journal again to fill a missing entry, and a missing
entry is never repaired from a later read. A crash before the terminal
event leaves no entry either; a later `apb stop` of that dead run
appends `run_aborted` (`stop.rs` line 265), the observer writes the run
as `aborted` and not eligible, so an aborted run never has an eligible
entry. `apb stop` of a run whose `run_finished` is already in the
journal returns `AlreadyTerminal` (`stop.rs` line 203) and appends
nothing, so it never turns a written success into an abort or the
reverse.

A child run ends through the same append path with its own journal, so
it writes into its own playbook's ledger (its id from its own
`run_started`, the project root its run directory belongs to). A child
of an attended parent has `unattended: false` in its manifest; a child
of an unattended parent has `true`.

#### Eligible: the exact test

An entry is `eligible` when all of these hold, else it is written with
`eligible: false`:

1. The outcome is `succeeded`: the last terminal event is `run_finished`
   with `outcome: succeeded` (a `run_aborted` is never eligible).
2. The run learns (step 4 above: a production run, not an eval, a
   scenario or an external-data run). Every written entry passes this.
3. The journal executed one definition from start to end:
   1. exactly one `run_started` event, and its `definition` is `Some(d)`;
   2. a `run_provenance` event whose `digest` is `Some(_)`;
   3. no `patch_applied` event (an in-run patch put another definition
      in force);
   4. no `profile_rebound` event (a rebind changed a node's executor
      after the start);
   5. no `run_migrated` event (a schema migration rewrote the
      definition);
   6. every `run_resumed` event carries `definition: Some(d)` with the
      same `d` (a resume that executed other bytes, or one an older apb
      journaled without a digest, is not the same definition).
4. The start applied no override: the run's `run.yaml`
   (`run_config::RunConfig`, read with `run_config::read_run_config`) has
   `overrides` absent or empty; a `run.yaml` that does not read is not
   eligible. Overrides are not journaled; `RunConfig` is where the start
   keeps them (`scheduler/prepare.rs` line 1353). With overrides the
   start journals the digest of the serialized effective playbook
   (`prepare.rs` line 1262), so the start check's definition equality
   below refuses such a run too; the `run.yaml` test makes the entry say
   so itself. A field on `run_started` was rejected: 107 files construct
   that event literally.
5. The run was not started from a run step: its manifest's launch record
   has no `started_by` (review fixes round 1, A3). A run step is never a
   person, and a step that starts runs and answers their gates would
   otherwise earn probation with no person involved.

"Attended" means only that a run was started without `--unattended`.
The count is of attended runs; no surface and no refusal says that a
person decided anything in them.

Unattended runs are written with their outcome and `eligible` as above;
the start check counts only `unattended: false` entries.

#### The check: the ledger only

In `crates/apb-engine/src/gate/unattended.rs`:

```rust
/// Where a probation stands for one definition in one project.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbationView {
    /// Attended runs asked for (the largest of the playbook's and the
    /// two `policy.yaml` files).
    pub required: u32,
    /// Eligible successful attended runs (started without --unattended)
    /// of this exact definition in this project's probation ledger.
    pub counted: u32,
    /// `playbook`, `machine` or `project`: which asked for `required`.
    pub source: String,
    /// The machine's `policy.yaml` waives probation here.
    pub waived: bool,
    /// The ledger verified (or does not exist yet). False: nothing
    /// counts, and an unattended start is refused unless waived.
    pub ledger_ok: bool,
}

/// The probation of the loaded definition `loaded` (scope label `scope`)
/// for the project at `root`; `None` when nothing asks for one.
pub fn probation(root: &Path, scope: &str, loaded: &LoadedPlaybook) -> Option<ProbationView>;

/// Refuses an unattended start whose probation, or the probation of a
/// sub-playbook it pins, is not done and not waived.
pub fn check_probation(
    root: &Path,
    scope: &str,
    loaded: &LoadedPlaybook,
    pins: Option<&BTreeMap<String, ChildExpectation>>,
    waive: bool,
) -> Result<(), Value>;
```

1. `required`: the largest of `loaded.playbook.unattended`'s value, the
   machine `policy.yaml` `unattended.after_attended_successes` and the
   project's (section "The unattended section of policy.yaml"), with its
   `source`; all absent or 0: `None`.
2. `counted`: `signed_store::read::<ProbationFile>(root, scope,
   &loaded.playbook.id)`; a missing file is a fresh signed one (0). A
   file that does not verify or parse (`signed == false`), one of a newer
   schema, or a read error sets `ledger_ok: false` and `counted: 0`: fail
   closed. Else `probation::counted(&file, &loaded.trust_digest()?,
   &run_definitions::digest(loaded.yaml.as_bytes()))`: the eligible,
   attended, succeeded entries whose trust digest is the digest the start
   verified (definition and scripts, so a changed script restarts the
   count) and whose definition is the digest of the loaded
   `playbook.yaml` (so an overridden run never counts). A run of another
   version, of an edited `playbook.yaml` or with a changed script never
   counts; a new version starts at 0. Runs that ended before this change
   have no entry and never count (release note 4).
3. `check_probation`: for the loaded definition, then (only when `pins`
   is `Some`) for every pinned sub-playbook, depth first in node order,
   each child's ledger read in the child's own scope,
   each through `ChildExpectation::load_definition(root)` (new in
   `run_config.rs`, the body of `ChildExpectation::load`, line 51,
   returning the whole `LoadedPlaybook`; `load` becomes
   `self.load_definition(root).map(|l| l.playbook)`) with the scope label
   of the pin (`project` or `global`) and its own ledger. The first that
   is not done refuses. `probation(...)` `None`, `waived`, `waive`, or
   `ledger_ok` with `counted >= required`: that one passes. The machine
   waiver (`waived`) passes a ledger that does not verify as well as the
   typed one does (review fixes round 1, A6 and M3), and the fit report's
   probation item agrees. A pin that no
   longer loads refuses (fail closed). The refusal:
   `json!({"policy": PROBATION_REQUIRED, "requirement":
   "attended_successes", "detail": <text>})` with the text, final:
   - the playbook itself: ``playbook `{id}` {version} runs unattended only after {required} successful attended runs (started without --unattended) of this definition in this project; {counted} so far here (probation is counted per project from its probation ledger, also for a global playbook; runs that ended before this apb kept the ledger do not count). Run it attended in this project, or waive probation at a terminal with --waive-probation``
   - a pinned child: ``sub-playbook `{child}` {version} (node `{node}`) runs unattended only after {required} successful attended runs (started without --unattended) of its definition in this project; {counted} so far here (probation is counted per project from its probation ledger, also for a global playbook; runs that ended before this apb kept the ledger do not count). Run it attended in this project, or waive probation at a terminal with --waive-probation``
   - a ledger that does not verify (the playbook or a child, `{what}` being ``playbook `{id}` `` or ``sub-playbook `{child}` (node `{node}`)``): ``the probation ledger of {what} in this project ({path}) does not verify, so its probation cannot be counted; move the file aside (the count starts again from zero) or waive probation at a terminal with --waive-probation``
   - a pin that does not load: ``sub-playbook `{child}` (node `{node}`) cannot be read to count its probation; start the run again once it reads``
   - a `policy.yaml`, the machine's or the project's, that does not read
     (review fixes round 1, A1): the floor it may set is unknown, so an
     unattended start fails closed with `probation_required` and a detail
     naming the file and its error, unless the start carries the typed
     waiver. The policies are loaded once per check, and the floor and the
     machine waiver come from that one load.
   - an unattended start that carries non-empty run overrides (review
     fixes round 1, A4): ``an unattended start runs the definition its probation counted; start it without --overrides or waive probation at a terminal``,
     unless the start is waived (typed or by the machine).
   `pub const PROBATION_REQUIRED: &str = "probation_required"` lives in
   `crates/apb-engine/src/run_config.rs` next to `UNATTENDED_UNSUPPORTED`
   (line 269), and `pub const NOT_READY: &str = "not_ready"` beside it
   (decision 8). Both functions and `ProbationView` are re-exported from
   `gate/mod.rs` (line 31, beside `check_unattended`).
4. The doctor: `probation::doctor_line(root)` walks
   `.apb/probation/project/` and `.apb/probation/global/` the way
   `start_key::doctor_line` (`start_key.rs` line 459) walks its files and
   returns, when any file does not verify, ``probation ledgers that do not verify (unattended starts of their playbooks are refused until each is moved aside; the count then starts again from zero): {paths}``.
   `apb doctor` (`crates/apb-cli/src/doctor.rs`, after the
   `start_key::doctor_line` block at line 158) prints it as a `WARN` line
   with the subject `probation`, through `sanitize_for_terminal`.

#### Where it runs: one site, on the loaded definition (review round 1 findings 2 and 3, round 2 findings 4 and major 3)

The probation check and the `--require-ready` check run in exactly one
place, in `crates/apb-engine/src/scheduler/prepare.rs` `prepare_placed`
(line 666), on the definition preparation itself loaded and will
execute, before the run directory exists:

1. Site: right after the import-approval block
   (`crate::gate::check_import_approval(&playbook_dir, id,
   &loaded.version, &digest)`, line 718, inside `if scenario.is_none()`)
   and before `let mut playbook = loaded.playbook.clone();` (line 728).
   At that point `reg.load(id, version)?` (line 685) has loaded the
   version the gate selected (a waiting candidate included), its
   `trust_digest()` has been checked against `opts.expected_digest`, and
   nothing is written; `allocate_run` (line 1163) comes much later.
   `unattended_start` (line 515) cannot hold it: it runs before the load
   and sees only `root` and `RunOptions`.
2. Probation first: when `opts.unattended` and `scenario.is_none()` (a
   scenario run executes nothing),
   `crate::gate::unattended::check_probation(root, t.origin_label,
   &loaded, if opts.parent_run.is_none() {
   opts.expected_children.as_ref() } else { None },
   opts.waive_probation)`, mapping a refusal to
   `EngineError::Invalid(format!("{PROBATION_REQUIRED}: {detail}"))`.
3. Readiness second (review round 2, blocker 4 and major 3): when
   `opts.unattended`, `opts.require_ready` and `scenario.is_none()`,
   `crate::fit::readiness::start_check(root, t.origin_label, &loaded,
   origin, pins)` with `origin` the `PlaybookOrigin` preparation derives
   from `t.origin_label` (as at line 792, computed here once and reused
   there) and the same `pins` as step 2. It assesses the loaded
   definition, then every pinned sub-playbook through
   `ChildExpectation::load_definition`, with every item except
   `probation` (decided in step 2, so a waived start is not refused on
   it here), and returns the first that is not ready. Refusals, final:
   - the playbook itself: ``not_ready: playbook `{id}` {version} is not ready to run alone: {detail}``
   - a pinned child: ``not_ready: sub-playbook `{child}` {version} (node `{node}`) is not ready to run alone: {detail}``
   - a pin that does not load: ``not_ready: sub-playbook `{child}` (node `{node}`) cannot be read to assess its readiness; start the run again once it reads``
   with `{detail}` from `not_ready_detail` over the items it assessed.
   Both arrive as `EngineError::Invalid`.
4. Children (round 1 finding 2). A child's probation and readiness are
   its own, never its parent's:
   1. At the top-level start, every sub-playbook the gate resolved and
      pinned (`opts.expected_children`, built by `gate::collect_children`,
      `gate/mod.rs` line 1333) is checked with its own definition, so a
      child that is under probation or not ready refuses the parent's
      start before anything runs, and the refusal names the child and
      its node.
   2. Where a child run is created: every child run passes through
      `prepare_placed` too (`scheduler/node.rs` `child_run_options`, line
      4733, `parent_run: Some(..)`), inherits unattended mode, the waiver
      and the readiness demand in `unattended_start` from its parent's
      manifest, and is checked at the same site on its own loaded
      definition (`pins` `None`: its own children were checked by the
      top-level start, or are checked when they are created). This
      catches a child resolved only then; for a pinned child it repeats
      the top-level check on the same bytes (`expected_digest` holds
      them) and passes.
5. `RunOptions` (`crates/apb-engine/src/scheduler/entry.rs` line 11)
   gets two fields:
   - `pub waive_probation: bool`, doc "A person at a terminal typed the
     confirmation that waives probation for this unattended start and
     every child run it starts (`apb run --waive-probation`); a child
     takes its parent's";
   - `pub require_ready: bool`, doc "Refuse this unattended start, and
     every child run it starts, when the readiness report of its loaded
     definition does not pass (`apb run --require-ready`); a child takes
     its parent's".
   Only the CLI sets them, for a top-level start. `RunExecutionManifest`
   (`manifest.rs` line 299) gets two fields, each `#[serde(default,
   skip_serializing_if = "std::ops::Not::not")]`, written where the
   manifest records `unattended` (`prepare.rs` line 1382), true only for
   an unattended run: `pub probation_waived: bool` and `pub
   readiness_required: bool`. `unattended_start` sets
   `opts.waive_probation` and `opts.require_ready` of a child run from
   its parent's manifest, exactly as it sets `opts.unattended` from it
   today.
6. The launch surfaces add no check of their own: the CLI, MCP and HTTP
   starts all reach `prepare_placed`, and each returns its refusal the
   way it returns every other preparation refusal (`run failed:
   probation_required: ...` or `run failed: not_ready: ...` with exit 2
   on the CLI; a tool error whose text starts with the code on MCP; 422
   with the text on HTTP, `apb-server` `state.rs` line 293). A start the
   dashboard queues on a busy working tree is refused when its
   preparation runs, as every preparation refusal of a queued start is.
   The CLI no longer assesses readiness before the gate (round 2,
   blocker 4): that was a second load of a version the gate could
   replace with a candidate.

#### A patch is held to the same checks (review round 3, blocker 2)

Both checks run once in preparation, and a supervisor's in-run patch
(`Control::Patch`, applied by `scheduler/patch.rs` `apply_patch`, line
34) loads another version and installs it as the run's snapshot. A run
that was started unattended, or with `--require-ready`, is therefore
held to the same checks again on the replacement definition and its
pinned children before the snapshot is replaced; otherwise the patch is
refused.

1. Site: `apply_patch`, right after the `super::approval::patch_refusal`
   block (line 130) and before
   `crate::run_redact::keep_secret_declarations` (line 140), so a refused
   patch has stored nothing, written no guard and left `playbook.yaml`
   as it was. At that point `loaded` (line 83) is the `LoadedPlaybook`
   of the patch version from `Registry::open(root)`.
2. What the run asked for: `crate::manifest::read(run_dir)` gives
   `unattended`, `probation_waived` and `readiness_required` (written at
   the start, section "Where it runs" step 5). A manifest that does not
   read refuses the patch with ``probation_required: the manifest of this run does not read, so whether it runs unattended cannot be told; the patch is refused``.
   An attended run (`unattended: false`) is not checked: the patch goes
   on as on main.
3. The scope label is the run's origin
   (`super::parent_run_origin(run_dir)`, `crate::run_origin`, as
   `uncovered_consent_sources` resolves it, line 219: `Origin::Global` is
   `global`, any other `project`), the pins are the run's own
   (`run_config::read_run_config(run_dir)`'s `expected_children` when
   the run has no `parent_run`, else `None`, as in preparation step 2; a
   `run.yaml` that does not read refuses the patch like a manifest that
   does not read), and the root is `root`.
4. Probation, when `unattended`: `crate::gate::unattended::check_probation(root,
   scope, &loaded, pins, manifest.probation_waived)`; a refusal becomes
   `append_patch_rejected(log, format!("{PROBATION_REQUIRED}: {detail}"))`
   with the texts of section "The check: the ledger only" step 3. The
   new definition has no attended runs of its own unless it was run
   attended before, so in practice a patch on an unattended run under
   probation is refused unless it was waived; that is the point.
5. Readiness, when `unattended` and `readiness_required`:
   `crate::fit::readiness::start_check(root, scope, &loaded, origin,
   pins)` with `origin` the `PlaybookOrigin` of the scope label; a
   refusal becomes `append_patch_rejected(log, <the text of "Where it
   runs" step 3, which starts with `not_ready: `>)`.
6. The refusal is journaled as every patch refusal is today: one
   `patch_rejected { reason }` (`append_patch_rejected`, line 26), counted
   against `max_patches_per_run` like the others, and the drive goes on
   with the running definition. The reason starts with the same code as
   the start refusal (`probation_required` or `not_ready`), so a
   supervisor reading the journal sees which check refused.
7. A forward patch (`forward_patch.rs`, a new version for later runs)
   is not checked: it does not change a running run, and the next
   unattended start of that version meets both checks in preparation.

Tests, in `crates/apb-engine/tests/suite/probation_test.rs`:
`a_patch_on_an_unattended_run_is_held_to_probation` (an unattended run
of a proven 1.0.0, a patch to 1.1.0 with no attended runs: one
`patch_rejected` whose reason starts `probation_required`, the run's
`playbook.yaml` unchanged, no `patch_applied`; the same patch on a run
started with `RunOptions.waive_probation` applies),
`a_patch_on_a_require_ready_run_is_held_to_readiness` (an unattended
`require_ready` run of a ready version, a patch to a version without a
budget: `patch_rejected` starting `not_ready`, and a patch whose pinned
child is not ready: refused naming the child),
`a_patch_to_a_proven_ready_definition_applies` (the patch version has
its own attended successes and passes readiness: `patch_applied`), and
`a_patch_on_an_attended_run_checks_neither` (an attended run patched to
an unproven, unready version applies as on main).

### The waiver (review round 1 finding 11, round 2 blocker 5)

A waiver lifts probation for one start and every child run of its tree.
apb has no way to prove that a person, and not a process, is at a
terminal, and this change does not claim one: it uses apb's existing
initiator model, where a typed confirmation at an interactive terminal
is the person's act (the model `apb review` uses for a person-only gate
and `apb resume --from` for a start past a gate, through
`crates/apb-cli/src/consent.rs` `ask_typed`, line 136).

1. `apb run --waive-probation` (`RunArgs`, `crates/apb-cli/src/run.rs`
   line 458, next to `unattended` at line 559), with `--unattended` only
   (without it: exit 2, `--waive-probation applies to an unattended
   start`):
   1. Inside a run step (`crate::util::StepCaller::from_env().describe()`
      is `Some`): exit 2, `--waive-probation is refused {from}: a run step
      does not waive probation`.
   2. Else, always, whatever the probation of the playbook or of any
      sub-playbook stands at (the CLI does not read it: a child it cannot
      see may need the waiver), `crate::consent::ask_typed(&[line],
      &question)` with `line`
      ``--waive-probation lifts probation for `{id}` and for every sub-playbook this run starts, in this project``
      and `question`
      ``Waive probation and start `{id}` unattended? Type the playbook id to confirm:``
      (both through `sanitize_for_terminal`). `None` (no interactive
      terminal): exit 2, `--waive-probation needs a person at an
      interactive terminal to type the playbook id; set
      unattended.waive_probation in the machine policy.yaml for a machine
      that runs alone`. A typed text other than the id: exit 2, `not
      confirmed: the text typed is not \`{id}\`; nothing was started`.
      The id typed: `RunOptions.waive_probation = true`.
   The question comes before the gate, so a refused confirmation starts
   nothing and writes nothing.
2. Where it is recorded and how far it reaches: preparation writes
   `probation_waived: true` into the run's write-once manifest
   (`RunExecutionManifest`) for an unattended start with
   `opts.waive_probation`; every child run of the tree reads its parent's
   manifest in `unattended_start` and gets `opts.waive_probation = true`
   and its own manifest field, so the waiver reaches every child start of
   the tree, a child created long after the start included, and no run
   outside the tree. A resume does not change it: the manifest is
   write-once.
3. MCP `playbook_run` and the HTTP start take no waiver: they have no
   field for it, so their starts are never waived (`RunOptions` default
   `false`).
4. `unattended.waive_probation: true` in the machine's `policy.yaml` is
   the unattended alternative: an operator sets it on a machine that runs
   alone (a CI runner with no attended runs). A project file cannot set
   it (section below). The redirected config dir rule of the launch gate
   covers it (review fixes round 1, A2): where `gate::launch`
   `effective_for` already compares a redirected config dir with the
   default one for a run step or a CLI without a terminal, it also
   compares their `unattended:` sections, and a redirected file that
   waives probation, or asks for fewer attended runs than the default
   one, is refused `policy_file_invalid`, as a laxer launch policy is.
5. The start request a start key binds (`StartRequestArgs`, line 703)
   does not carry the waiver: it is the act of that start, not part of
   the event.

What this does not claim: a process that controls a terminal and types
the id passes, as it passes every other typed confirmation of the CLI.
The waiver is recorded in the manifest of every run of the tree
(`probation_waived`), so a waived run stays recognisable afterwards; no
surface prints it in this change.

### The unattended section of policy.yaml

`PolicyFile` (`crates/apb-core/src/policy_file.rs` line 49) gets
`pub unattended: UnattendedSection`:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UnattendedSection {
    /// The least attended successes before any playbook runs alone in a
    /// project here (1 to 100); a project file may only raise the
    /// machine's.
    pub after_attended_successes: Option<u32>,
    /// Machine file only: no probation on this machine (a CI runner that
    /// never has attended runs). A person's file; nothing in apb writes it.
    pub waive_probation: bool,
}
```

`validate(source: PolicySource)`: a value outside 1 to 100 is
`unattended.after_attended_successes is {n}; use 1 to 100`;
`waive_probation: true` in the project file is `unattended.waive_probation
is a machine setting; a project cannot waive probation`. Called from
`parse_as` after `file.health.validate()?` (line 429) as
`file.unattended.validate(source)?`. `Policies` gets
`pub fn probation_floor(&self) -> Option<(u32, PolicySource)>` (the
larger of the two files' values with its source) and `pub fn
waives_probation(&self) -> bool` (the machine file's flag). The module
doc's example gets the section. The probation check loads `Policies` once
and reads the floor and the waiver from that one value; a load error is
never read as "no floor": it refuses an unattended start that is not
waived by the typed confirmation (review fixes round 1, A1; a project
file that sets `waive_probation` is such an error). The validation range
is `1..=readiness::MAX_PROBATION`, named in its message.

### The readiness section of the fit report

`crates/apb-engine/src/fit/readiness.rs` (beside the assessment that
builds them; `fit/report.rs` holds the report that carries them):

```rust
/// Whether a playbook is fit to run alone.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Readiness {
    /// Every item holds.
    pub ready: bool,
    pub items: Vec<ReadinessItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub probation: Option<crate::gate::unattended::ProbationView>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadinessItem {
    pub code: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub subject: Option<String>,
    pub detail: String,
}
```

Both are re-exported from `fit/mod.rs` (`pub use readiness::{Readiness,
ReadinessItem, not_ready_detail}`). `FitReport` (line 75) gets `pub readiness: Readiness` after `launch`;
`FitSummary` (line 101) gets `pub ready_alone: bool`. `unavailable`
(`fit/mod.rs` line 68) sets `Readiness { ready: false, items: vec![],
probation: None }`.

`crates/apb-engine/src/fit/readiness.rs` (new, `pub mod readiness;` in
`fit/mod.rs` after `mod report;`, public so the CLI and the later rule
start call `not_ready_detail`): `pub(crate) fn assess(root: &Path,
scope: &str, loaded: &LoadedPlaybook, origin: PlaybookOrigin) ->
Readiness`, called in `fit::assess` (`fit/mod.rs` line 92) where the
report is built (line 261), with the loaded version it reports on; `pub
fn not_ready_detail(r: &Readiness) -> Option<String>`: `None` when
`r.ready`, else the `detail` of every item that is not ok, joined by
`"; "` (decision 8); and `pub(crate) fn start_check(root: &Path, scope:
&str, loaded: &LoadedPlaybook, origin: PlaybookOrigin, pins:
Option<&BTreeMap<String, ChildExpectation>>) -> Result<(), String>`, the
preparation check of `--require-ready` (section "Where it runs" step 3):
`assess` on `loaded` and on every pin's `load_definition` (each child
with its own scope label and the `PlaybookOrigin` of its pin), its items
without the one whose code is `probation`, and the first refusal text of
that section. `isolation` is answered from
`crate::gate::isolation::node_contracts(root, &loaded.playbook, origin,
false)` (`gate/isolation.rs` line 28; `false`: an unattended run never
uses host execution): `tree` when the node's contract is `allowed()` and
its `backend` is `Backend::Bwrap`, and `network_closed` when, in
addition, its `holds` contain `Code::NetworkNone`. The items, in this
order, each `ok` or not:

1. `done_defined`: the goal has `enforce: true` and at least one `script`
   criterion (decision 4). Ok: `the goal has {n} enforced script
   criteria`; not: `nothing checks when the run is done: the goal has no
   script criterion under goal.enforce: true`.
2. `unchecked_actor` per `unchecked_actors` id (subject the node):
   `acting agent_task \`{id}\` has no deterministic check`; none: one ok
   item `checked_actors`: `every acting agent_task has a deterministic
   check`.
3. `unbounded_actor` per `unbounded_actors` entry (subject the node, and
   `{node}/{connector}` for a connector): tree: `acting node \`{id}\` can
   change the working tree without a boundary: set read_only or writes, or
   an isolation this machine enforces`; connector: `acting node \`{id}\`
   calls connector \`{c}\` without a limit: allow only read_only
   functions, or name its functions and set max_calls`; network:
   `acting node \`{id}\` is not bounded externally: it can reach the
   network and external services outside its connectors, and only an
   isolation with network: none that this machine enforces limits that`;
   finish agent: `finish node \`{id}\` runs an agent and cannot declare a
   local boundary (read_only, writes and isolation apply to agent_task
   and script nodes only): move the work into a bounded agent_task, or
   leave it unbounded`. None:
   one ok item `bounded_actors`: `every acting node has a boundary in the
   working tree, at every connector it binds and on the network`.
4. `budget`: `playbook.budget` (`schema.rs` line 188) is set. Ok: `a
   budget limits a run`; not: `no budget: a run alone spends without a
   limit`.
5. `ungated_irreversible` per id: `irreversible node \`{id}\` is reached
   without a human_review or publish gate, from the start or a failure
   route`; none: one ok item `irreversible_gated`: `every irreversible
   step is behind a gate`. A `human_review` a decision model may decide
   (`auto_decide` with `auto_decide_ok: true`) is not a gate here.
6. `probation` when `probation(...)` is `Some(p)`: ok when `p.waived`,
   or `p.ledger_ok` and `p.counted >= p.required`: `{counted} of
   {required} attended runs of this definition in this project`
   (`, waived on this machine` when waived, also over a ledger that does
   not verify); a ledger that does not verify and no machine waiver: not
   ok, `the probation ledger of this playbook in this project
   does not verify; move it aside to count again from zero`. Only the
   playbook itself: a pinned child's probation is refused at the start
   (above) and is not a readiness item.

`ready` is every item `ok`. Readiness never changes `FitReport.ready`
(that stays "nothing blocks a run here").

### The unattended start warns

1. `apb run --unattended` (`crates/apb-cli/src/run.rs`): after
   `gate_run` returned the version it selected (`start_version`, line
   1206), and only then, `apb_engine::fit::assess(root, &wref)` with
   `wref.version` that version (the one preparation loads, pinned by
   `expected_digest`); when `fit::readiness::not_ready_detail(&report.readiness)`
   is `Some(d)`, print to stderr `warning: not ready to run alone: {d}`.
   A fit report that cannot be made prints `warning: not ready to run
   alone: the fit report could not be made: {error}`. The warning decides
   nothing: it is printed whether or not `--require-ready` is given, and
   the refusal of `--require-ready` comes only from preparation (section
   "Where it runs" step 3). `--require-ready` is a new `RunArgs` flag next
   to `unattended`, help "Refuse an unattended start whose loaded
   definition, or a sub-playbook it starts, the readiness report does not
   pass", which sets `RunOptions.require_ready`; without `--unattended`
   it exits 2 with `--require-ready applies to an unattended start`.
2. MCP `playbook_run` (`crates/apb-mcp/src/server/run.rs`
   `playbook_run_for`, line 374) with `unattended: true` adds
   `"readiness": <Readiness>` to its answer when not ready (a host relays
   it). It takes no `require_ready` argument in this change (the later
   rules carry their own `require_ready`, over the same preparation
   field).

### Surfaces

1. `apb adopt <id>` (`Command::Adopt` at `crates/apb-cli/src/main.rs`
   line 192, `adopt_cmd` in `crates/apb-cli/src/manage.rs` line 89) prints
   the JSON of MCP `playbook_adopt_report`
   (`crates/apb-mcp/src/advisory_tools.rs` line 85, `fit::assess` at line
   205), so both carry `readiness` through `FitReport` with no change of
   their own.
2. Dashboard `web/src/lib/FitCard.svelte`: a "Running alone" block with
   each item (ok ones collapsed into a count), from a new pure helper
   `readinessLines(r)` in `web/src/lib/fit.ts` (tests in `fit.test.ts`).
   Plain CSS over the existing CSS variables only.
3. `Readiness`, `ReadinessItem`, `ProbationView` registered in
   `ts_contract.rs` next to `FitReport` (line 335).

### Older apb binaries (review round 1 finding 1, round 2 major 5, round 3 major 4)

Recorded as a limit, not fixed. It is the contract every added playbook
field has had: `Playbook` (`schema.rs` line 138) does not deny unknown
fields, so an apb older than this change loads a playbook with the
top-level `unattended:` block, drops the block, and starts it unattended
without counting probation, as an older apb that starts a playbook with
`person_only` drops that field and runs a plain gate
(`docs/GUARDRAILS.md` "Older apb binaries"). Probation is a guarantee of
this apb and newer only; nothing in this change claims it holds against
an older binary.

The playbook format has no way to demand a minimum apb. `requires`
(`schema.rs` line 160, `Requires` at line 256) names only the files and
commands a project must have, and `Requires` does not deny unknown fields
either, so a version key added there would be dropped the same way; this
change adds none. What holds the guarantee is the machine: every apb on a
machine that runs playbooks with `unattended:` must be this release or
newer, and `apb doctor` already warns about other `apb` binaries on
`PATH`.

Review round 2 (major 5) asked again for a fail-closed version
requirement or a checked prerequisite. Rejected again, recorded: the
format has no minimum-version field, and an older binary dropping a field
it does not know is the standing contract of every playbook field added
since schema 2 (`person_only`, `approves`, `deadline`, `concurrency`,
`health`). A minimum-version field would itself be dropped by every
binary released before it, so it could only fence binaries newer than
this change, which already honour the block. The limit stays in the
release notes and the docs.

Two neighbours behave differently, and the docs say so:

1. A list `pilot` or `stop_when`: from 1.0.0-beta.1 on `ListSpec` denies
   unknown fields, so such an older apb rejects the whole definition and
   starts nothing; 0.25.0 drops the two fields and runs every item with
   no pilot gate and no early stop (decision 7).
2. `policy.yaml` with `unattended:`: the strict reader of an older apb
   reads the file as invalid (release note 2); it does not count
   probation either way.

Review round 3 (major 4) asked a third time, now for a compatibility
marker the old binary rejects, such as a mandatory `unattended` entry in
the strictly read machine `policy.yaml`. Rejected a third time,
recorded: no machine-policy marker is added. The old binary already
reads a `policy.yaml` that holds the `unattended:` section as invalid
(release note 2), and what that refuses is the machine file's own
defaults, not the unattended start of a playbook whose `unattended:`
block the old binary drops; a marker that has to be present on every
machine for the guarantee to hold is the same operator duty the
paragraph above states, with a worse failure mode. The limit stays
stated in the docs and the release notes.

An older apb neither reads nor writes the probation ledger: its runs
leave no entry, so they never count toward probation here (a run's
end is what writes the entry).

### Docs

`docs/HOWTO-authoring.md` `### Before a playbook runs alone (readiness,
unattended:)` after `### A review in an unattended run (unattended)`
(line 2391): the six items with what counts as proof and as a boundary
per effect domain (the table above, the network domain included), V249,
V250, probation and how it counts (the probation ledger per project, an
entry per run written after its terminal event, a run whose process died
before the write left uncounted, what makes a run eligible, children on their own, runs before
this change not counted, a ledger that does not verify), `--require-ready`
on the loaded version and its sub-playbooks, and the older-binary limit. `docs/GUARDRAILS.md` `## Running alone: readiness and
probation` before `## Choosing where a rule goes` (line 626): the refusal
codes `probation_required` and `not_ready`, the waiver (a typed
confirmation at an interactive terminal, the machine file) and what it
does not prove, that it always asks and covers every sub-playbook of the
run, that a patch of an unattended run is held to the same checks, why a
project cannot waive, the `apb doctor` line of a ledger that
does not verify, and one paragraph added to "Older apb binaries" with
the two cases above.

### Wrong implementations

1. Counting successes of any version, or of a definition whose scripts
   changed. Caught by `probation_test::a_new_version_restarts_probation`
   (two attended successes of 1.0.0, then 1.1.0 with
   `after_attended_successes: 2` counts 0),
   `probation_test::an_edited_definition_does_not_count`
   (`playbook.yaml` of 1.0.0 edited in place: its earlier entries no
   longer count) and `probation_test::a_changed_script_resets_the_count`
   (two attended successes, then a script of the version changed and
   approved: the trust digest differs and the count is 0).
2. Counting unattended, failed, external-data, eval or scenario runs.
   Caught by `probation_test::only_attended_successes_count` (an
   unattended success and a failed run are written with their outcome
   and count 0; an external-data success, an eval run and a scenario run
   write no entry at all).
3. Counting a run that did not execute one definition. Caught by
   `probation_test::a_patched_run_is_not_eligible` (an attended run with
   a `patch_applied` that succeeds is written `eligible: false` and
   counts 0) and `probation_test::an_overridden_run_is_not_eligible` (an
   attended start with run overrides that succeeds is written
   `eligible: false`, and an entry forged eligible with its definition
   still counts 0 against the loaded `playbook.yaml`).
4. Counting from run directories, or counting a run twice. Caught by
   `probation_test::one_run_counts_once_across_a_prune`: two attended
   successes, `after_attended_successes: 2`; `apb_engine::retention`
   prunes the older run; the next unattended start passes and the count
   is 2 before and after the prune; a second terminal event for the
   same run (a resumed run that ends again) calls the observer again and
   leaves its single entry unchanged (written once).
5. Trusting a ledger that does not verify. Caught by
   `probation_test::an_unverifiable_ledger_refuses`: the ledger file with
   one byte changed: the unattended start is refused
   `probation_required` with the ledger text naming the path, no run
   directory is created, the run end of an attended run writes nothing
   to it and prints the stderr line, `probation::doctor_line` names the
   path, and a start with `RunOptions.waive_probation` passes. And by
   `probation::tests::a_ledger_is_written_only_by_the_terminal_observer`
   (the module's public functions are `install` and `doctor_line`, and
   neither writes; only `observe_ended` writes, and only the terminal
   observer calls it).
6. Checking a definition other than the one preparation loads (round 1
   finding 3). Caught by
   `probation_test::the_check_counts_the_version_the_start_loads`:
   1.0.0 has two attended successes and 1.1.0 is current with none, both
   asking for two; an unattended start of `--version 1.0.0` passes and
   one of the current version is refused, and no run directory is
   created for the refused start.
7. Exempting a child (round 1 finding 2). Caught by
   `probation_test::a_child_under_probation_refuses_the_parent_start`
   (the parent has finished its probation, its pinned child asks for one
   and has none: the unattended start is refused before anything runs,
   the detail names the child and its node, and no run directory of
   either exists), `probation_test::an_attended_parent_run_counts_for_its_child`
   (two attended successful parent runs write two eligible attended
   entries into the child's own ledger: the next unattended start
   passes) and `probation_test::a_waived_start_covers_its_children` (a
   start with `waive_probation` records `probation_waived` in its
   manifest and in its child run's, and the child run is created).
8. Checking readiness on a version other than the one loaded, or
   leaving out a child (round 2 blocker 4, major 3). Caught by
   `probation_test::require_ready_judges_the_loaded_version` (1.0.0 has
   a budget, the current 1.1.0 has none: `--version 1.0.0` with
   `require_ready` passes, the current version is refused `not_ready`
   and no run directory exists) and
   `probation_test::require_ready_refuses_an_unready_child` (a ready
   parent pins a child without a budget: refused `not_ready` naming the
   child and its node).
9. A waiver any channel can set, one asked only sometimes, or one
   claimed as proof of a person (round 1 finding 11, round 2 blocker 5).
   Caught by `probation_test::only_the_machine_file_or_a_waived_start_passes`
   (an engine start with `RunOptions.waive_probation` passes; MCP and
   HTTP starts are refused `probation_required`; a project
   `policy.yaml` with `waive_probation` does not read; the machine
   file's flag passes) and
   `people_cli_test::waive_probation_needs_a_typed_confirmation` (without
   a terminal exit 2 with the "needs a person at an interactive terminal"
   text, also for a playbook that asks for no probation; inside a run
   step, `APB_RUN_ID` set, exit 2 with the run step text; no run is
   created in any case).
10. Trusting self-reported or unenforced checks (round 1 finding 6).
    Caught by `readiness::tests::a_marker_is_not_a_deterministic_check`,
    `readiness::tests::an_output_item_check_is_not_proof`,
    `readiness::tests::a_script_item_check_counts_only_when_needs_look_fails_the_node`,
    `readiness::tests::a_goal_counts_only_when_enforced` (a `script`
    criterion without `enforce` and a `list` criterion with it count
    nothing).
11. Judging a connector by a local bound (round 1 finding 7). Caught by
    `readiness::tests::a_read_only_node_with_an_open_connector_is_unbounded`
    (`read_only: true`, a binding with `functions: all`: unbounded in
    that connector only), `readiness::tests::a_bounded_connector_names_functions_and_max_calls`,
    `readiness::tests::a_declared_external_effect_is_judged_in_the_network_domain`,
    `readiness::tests::a_read_only_script_is_not_bounded_externally`
    (round 3 major 7: a `read_only: true` script is bounded in its tree
    and listed in the network domain),
    `readiness::tests::an_isolated_agent_without_network_is_bounded_externally`
    (a stand-in `isolation` answering `network_closed: true` for it; the
    same agent with `tree: true` only is listed),
    `readiness::tests::a_prompted_finish_is_an_actor_without_a_local_boundary`
    (round 2 major 4), and in `fit/readiness.rs` tests
    `a_best_effort_node_without_a_sandbox_is_not_bounded` (a host facts
    stand-in without bubblewrap: `best_effort` falls back and the node is
    listed `tree`).
12. Missing failure routes, or failure routes that cannot occur (round
    1 finding 8, round 3 major 6). Caught by
    `readiness::tests::a_failure_before_the_gate_is_ungated` (`start ->
    work -> human_review -> script`, `defaults.on_failure` names an
    irreversible node: `work` can fail before the gate, so it is listed),
    `readiness::tests::a_failure_after_the_gate_is_gated` (`start ->
    human_review -> fork`, no `defaults.on_failure`, a `fail_fast` fork
    whose `on_failure` names an irreversible node: every failure that
    reaches it happens after the review, so it is not listed; round 1's
    seeding listed it. With `defaults.on_failure` the handler is listed,
    since the review itself can fail before it is passed, every kind but
    `start` failing in this walk: review fixes round 1, m3),
    `readiness::tests::a_gate_that_fails_is_not_passed` (`start ->
    human_review -> script` with an irreversible `defaults.on_failure`
    handler: listed),
    `readiness::tests::a_gate_a_model_may_decide_is_not_a_gate` (A8) and
    `readiness::tests::a_fail_fast_fork_routes_from_its_branches` (an
    irreversible `fork.on_failure` target is listed when the fork's
    branches start before any gate, and not when the fork sits behind
    one).
13. Warning on every playbook. Caught by
    `validate::readiness::tests::v250_only_with_an_unattended_block`.
14. Writing the entry before the terminal event, or with an outcome the
    journal does not hold (round 3, blocker 1). Caught by
    `probation_test::a_crash_before_run_finished_then_stop_is_never_eligible`:
    an attended run's journal holds every event of a succeeded run up to
    and not including `run_finished` (the events of a real run copied
    into a fresh run directory with no `driver.pid`, as a crash in the
    finish hooks leaves it); the ledger has no entry for it; `stop`
    appends `run_aborted` and the observer writes the run `aborted`,
    `eligible: false`; with `after_attended_successes: 1` the next
    unattended start is refused `probation_required` with `0 so far`.
    And by `probation_test::a_crash_after_run_finished_leaves_the_run_uncounted`:
    the same journal with `run_finished { outcome: succeeded }` written
    to the file directly (no append, so no observer runs, as a crash
    after the sync leaves it): the ledger has no entry, `stop` answers
    `AlreadyTerminal` and appends nothing, the ledger still has no
    entry, and the count is 0. And by
    `probation_test::the_outcome_comes_from_the_terminal_event`: an
    attended run whose node fails ends `run_finished { outcome: failed }`
    and is written `failed`, `eligible: false`.


## Journal corpus

`crates/apb-cli/tests/suite/journal_corpus_test.rs` gets row G1,
`g1_list_pilot_old_binaries_refuse_start_and_resume`, with its playbook
inline as rows D11 to D13 have theirs: a script list node over five
inline items with `pilot: { items: 2 }`. The oracle is fixed (round 2,
minor 2, corrected in review fixes round 1, M8): for each old binary
(`v0.25.0`, `v1.0.0-beta.1`), the row asserts both:

1. Start: `v1.0.0-beta.1`'s `apb run` of the playbook (installed in a
   fresh project) exits with a status other than 0, and no run directory
   is created under `.apb/runs` (`ListSpec` denies unknown fields, so the
   old parser rejects the definition before any run exists).
   `v0.25.0`'s `apb run` starts the playbook and its journal holds no
   `review_requested` and no `list_pilot`: it dropped the two fields and
   ran every item ungated, the standing contract of an older binary
   meeting a new playbook field.
2. Resume: the new binary runs the playbook to the pilot gate and its
   drive is stopped there by killing the driver (no `run_aborted`); the
   old binary's `apb resume <id>` exits with a status other than 0, and
   the journal is byte for byte what it was before the call (whether the
   old binary refuses on the definition or on the trailing `run_guard`
   naming `list_pilot`; `assert_run_guard_follows_last_checkpoint`, line
   3527, checks the guard's place first).

No other old-binary command is part of the row: an old `apb stop`
appends `run_aborted` by the standing contract (`read_all_for_stop`),
which the existing guard rows cover, and an old read is not asserted.
Then the new binary on a fresh copy of the run resumes, decides
`continue` (naming the request) and the run ends succeeded with five
items passed. The module doc gets one sentence for row G1.


## Release notes

The user-visible changes, for the release notes (the final report copies
them):

1. A list node with `pilot:` runs its first items, then stops at a gate on
   the list node (`continue`, `revise` with a note, `stop`); a list with
   `stop_when:` may end early, failed, with items left pending (counted
   after the pilot, never inside it). The pilot gate's pending block shows
   its slice, and its decision names the request it answers. An
   unattended run fails at a pilot gate unless the pilot declares
   `unattended: continue` and every slice item passed. apb 1.0.0-beta.1
   and later older releases reject a definition with `pilot` or
   `stop_when`; apb 0.25.0 drops the two fields and runs every item of
   the list with no pilot gate and no early stop, so keep every apb on a
   machine that runs such playbooks at this release or newer.
2. `policy.yaml` accepts `unattended:`; an older apb reads a file with it
   as invalid (it refuses launches through agent channels and creates no
   default notify subscription until updated), as for every earlier key
   of that file.
3. The fit report (`apb adopt`, MCP `playbook_adopt_report`, the dashboard
   fit card) says whether a playbook is ready to run alone: done defined
   by an enforced script goal criterion, every acting agent step checked
   by a script, every acting node bounded in its working tree and at each
   connector it binds and on the network (a script, or an agent without
   an isolation this machine enforces with `network: none`, is listed as
   not bounded externally; a `finish` with a prompt is listed as an actor
   without a local boundary), a budget, every irreversible step behind a
   gate a decision model cannot decide (along the failure routes a run
   can take), and its probation, counted in attended runs (started
   without `--unattended`, and not from a run step). `apb run
   --unattended` warns when it is not; with `--require-ready` the start
   is refused when the loaded version or a sub-playbook it starts is not
   ready.
4. With this apb or newer, a playbook with `unattended: {
   after_attended_successes: N }` (or a `policy.yaml` asking the same)
   refuses unattended starts until N attended successful runs of its
   exact definition (its `playbook.yaml` and its scripts), each run
   through one definition with no patch, rebind or override and not
   started from a run step, ended in the same project on this machine
   ("attended" means started without `--unattended`; it says nothing
   about who decided its gates); each sub-playbook it starts needs
   its own. They are counted from a new signed probation ledger per
   playbook and project (`.apb/probation/`), which every run end writes
   from this release on, once the run's terminal event is in its journal
   (a run whose process died before that entry was written stays
   uncounted): runs that ended before this release are not in it and do
   not count, so a playbook already proven by earlier runs
   needs N new attended runs, or a waiver. A ledger that does not verify
   refuses unattended starts until it is moved aside or the start is
   waived (`apb doctor` names it; the count then starts from zero); the
   ledger is bound to its project, so one copied or linked from another
   project does not verify. An unattended start that carries run
   overrides is refused `probation_required`, and so is one while a
   `policy.yaml` does not read, unless waived. A run step or a CLI
   without a terminal whose redirected config dir waives probation or
   asks for fewer attended runs than the default one is refused
   `policy_file_invalid`. A global playbook earns its
   probation per project. `apb run --waive-probation`, which always asks
   for the playbook id typed at an interactive terminal and covers every
   sub-playbook of the run, or `unattended.waive_probation` in the
   machine `policy.yaml`, waives it. A supervisor's patch of an
   unattended run is refused (`patch_rejected`, `probation_required` or
   `not_ready`) unless the patched version passes the same checks. An
   apb older than this release
   ignores the `unattended:` block and starts such a playbook unattended
   without probation: keep every apb on such a machine at this release
   or newer (`apb doctor` warns about others on `PATH`).
5. A run whose list node uses `pilot` or `stop_when` carries the run
   guard `list_pilot`; apb 1.0.0-beta.1 and later older releases cannot
   parse its definition, so they refuse to start such a playbook and to
   resume such a run and write nothing to its journal; apb 0.25.0 starts
   such a playbook without its pilot gate and early stop, and refuses to
   resume a run another apb started.
6. A decision on a gate that uses `max_scope`, `fields` or `show` names
   the request it answers (`apb review --request review:<seq>`, the
   `request` argument of MCP `review_decide` and of the HTTP route; the
   printed commands and next actions carry it). One that names none, or
   names a request the gate asked again since, is refused
   `decide_again`; before, it could answer the newer request. The request
   a decision names is the one it is stamped with, checked in the same
   journal read. While a run's definition snapshot does not read, a run
   using review forms refuses decisions the same way. A list pilot gate
   takes its decision the same way. Plain gates are unchanged.

## Files

1. apb-core (16): `list/mod.rs`, `schema.rs`, `lib.rs`, `readiness.rs`
   (new), `probation.rs` (new), `effects.rs`, `policy_file.rs`,
   `workspace.rs`, `validate/mod.rs`, `validate/guarded.rs`,
   `validate/pilot.rs` (new), `validate/readiness.rs` (new),
   `scenario/validate.rs`, `tests/suite/schema_test.rs`,
   `tests/suite/list_schema_test.rs`, `tests/suite/policy_file_test.rs`.
2. apb-engine (31, plus 6 carried over): `lib.rs`, `event.rs`,
   `state.rs`, `manifest.rs`, `run_config.rs`, `terminal_hook.rs`,
   `probation.rs` (new), `list/ledger.rs`, `list/view.rs`,
   `list/prompt.rs`, `scheduler.rs`, `scheduler/entry.rs`,
   `scheduler/prepare.rs`, `scheduler/review_gate.rs`, `scheduler/list/mod.rs`,
   `scheduler/list/batch.rs`, `scheduler/list/pilot.rs` (new),
   `review.rs`, `progress.rs`, `run_outcome/lists.rs`,
   `next_action/unattended.rs`, `scenario/capture.rs`, `fit/mod.rs`,
   `fit/report.rs`, `fit/readiness.rs` (new), `gate/mod.rs`,
   `gate/unattended.rs`, `tests/main.rs`,
   `tests/fixtures/journal/known_events.jsonl`,
   `tests/suite/list_pilot_test.rs` (new),
   `tests/suite/probation_test.rs` (new). Carried over from the review
   of #288: `scheduler/patch.rs`, `run_doctor/review.rs`,
   `tests/suite/review_scope_test.rs`, `tests/suite/passkey_gate_test.rs`,
   and, when their posts on a form gate name no request,
   `tests/suite/review_form_test.rs` and `tests/suite/loop_edges_test.rs`
   (`review.rs`, `scheduler.rs`, `scheduler/review_gate.rs` and
   `event.rs` are above).
3. apb-mcp (2, plus 2 carried over): `server/run.rs`,
   `tests/suite/run_tools_test.rs`; carried over: `server/args.rs`,
   `tests/suite/review_tool_test.rs`.
4. apb CLI (6): `main.rs`, `run.rs`, `run_items.rs`, `doctor.rs`,
   `tests/suite/people_cli_test.rs`, `tests/suite/journal_corpus_test.rs`.
5. apb-server (2, plus 1 carried over): `ts_contract.rs`,
   `tests/suite/launch_api_test.rs`; carried over:
   `tests/suite/runs_api_test.rs`.
6. web (8): `src/lib/api.gen.ts` (generated), `src/lib/ReviewPanel.svelte`,
   `src/lib/ListProgressPanel.svelte`, `src/lib/listprogress.ts`,
   `src/lib/listprogress.test.ts`, `src/lib/FitCard.svelte`,
   `src/lib/fit.ts`, `src/lib/fit.test.ts`.
7. Docs (2): `docs/HOWTO-authoring.md`, `docs/GUARDRAILS.md`.
8. Records (3): `docs/superpowers/specs/2026-11-DD-before-running-alone-design.md`
   (new, `DD` the day it is written), `CLAUDE.md`, `AGENTS.md`.
9. This change (6): `openspec/changes/before-running-alone/` (proposal,
   design, tasks, three spec files: the two new capabilities
   `list-pilot` and `unattended-readiness`, and the `review-scope`
   delta).

That is 83 to 85 files (section "Size"). Review round 2 moved the
reminders to wait-park (14 files: core `notify/precedence.rs`; engine
`notify/mod.rs`, `notify/start.rs`, `notify/transitions.rs`,
`notify/notice.rs`, `notify/view.rs`, `notify/send.rs`,
`connector/call/request.rs`, `connector/call/send_once.rs`,
`tests/suite/notify_remind_test.rs`; CLI `notify.rs`,
`tests/suite/notify_cli_test.rs`; `docs/NOTIFY.md`; the
`specs/wait-reminders/spec.md` delta), dropped the four retention files
(`retention/mod.rs`, `retention/archive.rs`, `run_stats/tests.rs`,
`health/facts.rs`), and added eight for the probation ledger
(`apb-core` `probation.rs` and `workspace.rs`; `apb-engine`
`probation.rs`, `lib.rs`, `run_end.rs`, `scheduler/finish_hooks.rs`,
`stop.rs`; the CLI `doctor.rs`). Review round 3 moved the ledger's
writer to the journal's terminal observer, which took `run_end.rs`,
`scheduler/finish_hooks.rs` and `stop.rs` out and put
`terminal_hook.rs` and the CLI's `main.rs` (the observer's install)
in, and swapped `graphutil.rs` (no longer changed: the gate walk lives
in `readiness.rs`) for `effects.rs` (`node_inferred` made public): one
file fewer. `policy_file.rs`,
`policy_file_test.rs` and `event.rs` stay for the `unattended:` section
and the two list events. The carried-over fix needs no change in
`crates/apb-server/src/routes/runs.rs` (409 already),
`crates/apb-mcp/src/tools/run.rs` (it passes `request` through),
`web/src/lib/rungates.ts` and `web/src/lib/runposts.ts` (the panel
already posts its next action's seq) or
`crates/apb-engine/src/next_action/command.rs` (every printed command
carries `--request`). Not in the list, because they need no change:
`crates/apb-engine/src/review/form.rs` (a pilot request passes
`GateDetail::default()`), `crates/apb-engine/src/list/mod.rs` (`PilotView`
is named by its path), `crates/apb-engine/src/report_html/sections.rs`
(it prints `ListOutcome::line` already), `crates/apb-engine/src/scheduler/node.rs`
(a child run already passes through `prepare_placed`),
`crates/apb-engine/src/gate/isolation.rs` (readiness calls
`node_contracts` as it is), `crates/apb-engine/src/retention/` (the
archive is untouched), `crates/apb-mcp/src/tools/run.rs` (the pilot gate
reaches `run_status` through `PendingReview`), `crates/apb-engine/src/run_end.rs`,
`scheduler/finish_hooks.rs` and `stop.rs` (the ledger is written by the
terminal observer; a stop's `run_aborted` reaches it through the
journal), `crates/apb-core/src/graphutil.rs` (the gate walk uses
`adjacency` as it is),
`crates/apb-cli/src/consent.rs` (`ask_typed` is used as it is),
`crates/apb-cli/tests/main.rs` (no new CLI test file) and
`crates/apb-cli/src/manage.rs` (`apb adopt` prints the report's JSON). A
file the compiler names beyond these (a literal construction of a changed
struct) is fixed where it is and counted in the final report.
