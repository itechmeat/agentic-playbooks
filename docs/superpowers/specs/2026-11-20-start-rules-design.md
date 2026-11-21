# Start rules

The public record of the change: part 1 the proposal, part 2 the design
as built.

---

# Proposal: start rules

## Why

This change is one of the 1.0 feature changes. It is read on main
efae5822, where everything it builds on is merged and archived:
`typed-inbox-reads` (#287), whose event filter is what a rule's `types`
and `where` are; `people-in-the-loop` (#288); `before-running-alone`
(#289), whose probation and readiness checks every unattended start of a
rule goes through in preparation; and `wait-park` (#290), which this
change leaves as it is. Two other changes are in flight, `eval-doubles`
and `inbox-waits`; this change needs nothing from either and may merge
before or after them (design "The in-flight changes this one must not
conflict with").

A routine today runs only when something outside apb starts it. An
inbound delivery never starts a run: the ingest listener stores a
verified delivery and answers 200 (`crates/apb-server/src/ingest.rs`
`post_hook_handler`, line 637, the append at line 789), and
`docs/CONNECTORS.md` line 454 says "An inbound delivery never starts a
run". A playbook can only poll the inbox from inside a run someone else
started, and `docs/CI.md` line 455 names an outside scheduler as "the
interim answer to scheduled and event-driven runs".

| Code example | Non-code example |
|---|---|
| a GitHub `issues` delivery with `action: opened` starts `issue-triage` pinned to 1.2.0, key `{{body.repository.full_name}}#{{body.issue.number}}`, at most 5 per hour | a CRM new-lead delivery starts `qualify-and-reply` with the lead id mapped to a param marked `untrusted: true` and the key `{{body.lead.id}}`, so later updates of one lead start no second run |

## What changes

Inbox start rules (capability `inbox-start-rules`):

1. A person writes, approves, enables, disables and removes a rule
   (`apb trigger` at an interactive terminal with the rule's name typed,
   or a signed-in dashboard session; MCP and run steps only list and
   test) into a signed store of its project (`.apb/triggers/`), bound to
   the registered project and read with no link followed; a rule is
   saved only in a registered project. Every rule needs an approval for
   its current digest to start anything; `enabled` changes only through
   `enable` and `disable`. Like main's launch floors and signed stores,
   an approval guards against mistakes and agents acting through apb's
   surfaces, not against another process of the same user that writes
   the signed file; binding an approval to a passkey is future work.
2. A rule names a connector and account, the event types and body
   conditions it accepts, a playbook pinned to a version, params mapped
   from body paths, a key template and a cap on starts per hour. Each
   creation of a rule, and each change of its connector or account, gets
   a random incarnation to which its state, journal and start keys are
   bound; a rule registers no inbox consumer and records a gap when
   retention removed events it had not seen.
3. The trigger sweep runs inside the ingest listener (`apb ingest`, or
   `apb dashboard` with `ingest.enabled`) every 5 seconds and at once
   after a stored delivery, and once in `apb resume --all` (never inside
   a run step); nothing new runs as a daemon. A pass walks each rule's
   new events in order: each match starts its run at once through the
   one start sequence every surface uses, or is recorded with its reason;
   only a busy or still preparing start key, the hourly cap (counted
   from the runs' own launch records) and a changed approval binding
   wait for the next pass. Nothing is queued or merged.
   `approve` and `enable` delete the rule's state, and the first pass
   after them positions the rule at the head it reads and starts nothing
   in that pass.
4. The start is made with the new `Initiator::Trigger`: never a person,
   decided like the CLI without a terminal, always unattended, a budget
   required, an `ask` launch policy and an irreversible consent only
   through a person's rule approval bound to the rule's digest and to
   one binding over the permit digest of its target, the launch policy
   in force and the billing decision, which every start compares once,
   in admission before the launch decision (for `ask` the equal approval
   is the confirmation; a billing `ask` is refused, a rule needs billing
   `allow`); a rule never starts a run under a binding other than
   the approved one: a start attempted under a changed binding, a
   stricter policy included, revokes the approval for good.
5. The rendered key is the run's start key and the only dedupe, so a
   resent delivery or a crash between a start and its record never
   starts twice; it lives `runs.start_key_days` like any start key. Runs
   at once are the playbook's own `concurrency:` block. A rule change
   waits for the start in progress at most, and no start of the old rule
   follows it.
6. A trigger journal per rule answers "why did it not start" in codes,
   condition indexes and pass or fail, never values; each line is synced
   before the position moves past its event. `apb trigger test` explains
   one stored event, runs the checks before preparation, names the ones
   checked only at start and writes nothing, read-only MCP included.
7. The run's launch record names the rule (`triggered_by`, optional) on
   every run surface. Otherwise a rule's run is an ordinary unattended
   run: it keeps its driver on a long wait, it never counts toward a
   playbook's probation, and an older apb drives it correctly without
   knowing it came from a rule. Validator codes V260 to V267 check a
   rule; event text enters only a param declared `untrusted: true`.

## Deferred to after 1.0

Time-based rules (a run started at a time) are not part of 1.0. Until
they come, a time-based start is an outside timer calling `apb run
<playbook> --unattended --start-key <slot> --detach`. The rule format
takes a time-based source later as a new variant of `source:` without
changing any rule written now (design "Deferred to after 1.0").

## Code that differs from the plan

1. The plan says a triggered run is "always unattended" and also that "a
   triggered run that reaches publish waits for a person". The code makes
   both impossible together: an unattended run fails a publish consent
   with `hitl_required` (`crates/apb-engine/src/scheduler/unattended.rs`
   lines 10 and 11). Decided: always unattended; a routine whose gates a
   person answers is not a rule's in 1.0.
2. The plan puts the rule store "under the config dir". Every store a
   start consults is per project (start keys, concurrency slots, health,
   all through `crates/apb-core/src/signed_store.rs`, `dir` line 127).
   Decided: rules live in the project they start runs in; only the inbox
   stays machine-scoped, and a rule reads it without registering a
   consumer there.
3. The plan's "own-playbook exclusion" has no provenance to work from: an
   inbox event does not say which run caused it. Decided: an event whose
   string at a body path the rule reads ends with the exact hidden send
   marker of a run this installation created
   (`crates/apb-core/src/connector/marker.rs`) is skipped `own_send`; a
   rule whose playbook can write to the same connector and account
   without a marker gets warning V265; the hourly cap bounds the rest.
   The step chain guard (`start_chain_too_deep`) does not apply: a rule
   start is not made from a run step, and no run step may write or
   approve a rule.
4. The plan treats a rule start as "not a person" but says nothing of
   probation. Decided: needs nothing of its own; a rule's run is
   unattended, attended credit counts only `unattended == false` entries
   (`crates/apb-core/src/probation.rs` lines 80 to 91), and children
   inherit the mode (`crates/apb-engine/src/scheduler/node.rs` lines 5013
   to 5015).

## Out of scope

1. Time-based rules: after 1.0 (above).
2. Chat-routed conversational agents, quiet hours, a webhook payload
   shape check: owner decisions in the brief.
3. A process or service of its own for the sweep: the ingest listener
   and `apb resume --all` host it.
4. Attended rule runs, a rule's run releasing its driver on a long wait,
   and a passkey-bound approval: future work (design "Cut in the tenth
   pass").
5. Standing publish consent for triggered runs: after 1.0.
6. The event filter, the stored event type, the github webhook block and
   the timer wait's park: merged with `typed-inbox-reads` and
   `wait-park`; used or left as they are, not redefined.
7. Waits that match inbox events (`wait_for: { type: inbox }`): the
   change `inbox-waits`.
8. Writing, approving, enabling, disabling or removing a rule from MCP
   or from a run step. A rule starts paid runs with nobody at the
   keyboard; it is a person's decision. MCP lists and tests.
9. Rules across projects or for sub-playbook children: a rule starts a
   top-level run in its own project.
10. The inbox's own size retention and the start key's lifetime
    (`runs.start_key_days`): both pre-existing on main, unchanged; a gap
    is journaled once and a key expired before a resent delivery starts
    a new run, both said in `docs/TRIGGERS.md`.
11. The mechanisms cut in the design passes (a rule's own concurrency
    cap, burst windows, held starts, a rule inbox consumer, run guard
    names, an outbox, a mark for older binaries, a passkey proof): see
    the design's "Cut in the ..." sections.

## Impact

1. Crates: `apb-core` (new `trigger/` with `rule.rs`, `state.rs`,
   `render.rs`, `validate.rs`, `evaluate.rs`; `connector::inbox`
   `head_seq`; `connector::marker::trailing_marker`;
   `launch::TriggeredBy`; `workspace::LOCAL_ENTRIES`;
   `start_key::key_tag_if_keyed` and `StartRequest::capture_if_keyed`;
   `fsutil::flock_held` and `fsutil::repair_line_tail`, shared with the
   review channel's tail repair in `apb-engine` `review.rs`),
   `apb-engine` (new `trigger/` with the sweep, the start count, the
   views, the run's rule and the listener heartbeat; `gate::launch`
   `Initiator::Trigger`, `LaunchContext.approved_binding`,
   `launch::approval_binding`; the binding compared in `gate/mod.rs`
   `Gate::admit`; `gate::billing::BILLING_TRIGGER`; `start_key::peek`;
   `report_html`), `apb-mcp` (`tools::trigger` with the fire every rule
   start goes through; tools `trigger_list`, `trigger_test`), `apb` CLI
   (`apb trigger`, `apb doctor`, `apb runs <id>`, `apb resume --all`),
   `apb-server` (the trigger sweep task in the listener, the ingest
   nudge, `/api/triggers` routes, run detail), `web` (Triggers page, the
   run's "started by rule" note). The scheduler, the manifest and the
   probation writer do not change.
2. Bundled content: none.
3. Journal and manifest: no new event type, no new event field, no run
   guard name and no manifest mark. The manifest's launch record gains
   `triggered_by` (additive, `serde(default)`, skipped when absent).
4. New state: `.apb/triggers/` (rules) and `.apb/trigger-state/`
   (state, trigger journals per incarnation, the sweep and change locks
   per rule) in the project (git-ignored, both signed files bound to the
   registered project), no inbox consumer,
   `<config_dir>/trigger-listener.json` (the sweep's heartbeat).
5. No new dependency.
6. Files: 71 (design.md "Code map").
7. Behaviour changes for release notes:
   1. A rule can start a run from an inbox delivery; nothing starts
      without a rule a person wrote and approved.
   2. A rule is changed and approved only by a person: at a terminal
      with its name typed or in a signed-in dashboard; only in a
      registered project.
   3. Each matching event starts its run at once or is recorded with
      its reason; the rule's key dedupes for `runs.start_key_days`.
   4. A rule's run is always unattended and keeps its driver on a long
      wait, like any unattended run.
   5. `apb doctor` warns when enabled rules exist and no sweep runs.

---

# Design: start rules

Part 1 is the proposal. Every decision below names its files, its
functions and the line where the code is today. Every file:line was read
on main efae5822 (`Wait park (#290)`), where `typed-inbox-reads` (#287),
`people-in-the-loop` (#288), `before-running-alone` (#289) and
`wait-park` (#290) are merged and archived. If a line moved by the time this is built,
find the site by the name given next to it, never by the number alone.

Two decisions, built in the order D0, D1. The names D2 to D4
are not used: D2 and D3 belonged to the earlier combined draft that is
now `typed-inbox-reads` and `wait-park`, and D4 was the time-based rule,
which is not part of 1.0 ("Deferred to after 1.0" below).

This is the twelfth pass of the design. Its sections "Cut in the third pass",
"Cut in the fourth pass", "Cut in the fifth pass", "Cut in the
eighth pass" and "Cut in the tenth pass" list what the reviews led to
remove and why.

Nothing in this document is left to the implementer. Where a choice was
open it is decided here with its reason.

## Deferred to after 1.0: time-based rules

A rule that starts a run at a time (a cron or a one-off instant) is not
part of 1.0; it comes after the 1.0 release. Until then a time-based
start is an outside timer (a systemd timer, cron, a CI schedule) calling
`apb run <playbook> --unattended --start-key <slot> --detach`, with a key
that names the slot (`daily-2026-11-23`), so a timer that fires twice
starts one run (`docs/CI.md` line 455 says so after this change).

The format of this change takes a time-based rule later without a
breaking change:

1. `RuleSource` is an externally tagged enum (`source: { inbox: {..} }`).
   A later `schedule` variant is a new tag; every inbox rule written now
   stays valid as written.
2. `TriggeredBy.source` is a string and its `connector`, `account` and
   `event_seq` are optional, so a slot field is one more optional field.
3. The trigger journal's `Verdict` gains values; its reader skips a line
   that does not parse (D1 "The state and the trigger journal").
4. A rule's start key is namespaced (`trigger:<incarnation>:...`), so a
   slot key gets its own prefix.
5. V268 to V279 stay reserved for this family.

An apb of this change that meets a rules file written by a later apb
with a time-based rule refuses it through the signed store's existing
newer-schema rule (`StoreError::NewerSchema`, `signed_store.rs` line
107): the later change bumps `RulesFile::SCHEMA`. That is the store's
standing downgrade refusal, not a format break.

## What this change builds on (merged, read on efae5822)

| Piece | Where |
|---|---|
| the event filter of typed-inbox-reads | `crates/apb-core/src/connector/event_filter.rs`: `EventFilter` line 29 (`conditions` line 33), `BodyCondition` 40, `ConditionResult` 49, `check` 68, `explain` 111, `matches` 137 |
| the stored event type | `InboxEvent.event_type`, `crates/apb-core/src/connector/inbox.rs` line 87; `WebhookSpec::names_event_type`, `connector/def.rs` line 319 |
| the typed append at ingest | `Inbox::append_typed` (`inbox.rs` 306), called by `post_hook_handler` (`crates/apb-server/src/ingest.rs` line 637) at line 789, whose `Ok(Appended::Stored(_)) \| Ok(Appended::Duplicate)` arm is line 790 |
| probation of an unattended start | `crates/apb-engine/src/gate/unattended.rs` `check_probation` line 263: one check in preparation (`crates/apb-engine/src/scheduler/prepare.rs` line 775, `opts.waive_probation`), never a surface's |
| the readiness demand | `RunOptions.require_ready` (`crates/apb-engine/src/scheduler/entry.rs` line 150), checked in preparation (`prepare.rs` line 791, `fit::readiness::start_check`) |
| the unattended start check | `apb_engine::gate::check_unattended` (re-export of `gate/unattended.rs` `check_start`, line 72; `gate/mod.rs` line 31) |

## What a rule start reuses, unchanged from main

A rule-started run is an ordinary start through the one start sequence
every surface follows (`crates/apb-mcp/src/server/run.rs`
`playbook_run_for`, line 374). This change adds no hand-over, control or
crash-recovery protocol of its own: the start key is the only dedupe,
and every check below is the engine's or the tools layer's as on main.

| Check | Where (efae5822) | What the rule gives it |
|---|---|---|
| start key, once per event | lookup `crates/apb-engine/src/start_key.rs` line 231, through `apb_mcp::tools::run::keyed_lookup` (`tools/run.rs` line 120); claim in preparation (`prepare.rs` line 1176) | the key `trigger:<incarnation>:<rendered key>`; never `reuse`; the only dedupe (I3) |
| unattended start | `gate::check_unattended` | `unattended: true` always (a rule's run is always unattended), mode autonomous, execution cli |
| the run gate | `gate::check_run` (`crates/apb-engine/src/gate/mod.rs` line 692): lifecycle, trust, requires, pins | `acknowledge_untrusted: false` always |
| launch policy | `gate::launch::decide` (`gate/launch.rs` line 863) | `Initiator::Trigger` (D0), the approval binding on `LaunchContext.approved_binding` (permit digest, launch policy in force, billing decision), compared in admission and the confirmation of an `ask` policy |
| billed use | `decide`'s billing half, `gate/billing.rs` | a rule never confirms: `billing_refused_for_trigger` (D0) |
| irreversible consent | `RunPermit::needs_consent` and `check_confirmation` (`gate/mod.rs` lines 616, 647) | the approval's consent nonce |
| health hold | `gate/mod.rs` line 738 and `prepare.rs` line 1157 (`playbook_on_hold`) | nothing; refused like any start |
| concurrency key | `concurrency::admit` (`prepare.rs` line 1252) | nothing: the playbook's `concurrency:` block is the one limit on runs at once; with `on_busy: refuse` the event's verdict is `refused` `concurrency_busy` (I2) |
| a busy working tree | `RunOptions.workdir_queue_wait` (`scheduler/entry.rs` line 85) | the wait the dashboard's start uses, `server.workdir_queue_wait_seconds` (`crates/apb-core/src/config.rs` line 1070, read at `crates/apb-server/src/routes/playbooks.rs` line 454): the run is admitted and waits for the tree in its drive (D1 "The fire" step 13) |
| probation | `prepare.rs` line 775 | `waive_probation: false` always; a rule's run is unattended, so it never counts as an attended success (D1 "Probation") |
| readiness | `prepare.rs` line 791 | `require_ready` from the rule (default true) |
| param patterns and values | `gate::check_params_with` (`gate/params.rs` line 41) | the rendered params |
| budget | `tools::budget::preflight` (`crates/apb-mcp/src/tools/budget.rs` line 157) | the rule's budget over the playbook's block (V263 requires one) |
| notify | `tools::notify::start_check` (`tools/notify.rs` line 220) | the rule's `notify` |
| external data | `apb_engine::external_data::for_surface_start` (`external_data.rs` line 196) | `external_data` from the rule (default true) |
| deadline, retention, follow-up, known problems, corrections | the engine, as for any run | nothing |
| a detached driver | `apb_engine::start_detached` (`scheduler/entry.rs` line 850) and `start_detached_resolved` (862) | always detached |

## Which process evaluates rules

Decided: the trigger sweep is a tokio task inside the process that runs
the ingest listener, `apb_server::ingest::run_ingest_server`
(`ingest.rs` line 464): `apb ingest`, or `apb dashboard` when
`ingest.enabled` is true, which co-starts that same listener
(`crates/apb-cli/src/serve.rs` `spawn_ingest_if_enabled`, line 71). It
runs every 5 seconds and at once after a stored delivery (an in-process
`Notify`). Besides it, `apb resume --all` (`crates/apb-cli/src/run.rs`
`resume_all_cmd`, line 2847) runs one synchronous pass over its project
right after its health sweep (line 2900). Nothing new runs as a daemon:
no new process, no new service unit, no new spawned child.

Why the listener and not the dashboard's health sweep
(`crates/apb-server/src/health_sweep.rs`, every ten minutes in `apb
dashboard`): only the listener stores inbox events (`append_typed` has
no other caller outside tests), so the sweep runs exactly where and when
events can arrive, a delivery wakes it in the same process with no
inter-process signal, and a dashboard without a listener has nothing to
sweep. Ten minutes is also too slow for an event start. Why `apb resume
--all` too: it is the existing "the machine is back" entry; one pass
there evaluates what arrived, or what the hourly cap or a crash left
unrecorded, even before a listener starts again. Two hosts running at
once never start one event twice (the per-rule sweep lock, then the
start key). Both sweep registered projects only: the listener the
entries of `projects::list_active()` (`crates/apb-core/src/projects.rs`
line 899), `apb resume --all` its own project when
`projects::registered_id` (line 834) names it; a rule is saved only in a
registered project (D1 "Where rules live" item 2). Neither runs inside a
run step: the listener is a person's process and never a run step, and
`apb resume --all` skips the rule sweep when
`apb_core::step_env::StepCaller::from_env()` (`crates/apb-core/src/step_env.rs`
line 17) names a run or a host task, before it reads any rule or event
and journaling nothing, because the start checks of a step's process
refuse what the listener would start (`gate::check_unattended` refuses
an unattended start from a step, `crates/apb-engine/src/gate/unattended.rs`
line 83; `external_data::for_surface_start` inherits the step's flag,
`external_data.rs` line 196), and every refusal is an event's verdict
(I2).

## Invariants

Five statements; every rule about rules, their state and their sweep
follows from them, and the sections below cite them by number.

1. I1, who changes a rule. Only a person changes a rule: `set`,
   `enable`, `disable`, `approve` and `remove` run at an interactive
   terminal with the rule's name typed, or in a signed-in dashboard
   session, and are refused from MCP and from a run step, main's rule for
   a person's command. A rule fires only while it is enabled and
   approved for its current rule digest and for the current approval
   binding (the permit digest of its target, the launch policy in force
   and the billing decision the gate resolves); any `set` that changes
   the rule digest drops the approval. A rule never starts a run under a
   binding other than the approved one: the binding is compared in
   exactly one place, admission of an actual start, and a start attempted
   under a changed binding (a stricter policy included) revokes the
   approval until a person approves again. An event the start key
   answers `already_started` attempts no start and revokes nothing, and a
   binding that changed and was restored with no start attempted in
   between leaves the approval valid, which is right, since every run
   still starts under the approved binding. MCP and a run step never write `rules.json`; the
   engine writes it only to revoke an approval whose binding changed (D1
   "Approval" item 4), never to give or change one. The boundary, once:
   like main's launch floors and its other signed stores, a rule approval
   guards against mistakes and against agents acting through apb's
   surfaces, not against another process of the same user that writes
   the signed file; binding an approval to a passkey is future work.
2. I2, the event is the unit, and its journal line comes first. A pass
   walks a rule's new events in seq order. For each it starts the run
   now through the ordinary start, or decides a verdict without a start.
   Its line is appended to the rule's journal with a stable id and
   synced, and only then does a state transaction move `seen_through`,
   the rule's only position, past the event. Nothing is queued, held for
   later or merged. Exactly three verdicts leave the position before the
   event and end the rule's pass, to be tried again next pass: a busy or
   still preparing start key (`start_key_busy`; a start key entry that is
   only pending, its owner alive and not bound to a started run, never
   consumes an event), the rule's own hourly cap, and a changed approval
   binding. A pass that ends early (a waiting change, the per-pass start
   bound, a journal write error, or a re-read showing the rule changed)
   also resumes before the event. Every other outcome is the event's
   verdict and moves the position past it: a skip, a start,
   `already_started` (a key bound to a started run only), a refusal, a
   failure. A crash between the line and the transaction
   repeats the event on the next pass; the start key answers
   `already_started` for a start, and the appender and the reader drop a
   line whose id is already there.
3. I3, the start key is the only dedupe. A start's key is
   `trigger:<incarnation>:<rendered key>`, the rendered key being the
   author's template (an event id for once per event, an entity id for
   once per entity). It dedupes for `runs.start_key_days` (default 7,
   `crates/apb-core/src/start_key.rs` line 25), exactly as any start key
   on main; a delivery of the same key after that starts a new run. The
   hourly cap counts the runs the incarnation started, read from their
   launch records, so no crash and no evicted event loses a start from
   it.
4. I4, one holder per rule, and a person waits for one start at most.
   Every pass and every mutation of a rule holds its sweep lock. A
   mutation first takes the rule's change lock, which a pass probes
   before every start and yields to. A pass reads the rule under the
   sweep lock and again right before each start, and starts only while
   the entry is enabled and approved with the incarnation and
   `rule_digest` it read first. So no start of the old rule follows a
   committed mutation, and a mutation waits for the start in progress at
   most.
5. I5, whose state it is. Each creation of a rule, and each change of
   its source (connector or account), gets a random, persistent
   incarnation; its state, its journal and its start keys are bound to
   it. The rule registers nothing in the inbox: it reads events above
   `seen_through` and records a gap when retention removed some it had
   not seen. A rule's position exists only in its signed state, and only
   a pass writes it: `approve` and `enable` delete the rule's state
   under the rule's locks and only then commit the rules change (so a
   crash between them leaves an unapproved or disabled rule without
   state, never an active rule with an old position), and the first pass that finds an approved, enabled rule with
   no state (or one of another incarnation) initialises the position at
   the head it reads then, journals one position line and starts nothing
   in that pass. Events that arrived between the command and that pass
   are skipped, the safe direction, at most one sweep period; a state
   removed by a person after it did not verify is initialised the same
   way, never at an older seq. Both signed files belong to one
   registered project and one installation and are read with no link
   followed. Only `remove`, `approve` and `enable`, under the rule's
   locks, delete a rule's state; only `remove` its journal; what a
   crashed `remove` leaves names no rule, decides nothing and is reported
   by `apb doctor`.

The cases, against them. "Next pass" is the next holder of the rule's
sweep lock.

| Case | Invariant | What happens |
|---|---|---|
| a person disables, edits, approves or removes a rule during a pass | I4 | the mutation holds the change lock; the pass ends after its current start and releases the sweep lock; the mutation commits; the next pass reads the new entry |
| two sweepers (the listener and `apb resume --all`) | I3, I4 | `try_hold_lock` skips a held rule; a pass reads the rule's state only after it holds the sweep lock, and `position_at_head` positions only while, inside its state transaction, the entry is still missing or of another incarnation; the start key covers the rest |
| crash after evaluating, before the journal line | I2 | nothing was recorded: the next pass evaluates the same events again from `seen_through` |
| crash inside the start, or after it and before its line | I2, I3 | the next pass evaluates the event again: a bound key answers `already_started` at once, before the hourly cap and the per-pass bound; a key still pending (its owner alive, not bound) holds the event `start_key_busy` with the position before it; a claim whose owner died is void and the event starts once (`start_key.rs` judges by owner, never by age) |
| crash after the synced line, before the state transaction | I2 | the next pass repeats the event; its line, if its id is the same, is not appended again but is synced (file and directory chain) before the state commit, and the reader drops a repeated id |
| the journal write or its sync fails | I2 | the state is not advanced; the pass ends for the rule and the event is tried again next pass |
| crash inside `approve` or `enable` after the state deletion, before the rules write | I5 | the rule is not yet approved or enabled and has no state; nothing sweeps it; the next `approve` or `enable` deletes the (absent) state again and commits |
| an approved, enabled rule with no state, or a state of another incarnation | I5 | the first pass initialises the position at the head it reads, journals one position line and starts nothing in that pass; events that arrived meanwhile are skipped, the safe direction |
| crash after a position line, before its transaction | I2 | the next pass writes another position attempt at the head it reads then; the signed state is the committed result, the lines are attempts, and readers show the last |
| a torn final journal record | I2 | repaired under the sweep lock before the next append: kept with its newline when it parses whole, cut to the last complete line otherwise |
| the rule's source is changed | I5 | the rule gets a new incarnation and loses its approval: the old state entry is inert until `approve` deletes it, and the old journal stays, inert, listed by `apb doctor` |
| a profile bundle, connector or account of the target, its launch policy (a raised floor, a narrowed `policy.yaml`) or its billing decision changes after the approval, and a start is attempted | I1 | admission resolves the binding before the launch decision; it differs from the approval's: no start, one `binding_changed` line synced, then the approval is cleared in `rules.json`; restoring the old target afterwards does not bring it back |
| the binding changes and is restored with no start attempted in between (no matching event, or only events the start key answers `already_started`) | I1 | nothing is revoked; the approval stays valid, since no run started under another binding |
| crash between the `binding_changed` line and the revocation | I1, I2 | the next pass compares again: still changed, the same line id (not appended twice) and the revocation completes; restored meanwhile, the approval stands, since none was revoked and the binding is the approved one |
| a run directory of the last hour with a manifest whose origin or rule cannot be established | I3 | it counts toward the hourly cap of every rule of the project; a stamped directory with no manifest does not count |
| a run directory whose journal (`events.jsonl`) is missing or whose metadata does not read | I3 | it stays in the window and is classified: attributed to the incarnation, it counts as a start inside the window; it is left out only when its records prove this incarnation did not start it |
| a start happened, then its line and its event were lost (a crash, then retention) | I3 | the hourly cap counts it from its launch record |
| `set` names `enabled` | I5 | refused `trigger_enabled_not_settable`; only `enable` and `disable` change it |
| remove, a crash before the state and journal are deleted | I5 | the leftovers name no rule and decide nothing; a rule of that name created later has another incarnation, and its `approve` deletes the old state entry (the journal of the old incarnation stays, inert); `apb doctor` lists the orphan files with their paths to delete |
| the hourly cap is reached | I2, I3 | asked only after the start key lookup said the event starts a new run; the event is not started, `seen_through` stays before it, one `held` line `rate_cap` (its id makes a repeat a no-op); it starts once the oldest counted start is an hour old |
| inbox retention removed events above `seen_through` | I5 | one `gap` line with the range; the pass continues from the oldest kept event |
| a rules or state file of another project or installation, or reached through a link | I5 | read as unsigned: nothing starts, every write refuses, `apb doctor` names it |
| a trigger journal that is a link, a hard link, not a regular file or over its cap, or a link at a component of its path | I2 | the pass evaluates nothing for that rule and starts nothing for it, every append refuses, so no position moves; the other rules go on, and the rule's view names the refusal with its remedy (`journal_error`) |
| a send marker whose `run` is not one safe segment, or names a link or a directory outside `.apb/runs` | I2 | not the rule's own send (`own_run`); the event is evaluated on |
| a run step or an MCP client tries to write a rule | I1 | refused; the environment check guards mistakes only (I1's boundary) |
| `apb resume --all` inside a run step | I2 | the rule sweep is skipped before any read: no line, no position moves |
| `apb trigger test`, MCP `trigger_test` | I2 | reads only: no lock, no state, no journal line, the start key peeked (D1 "`apb trigger test`") |
| an older apb meets a rule's run | (none) | an ordinary unattended run to it: it resumes, pauses and stops it as any unattended run; it ignores `triggered_by` (D0 "An older apb and a rule's run") |

How many runs of a rule's playbook run at once is the playbook's
`concurrency:` block, unchanged from main; a rule has no limit of its
own on it (Cut in the third pass, item 2).

## The in-flight changes this one must not conflict with

`eval-doubles` (V280 and V281) and
`inbox-waits` (V284 to V286, a
run-scoped inbox consumer and its acknowledgement) may merge before or
after this change. This change uses nothing either adds.

| Shared file | This change | inbox-waits | eval-doubles |
|---|---|---|---|
| `crates/apb-core/src/connector/inbox.rs` | `head_seq` | `read(.., after)`, `retire`, `tail_seq`, `cursor` made `pub` | none |
| `crates/apb-server/src/ingest.rs` | `trigger_sweep::nudge()` on a stored delivery, `trigger_sweep::spawn()` at listener start | `wake_waiters` on a stored delivery, `wake_all` at listener start | none |
| `crates/apb-mcp/src/tools/run.rs` | `run_status.triggered_by` | its own `run_status` field | none |
| `crates/apb-server/src/routes/runs.rs`, `ts_contract.rs`, `web/src/lib/api.gen.ts` | `RunDetail.triggered_by` and the trigger types | its own fields and types | none |
| `crates/apb-core/src/lib.rs`, `crates/apb-engine/src/lib.rs`, `crates/apb-cli/src/main.rs` | one `pub mod` or one command each | its own | `pub mod eval_doubles;` |
| `crates/apb-engine/tests/main.rs`, `crates/apb-cli/tests/main.rs` | one test module each | its own | its own |
| `docs/CONNECTORS.md` | the sentence at line 454 | lines 454 to 456 | none |
| `docs/GUARDRAILS.md` | "Launch policy for agents" and "Billed use" | its own section | the guard name `eval_doubles` |
| `CLAUDE.md`, `AGENTS.md` | the code map lines | its own | its own |

Who owns which symbol:

1. `Inbox::retire` is inbox-waits' alone: this change registers no
   inbox consumer (I5), so it neither adds nor calls `retire`, and the
   shared-symbol note of the third pass is withdrawn.
2. `Inbox::head_seq` is this change's. `head_seq` is
   a new public function that returns the private `last_seq` (line 650),
   which stays private and unchanged, as inbox-waits requires for its
   two callers under the lock.
3. `Inbox::read` and its `after` parameter, `tail_seq` and the public
   `cursor` are inbox-waits'. This change never calls `read`; the sweep
   reads with `read_events` (line 590).
4. `scheduler.rs`, `scheduler/deferred.rs` and `scheduler/wait_park.rs`
   are not touched by this change: a rule's run is an ordinary unattended
   run and keeps its driver on a long wait, as on main.
5. Consumers: this change adds none, so it cannot collide with
   inbox-waits' reserved `run_` names (`inbox_wait::is_wait_consumer`).
   A run-scoped wait consumer that acknowledges events lets age retention
   drop what every consumer acknowledged after 30 days (`inbox.rs` lines
   103 and 705 to 712); a rule that has not seen them by then records a
   gap (D1 "One pass over one rule" item 5).
6. Validator codes: this change V260 to V267, with V268 to V279 reserved
   for this family; eval-doubles V280 and V281 (V282 reserved and
   unused); `incremental-lists-and-session-fork` V283; inbox-waits V284
   to V286 (V287 to V299 reserved for that family).
7. `crates/apb-engine/src/deadline_hooks.rs`: both in-flight changes add
   hooks there; this change adds none (its race test holds the start at
   the existing `Hook::KeyAdmitted`).

## D0. Shared pieces

### Validator codes

The codes check a rule, not a playbook: a person writes a rule outside
the playbook, so an agent that edits a playbook can never add one. They
use `apb_core::validate::Issue` and `Severity`
(`crates/apb-core/src/validate/mod.rs` lines 93 and 87), so every
surface renders them like playbook issues. A rule with an error is never
saved; warnings are saved and shown.

| Code | Severity | Rule |
|---|---|---|
| V260 | error | the source does not resolve: unknown connector, a connector without a `webhook:` block (an MCP tool connector, which may not set one, included), unknown account, `types` on a connector whose webhook names no event type, a `where` condition `EventFilter::check` refuses, a template that does not parse, a name that is not `[a-z0-9][a-z0-9-]*` of at most 40 characters |
| V261 | error | the playbook does not resolve (unknown id, unknown version) or its lifecycle is `draft` or `retired` |
| V262 | error | a mapped param is not declared or is `secret: true`; a param that reads event text (`{{body.*}}` or `{{event.type}}`) is not `untrusted: true` (a `pattern` constrains the value further but never replaces it); a required param without default is not mapped; the playbook has a `secret: true` param without a default (a rule cannot fill it) |
| V263 | error | no budget: neither the rule's `budget` nor the playbook's `budget:` limits tokens or dollars, or the effective `on_no_estimate` is `ask` |
| V264 | error | a limit out of range: `max_per_hour` not 1 to 500 (the bound of the start count, D1 "One pass over one rule" item 6.2.5.1), an empty `key`, more than 32 params |
| V265 | warning | the playbook can write through the connector and account the rule listens on with a function whose `idempotency` is not `marker` |
| V266 | warning | `external_data: false`: what arrives from outside is kept for learning |
| V267 | warning | the effective `on_no_estimate` is `pause` while the rule's runs are external data (`external_data: true`, the default): an external data run never enters estimate history (`run_history::runs_of` over `external_data::learnable_run_dirs`, `crates/apb-engine/src/run_history.rs` lines 124 to 137), so a version with no learnable run pauses every rule run at its first gated node, and only a person resumes a budget pause |

Checked on efae5822: `git grep -ohE '"V2[0-9][0-9]"' crates | sort -u`
prints V200, V210, V211, V212, V220 to V222, V240 to V247, V249 and V250
only. The block V260 to V279 is the one eval-doubles' design reserves for
this change; V260 to V267 are used, V268 to V279 stay reserved. The codes
are `pub const`s in the new `crates/apb-core/src/trigger/validate.rs`
(`pub const V260: &str = "V260";` and so on), so no module under
`validate/` changes for the codes. `validate/guarded.rs` does not change:
a rule's run carries no mark for older binaries (D0 "An older apb and a
rule's run").

### New state

1. Project, git-ignored: `.apb/triggers/project/rules.json` (the rules,
   signed), `.apb/trigger-state/project/state.json` (what the sweep
   knows, signed), `.apb/trigger-state/log/<incarnation>.jsonl` (the
   trigger journal of one incarnation), and per rule name
   `.apb/trigger-state/<rule>.sweep.lock` and
   `.apb/trigger-state/<rule>.change.lock` (I4; OS locks a dead process
   releases). Both signed files carry the project's registered workspace
   id and are read with no link followed (I5).
   `crates/apb-core/src/workspace.rs` `LOCAL_ENTRIES` (line 27) gets
   `"triggers/"` and `"trigger-state/"` after `"runs-archive/"` (the
   last entry), each with a one-line comment in the style of
   `"start-keys/"` (line 55), and the test
   `the_enforcement_state_directories_are_local_entries` (line 329) names
   them.
2. Machine: `<config_dir>/trigger-listener.json` (D1 "The sweep host").
   No inbox consumer: a rule reads the account's events without
   registering in `cursors.yaml` (I5).

### Who starts the run: `Initiator::Trigger`

In `crates/apb-core/src/launch.rs`, after `StartedBy` (line 86):

```rust
/// The rule that started a run (start rules). Written once into the
/// launch record; never a person, never a run step. Additive.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggeredBy {
    /// The rule's name.
    pub rule: String,
    /// The rule's incarnation (D1 "Where rules live"): which creation of
    /// that name started the run.
    pub incarnation: String,
    /// What the rule listens to: "inbox".
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub connector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub account: Option<String>,
    /// The inbox seq of the event the run was started for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub event_seq: Option<u64>,
}
```

`LaunchRecord` (line 48) gets, after `started_by` (line 78):

```rust
/// The rule that started this run. Absent for every other start. Additive.
#[serde(default, skip_serializing_if = "Option::is_none")]
#[cfg_attr(feature = "ts", ts(optional))]
pub triggered_by: Option<TriggeredBy>,
```

Every struct literal of `LaunchRecord` gets `triggered_by: None` (find
them with `rg -n 'LaunchRecord \{' crates`; `launch.rs` line 262 and the
grant in `gate/launch.rs` `decide`, line 883, are two of them), and
`LaunchGrant::child` (line 437) too: a child records no rule. It needs
none: a child of a rule's run is unattended because its parent is
(`scheduler/node.rs` lines 5013 to 5015, `opts.unattended =
unattended::mode(run_dir, cfg)`), so it never earns attended credit
(D1 "Probation"), and the hourly count reads the rule's own runs only.
`triggered_by` is `serde(default)`: display and the hourly count are its
only readers.

In `crates/apb-engine/src/gate/launch.rs`:

1. `Initiator` (line 77) gets, after `Engine { by: EngineResume }`:
   ```rust
   /// A rule a person wrote starts the run (start rules): never a person,
   /// decided like the CLI without a terminal.
   Trigger { by: Box<apb_core::launch::TriggeredBy> },
   ```
   Every other exhaustive `match` on `Initiator` the compiler names
   (`echoes_on_its_channel` line 1003, `confirmation_nonce` line 1020,
   `started_by` line 842) treats `Trigger` as it treats
   `CliUnattested { terminal: false }`.
2. `label` (line 120): `Initiator::Trigger { by } => format!("trigger:{}", by.rule)`.
3. `checks_redirect` (line 144): `Initiator::Trigger { .. }` joins the
   `matches!`, so a redirected config dir that hides stricter settings
   refuses a rule start as it refuses a CLI without a terminal.
4. `is_agent_channel` (line 153) needs no change: everything but
   `Dashboard` and a terminal is an agent channel.
5. `LaunchContext` (line 277) gets one optional field, set only by a
   rule's start:
   ```rust
   /// The approval binding a person approved for this rule (start rules):
   /// `Gate::admit` compares it with the binding it resolves, before
   /// `launch::decide`, and an equal one is the confirmation an `ask`
   /// policy needs for `Initiator::Trigger`.
   pub approved_binding: Option<String>,
   ```
   with `approved_binding: None` in `LaunchContext::new` (line 291) and a
   builder `with_approved_binding(digest)`. No `LaunchConfirmation` variant
   is added: a rule stores no launch nonce.
   The binding is one digest, `gate::launch::approval_binding(subject, effective, billing) -> String`
   (new beside `nonce`, line 820): `apb_core::content::short_nonce("rule-approval", ..)`
   over `subject.permit_digest` (the playbook digest, profile bundles,
   connectors, connector accounts and children, `gate/mod.rs` line 604),
   `effective.policy` and its sources (the launch policy in force after
   the floor and `policy.yaml`), and the full billing decision,
   `billing.binding_lines()` (new in `crates/apb-engine/src/gate/billing.rs`
   beside `nonce_lines`, line 84). Not `nonce_lines`: it is empty unless
   the billing policy is `ask`, so a change between `allow` and `deny`
   would not move the binding. The canonical input, in this order, for
   every case (allow, ask and deny alike): the line
   `billing <billed> <unknown_basis>` (the effective settings), the line
   `required <policy>` (`required()`, the strictest policy any predicted
   entry falls under, line 65), each source of `effective.sources`
   sorted, and each predicted entry of every executor of the tree as
   `BillingEntry::label()` with the policy it falls under
   (`policy_for(basis)`), sorted; an empty prediction is the line
   `predicted none`. So a
   changed profile, connector or account, a stricter or looser launch
   policy (a raised floor, a narrowed `policy.yaml`) and a changed billing
   decision each change it.
6. Next to `ASK_POLICY` (line 463):
   `pub const TRIGGER_POLICY: &str = "launch_requires_rule_approval";`
   and `pub const BINDING_CHANGED: &str = "binding_changed";`.
7. `decide` (line 863), first statement after the `BILLING_DENIED` check
   (line 870):
   ```rust
   // A rule start never confirms billed use: nobody is there to ask, and
   // no single-use challenge is issued for it.
   if matches!(ctx.initiator, Initiator::Trigger { .. })
       && billing.required() == LaunchPolicy::Ask
   {
       return Err(billing_refusal(
           super::billing::BILLING_TRIGGER, ctx, subject, effective, billing, None,
       ));
   }
   ```
   and the `grant` closure's record gets
   `triggered_by: match &ctx.initiator { Initiator::Trigger { by } => Some((**by).clone()), _ => None }`.
   The binding check is not here but one step earlier, in
   `crates/apb-engine/src/gate/mod.rs` `Gate::admit` (line 884), after it
   resolved `effective` (`launch::effective_for`, line 899) and `billing`
   (`billing::check_tree`) and right before its call of `launch::decide`
   (line 924). For `Initiator::Trigger`: with `approved_binding: None` it
   returns `Err(json!({ "policy": TRIGGER_POLICY, "binding": <resolved>, "permit_digest": .., "launch_policy": .., "billing": .. }))`
   (an unapproved rule never reaches `decide`; `approval_need` reads what
   it shows from this refusal); with `Some(d)` not equal to the resolved
   binding (`ct_eq_str`) it returns
   `Err(json!({ "policy": BINDING_CHANGED, "binding": <resolved> }))`,
   which `launch::binding_changed_of(&Value) -> Option<String>` reads back
   as the typed `BindingChanged { binding }`. So the one comparison is made
   on the very permit, policy and billing decision that are then decided
   and handed to the engine.
8. `launch_confirmation` (line 1066) gets the arm
   ```rust
   // `Gate::admit` let this start through only with an approved binding
   // equal to the resolved one, which covers the policy in force: that is
   // what an `ask` policy needs.
   Initiator::Trigger { .. } => match &ctx.approved_binding {
       Some(_) if ask => confirmed_by("rule_approval"),
       _ if ask => refuse(TRIGGER_POLICY, false),
       _ => refuse(DENY_POLICY, false),
   },
   ```
   `deny` still denies. `allow` returns before this arm (line 1072). An
   approval given under `allow` never confirms a later `ask`: the policy
   is in the binding, so the stricter policy is a `BindingChanged` and
   the person approves again under it. The detail of `TRIGGER_POLICY`, in
   the `match policy` of `refusal` (line 1137):
   `"playbook `{id}` needs a person's approval of the rule that starts it (agent_launch `{policy}`: {sources}): apb trigger approve <rule>"`.
9. `billing_confirmation` (line 1035) gets the arm
   `(Initiator::Trigger { .. }, _) => Err(super::billing::BILLING_TRIGGER)`
   before the catch-all. On main a billing `ask` needs a person's
   confirmation of a single-use challenge (`gate::challenge`, consumed by
   `LaunchGrant::consume_challenge`); a rule start cannot give one, so it
   is refused `billing_refused_for_trigger` (item 7) before any challenge
   is written, and `docs/TRIGGERS.md` says a rule needs `allow` in
   `policy.yaml` `billing:` for the basis of its executors (`billed`,
   `unknown_basis`).
10. `crates/apb-engine/src/gate/billing.rs` gets, after `BILLING_ENGINE`
    (line 37): `pub const BILLING_TRIGGER: &str = "billing_refused_for_trigger";`,
    added to the `matches!` of `is_billing_refusal` (line 40), and its detail in the `match code` at
    line 158: `"a rule cannot confirm agent work that is billed or of unknown basis ({held}; {sources}); allow the basis in policy.yaml or start the playbook from a terminal"`.

Why a boxed struct: the initiator travels in every `LaunchContext`, and
the record keeps exactly what the start knew; a label alone would make the
record parse it back.

Wrong implementations:

1. Mapping `Initiator::Trigger` to `CliUnattested { terminal: false }`.
   The record then says `cli` and no surface can name the rule. Caught by
   `a_rule_start_records_its_rule` (`crates/apb-engine/tests/suite/trigger_test.rs`).
2. Letting an approved permit pass a `deny` launch. Caught by
   `a_deny_playbook_is_refused_even_with_an_approval` (unit test in
   `gate/launch.rs` `mod tests`).
3. Issuing a single-use challenge for a trigger's billing ask (the
   `offer()` path writes `.apb/launch-challenges`). Caught by
   `a_rule_billing_ask_writes_nothing` (unit test): the project tree is
   byte-equal before and after.
4. Comparing the approved permit anywhere but in `Gate::admit` before
   `launch::decide`, or on another permit than the one admitted. Caught
   by `the_permit_is_compared_in_admission` (unit, `gate/mod.rs`) and
   `a_changed_permit_revokes_the_approval`.

### An older apb and a rule's run

A rule's run carries no mark for older binaries. It needs none: it is
always unattended (D1 "The rule" has no `attended` field; "The fire"
step 5), it parks exactly as any unattended run does on main, which is
not at all (D1 "A rule's run keeps its driver"), and the only new datum
it holds is `LaunchRecord.triggered_by`, an optional `serde(default)`
field. To an apb that does not know the field the run is an ordinary
unattended run: it resumes, pauses and stops it as one, and it does so
correctly, since nothing about the run's behaviour depends on the field.
An older apb that rewrites the manifest (`manifest::amend`,
`crates/apb-engine/src/manifest.rs` line 860) may drop `triggered_by`;
the run then shows no rule on the surfaces, and the hourly count, which
cannot attribute it, counts it toward the cap of every rule of the
project while it has a manifest (D1 "One pass over one rule" item
6.2.5.1). A rule start whose preparation fails is handled exactly as main
handles any failed start (`prep_try_unstarted`, `scheduler/prepare.rs`
lines 160 to 176): its directory is a failed run, which no reader
mistakes for a rule's started run (a stamped directory with no manifest
does not count toward the cap). The manifest, `prepare.rs`,
`validate/guarded.rs` and the journal corpus do not change.

### A person's command

I1 in practice. Writing a rule (`set`), enabling, disabling, removing
and approving it are a person's, under main's existing rule for a
person's command; none takes a passkey.

1. CLI (`crates/apb-cli/src/trigger.rs`): every mutation starts with
   `if let Some(why) = apb_core::step_env::StepCaller::from_env().refusal("<what>")`
   (`crates/apb-core/src/step_env.rs` line 37), prints `why` on stderr and
   exits 2. Then it asks through `crate::consent::ask_typed`
   (`crates/apb-cli/src/consent.rs` line 136, the typed confirmation of a
   person-only gate in `apb review` and of a resume past a gate,
   `crates/apb-cli/src/run.rs` lines 3427 and 3478):
   `Type the rule's name to <set|approve|enable|disable|remove> it:`. No
   terminal (`None`) exits 2 with
   `a person changes a rule at an interactive terminal or in a signed-in dashboard`;
   another answer exits 2 with `not confirmed: the name typed is not `<rule>`; nothing was changed`.
   `--yes` is not accepted for any of them.
2. HTTP (`crates/apb-server/src/routes/triggers.rs`): every mutation
   needs `RequestChannel::Session` (`crates/apb-server/src/auth.rs` line
   688): a keyless dashboard and a bearer key are refused 403
   `trigger_requires_session`, a run step 409 (`StepCaller::from_env()`,
   as `routes/discover.rs` line 120). No route takes a passkey token. All
   these routes run their blocking work (the rule locks, which may wait
   up to 60 s each, `approval_need` and its `check_run`, the writes)
   inside `tokio::task::spawn_blocking`, as the sweep host runs
   `sweep_once`, so a waiting lock never holds a runtime thread the
   listener's sweep task shares in `apb dashboard`.
3. MCP has no write tool, and `trigger_list` and `trigger_test` never
   change a rule. `trigger_test` (MCP, CLI and HTTP alike) is refused
   from a run step by name, `trigger_test_refused_in_run_step`
   (`StepCaller::from_env()`): its dry start would run `check_unattended`
   and `external_data::for_surface_start` in the step's process, which
   refuse or flag what the listener would not (`gate/unattended.rs` line
   83, `external_data.rs` line 196), and refusing is simpler than
   evaluating as another process would.

The boundary, said once here and in `docs/TRIGGERS.md` (I1): like main's
launch floors and its other signed stores, a rule approval guards against
mistakes and against agents acting through apb's surfaces, not against
another process of the same user that writes the signed file (the
installation key it is signed with is readable by that user,
`crates/apb-core/src/run_origin.rs` lines 23 to 27), and the environment
and terminal checks guard a run step's mistakes, not a process that
clears `APB_RUN_ID` and `APB_HOST_TASK` and opens a pseudo terminal
(`step_env.rs` lines 1 to 7). Binding an approval to a passkey is future
work. No new credential is introduced.

### Lock order

1. The per-rule change lock (`<rule>.change.lock`): taken first by every
   mutation with `fsutil::hold_lock_waiting` (`crates/apb-core/src/fsutil.rs`
   line 823, 60 s, else `TriggerError::Busy`). A pass never takes it; it
   probes it with `fsutil::flock_held` before every start (I4).
2. The per-rule sweep lock (`<rule>.sweep.lock`): a pass takes it with
   `fsutil::try_hold_lock` (line 815) and skips the rule on
   `WouldBlock`; a mutation takes it after the change lock with
   `hold_lock_waiting` (60 s) and holds both through its rules write (and,
   for `remove`, the deletion of the rule's state and journal). A pass that sees the change lock held ends
   after its current start, so the mutation waits for that start at
   most.
3. Inside them, one `rules.json` transaction at a time, never nested in
   a state transaction or an inbox lock. A pass only reads `rules.json`
   (`signed_store::read`, line 148), checking `Loaded.signed`.
4. Inside them, the state transaction (`signed_store::transact` on
   `state.json`), only for reads and writes of the state file; never a
   start, never an inbox lock, never a journal append.
5. The inbox's own reads (`Inbox::head_seq`, `read_events`) only
   outside both transactions; a rule takes no inbox lock and writes
   nothing to the inbox.
6. The trigger journal append and its sync outside both transactions,
   under the sweep lock, always before the state transaction that moves
   `seen_through` past the event it records (I2).
7. A start runs with only the sweep lock held. Its own locks (start
   key, concurrency slot, working tree) are the engine's, as for any
   start.

### The MCP client's count of a closed session

The intermittent `apb-mcp-client` test that read the tap's dropped-line
count before the reader got to the last line is fixed in the client,
not in the test's timing: once the server is reaped, the close drains
the whole lines its reader never got to from a duplicate of the
server's stdout and counts them by the tap's own rule. The drain:

1. The duplicate descriptor is close-on-exec from its creation
   (`try_clone_to_owned`, never `dup`), so no process apb spawns while
   the session lives inherits another server's output.
2. Counts by the tap's rule: when the reader left a partial line,
   counting starts after the first newline; a last piece without a
   newline is never counted; a whole line over `MAX_LINE_BYTES` is not
   counted.
3. Never blocks the runtime: the descriptor is nonblocking, the drain's
   own 250 ms bound (taken out of the reap budget, so the documented
   close bound and `CLOSE_REPLY_WAIT` keep their distance) is checked on
   every read, successful ones included, and the runtime is yielded
   after every chunk, so a writer that keeps the pipe full cannot
   stretch the close.
4. Scans only the bytes just read for a newline; the carried piece is
   at most `MAX_LINE_BYTES + 1` bytes. A longer line cannot be counted:
   only it is dropped, up to the first newline that ends it, and what
   follows that newline in the same read is new lines.
5. Counts every byte it reads toward the session's byte limit before
   the read's lines are classified, the bytes of a dropped overlong line
   included (`LineTap::count_drained`), together with the total the
   reader recorded on the tap when it dropped (where it also records its
   partial line); once that sum passes `MAX_SESSION_BYTES` it counts no
   line more.

Its tests: a stub mode that writes a log line only at end of input,
after the reader is gone, so the test fails without the drain; a
server that dies in the middle of a line and logs one line at end of
input counts exactly one; the log line written right after an answer
while the reader lives keeps its own test; an overlong line, its
newline and a log line in one read count the log line; with the reader
near its limit, an overlong drained line takes the session past it and
the log line after it counts nothing (both through the drain itself,
over a file, on a paused clock); a quiet orphan and a flowing orphan
that hold the pipe after the server ends, where the close answers well
under two seconds, the test asserting that its orphan started (its
recorded pid, and the quiet one alive) before it judges the time and
killing it in teardown. A test that needs `/proc` (the descriptor
inheritance check) or the stub's `setsid` orphan is compiled for Linux
only (`cfg(target_os = "linux")`), so the release gate's macOS run never
runs a test that cannot hold or that has no orphan; `stdout_link()`
answers no link for an empty recording, so a missing recording fails by
name.

## D1. Inbox start rules

### The rule

New module `crates/apb-core/src/trigger/` (`pub mod trigger;` in
`crates/apb-core/src/lib.rs`), `mod.rs` re-exporting its files. The rule,
`trigger/rule.rs`:

```rust
/// The longest rule name; names are [a-z0-9][a-z0-9-]* like profiles.
pub const NAME_MAX: usize = 40;
pub const MAX_PARAMS: usize = 32;
/// A rendered param or key longer than this is refused.
pub const RENDERED_MAX_CHARS: usize = 4096;

/// A start rule as a person writes it (YAML at `apb trigger set`, JSON in
/// the store and on the HTTP API).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub name: String,
    pub playbook: RulePlaybook,
    pub source: RuleSource,
    /// Param name to template (`render.rs`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
    pub limits: RuleLimits,
    /// Over the playbook's `budget:` block per field, as a run argument.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<crate::budget::BudgetRequest>,
    /// The run's input is outside data (capability run-external-data).
    #[serde(default = "yes")]
    pub external_data: bool,
    /// Refuse a start whose readiness report does not pass (preparation
    /// checks it for an unattended start, which a rule's start always is).
    #[serde(default = "yes")]
    pub require_ready: bool,
    /// `None`: the machine's default notify subscription; `Some(false)`:
    /// off; `Some(true)`: the machine's target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulePlaybook {
    pub id: String,
    /// Always a version: a rule never follows `current` or a candidate.
    pub version: String,
    #[serde(default)]
    pub scope: RuleScope,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleScope { #[default] Project, Global }

/// What starts the rule (externally tagged: `source: { inbox: { .. } }`).
/// A later source kind is a new variant ("Deferred to after 1.0").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RuleSource {
    Inbox(InboxSource),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxSource {
    pub connector: String,
    pub account: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<String>,
    #[serde(default, rename = "where", skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<BodyCondition>,
    /// The template of the start key: one run per rendered key while the
    /// key lives (`runs.start_key_days`). An event id gives once per
    /// event, an entity id once per entity.
    pub key: String,
}

impl InboxSource {
    /// The filter of `types` and `where` (`EventFilter` of typed-inbox-reads).
    pub fn filter(&self) -> EventFilter;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleLimits {
    /// Starts in any 60 minutes (I2). Runs at once are the playbook's
    /// `concurrency:` block, not a rule limit.
    pub max_per_hour: u32,
}
```

`Rule` has no `attended`: a rule's run is always unattended (a rule
whose playbook needs a person at a gate fails there with
`hitl_required`, as any unattended run does). `Rule` has no `enabled`: whether a rule is enabled lives in its store
entry (`RuleEntry.enabled`) and changes only through `enable` and
`disable` (D1 "Where rules live" item 6). A rule document that names
`enabled` is refused before it is parsed as a `Rule` (`set_rule` reads it
as a YAML map first) with
`trigger_enabled_not_settable: a rule is enabled or disabled with apb trigger enable|disable, never through set`,
so a `set` can never switch a rule on and see what arrived while it was
off.

`InboxSource` names its fields instead of flattening `EventFilter`:
serde does not support `deny_unknown_fields` together with `flatten`.
The key below is per entity (one run per issue while the key lives); a
key of `{{body.delivery_id}}` would be per event.
The example rule:

```yaml
name: triage-new-issues
playbook: { id: issue-triage, version: 1.2.0 }
source:
  inbox:
    connector: github
    account: main
    types: [issues]
    where:
      - { path: action, equals: opened }
    key: "{{body.repository.full_name}}#{{body.issue.number}}"
params:
  repo: "{{body.repository.full_name}}"
  issue: "{{body.issue.number}}"
limits: { max_per_hour: 5 }
budget: { usd: 2, on_no_estimate: continue }
```

### Inbox additions for rules

`Inbox` (`crates/apb-core/src/connector/inbox.rs`) gets, after
`read_filtered` (line 462):

```rust
/// The highest stored seq, 0 for an empty log: the private `last_seq`
/// (line 650), which stays private and unchanged.
pub fn head_seq(&self) -> Result<u64, InboxError>;
```

Unit test in `inbox.rs`: `head_seq_is_zero_for_an_empty_log`. The sweep
and `apb trigger test` read events with `read_events` (line 590), which
registers nothing and takes no cursor; no lookup by seq is added.

A rule registers no inbox consumer. How retention treats that, read on
efae5822: `enforce_retention` (line 687) drops an event by age only when
it is acknowledged and past the window, where "acknowledged" is at or
below `min_cursor()` (line 705), the lowest cursor of the consumers
there are, and 0 when there is none (lines 624 to 627). So on an account
with no consumer no event is ever dropped by age; on one with consumers,
age retention drops only what all of them acknowledged and only after
`max_age_ms` (30 days by default, line 103). The size cap (line 720)
drops the oldest events whatever their ack state, and a consumer would
not prevent it either. So retention without a rule consumer cannot drop
an event before a 5 second sweep sees it, other than through the size
cap that a consumer does not stop: the preferred case holds, and a rule
reads events above `seen_through` and records a `gap` when retention
removed some it had not seen (D1 "One pass over one rule" item 5).

### Where rules live

1. `trigger/rule.rs` also defines the store file, one per project:

   ```rust
   #[derive(Debug, Clone, Serialize, Deserialize)]
   pub struct RulesFile {
       pub schema: u32,
       pub scope: String,   // "project"
       pub id: String,      // "rules"
       /// The registered workspace id of the project the file belongs to
       /// (I5), stamped on every write. The MAC covers it.
       #[serde(default)]
       pub project: String,
       #[serde(default)]
       pub rules: Vec<RuleEntry>,
       #[serde(default, skip_serializing_if = "Option::is_none")]
       pub mac: Option<String>,
   }
   #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
   pub struct RuleEntry {
       pub rule: Rule,
       /// 32 lowercase hex, `uuid::Uuid::new_v4().simple()` (uuid is already
       /// an apb-core dependency, `connector/marker.rs` line 135), given at
       /// creation and again at every change of the source (I5).
       pub incarnation: String,
       /// Only `enable` and `disable` change it (D1 "The rule").
       pub enabled: bool,
       #[serde(default, skip_serializing_if = "Option::is_none")]
       pub approval: Option<Approval>,
       pub created_at: u64,
       pub updated_at: u64,
       /// `terminal` or `dashboard_session` (the channel of I1).
       pub updated_by: String,
   }
   #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
   pub struct Approval {
       /// `rule_digest` of the rule it approved.
       pub rule_digest: String,
       /// The permit's playbook digest when it was approved.
       pub playbook_digest: String,
       /// The one approval binding (`gate::launch::approval_binding`, D0 "Who
       /// starts the run" item 5): the permit digest, the launch policy in
       /// force and the billing decision when approved. Admission compares
       /// it, and only it.
       pub binding: String,
       /// What the person saw, for the surfaces only: the permit digest and
       /// the launch policy. Never compared.
       pub permit_digest: String,
       pub launch_policy: String,
       #[serde(default, skip_serializing_if = "Option::is_none")]
       pub consent_nonce: Option<String>,
       pub approved_at: u64,
       pub approved_by: String,
   }
   /// SHA-256 of the canonical JSON (keys sorted) of
   /// `{ "incarnation": .., "rule": <the rule> }`.
   pub fn rule_digest(entry: &RuleEntry) -> String;
   ```

   `rule_digest` covers every field that changes whether or how the rule
   fires: the name, the playbook (id, version, scope), the source
   (connector, account, `types`, `where`, `key`), the params, the
   limits, the budget, `external_data`, `require_ready`,
   `notify`, and the incarnation, so an approval never carries over to
   a rule created again under the same name. `enabled` is not part of
   `Rule` (D1 "The rule"), so it is not in the digest: a rule approved,
   disabled and enabled again keeps its approval, and only `enable` and
   `disable` change it.

   `impl SignedDoc for RulesFile` (`crates/apb-core/src/signed_store.rs`
   line 27) with `MAC_DOMAIN = "apb-trigger-rules-v1"`, `SCHEMA = 1`,
   `DIR = "triggers"`, `ENTRIES_KEY = "rules"`, `NO_LINKS = true` and
   `belongs_to(root)` true exactly when
   `apb_core::projects::registered_id(root)` (`crates/apb-core/src/projects.rs`
   line 834) equals `project`. The store's read already treats a signed
   file that does not belong as unsigned (`signed_store.rs` line 211), so
   a file signed for another project, or reached through a link, is
   refused like a forged one (I5). The file is
   `signed_store::file::<RulesFile>(root, Scope::Project, "rules")` (line
   140).
2. A rule is saved only in a registered project. Every `set` that
   creates a rule first asks `projects::registered_id(root)`; `None` is
   `TriggerError::Unregistered`, printed and answered as
   `trigger_project_unregistered: this project is not in the projects registry, so no listener would sweep it; registration is off (APB_NO_REGISTRY=1, CI set, or registry: false in the global config): turn it on and run any apb command here (apb projects list registers it), then save the rule again`.
   Every apb command in a project with `.apb` registers it on its own
   (`crates/apb-cli/src/main.rs` line 1110, `projects::touch`) unless
   registration is off (`registration_disabled`, `projects.rs` line
   207), so the refusal appears only then. No fallback to the canonical
   root: the listener sweeps registered projects only (I5, "Which
   process evaluates rules"). A rules file of a project whose
   registration later went away reads as not belonging, so its rules
   start nothing and `apb doctor` names it.
3. Why per project (proposal "Code that differs" 2): the run, its start
   key, its concurrency slot and its health live in the project; the MAC
   binds the file to this installation and `project` to this project, so
   a checkout copied to another machine, or a rules file copied into
   another project of this machine, starts nothing there.
4. Every write is one `signed_store::transact(root, Scope::Project, "rules", Unsigned::Refuse, ..)`
   (line 267) that stamps `project`; a file that does not verify or does
   not belong refuses every write with
   `trigger_rules_unreadable: <path> does not verify (changed outside apb, written by another installation or for another project); move it aside to write rules again`
   (one source, `apb_core::trigger::rules_unverified_text` and
   `rules_unverified_detail`, code `RULES_UNVERIFIED`, for the CLI, the
   engine's list and doctor line, the HTTP refusal and the MCP refusal)
   and the sweep starts nothing in that project. Every read checks
   `Loaded.signed` explicitly (`signed_store::read` answers
   `Ok(Loaded { signed: false, .. })` for such a file, never an error,
   line 148): `false` is the same refusal. A save never writes a store
   its reader refuses: for every document read with no link followed
   (`NO_LINKS`), `signed_store`'s transaction measures the final signed
   bytes before the replacement and refuses the write when they exceed
   the reader's own limit (`NO_LINKS_CAP`, named once), leaving the stored
   file as it was (`StoreError::TooLarge`). The rules store reports it as
   `TriggerError::StoreFull`, `rules_store_full: the rules file would
   exceed <limit> bytes; remove a rule first` (exit 2 on the CLI, a
   refusal on MCP and HTTP), for a save, an update and an approval alike;
   a document of exactly the cap is written, one byte more is refused.
5. A rule has no inbox consumer (D1 "Inbox additions for rules"):
   nothing in `cursors.yaml` names it, so nothing has to be retired when
   it changes source, is disabled or removed.
6. Every mutation, in one order (I4): take the change lock, then the
   sweep lock; for `approve` and `enable`, delete the rule's state entry;
   one `rules.json` transaction; release. No mutation reads the inbox. What each changes:
   1. `set` of a new name: the project is registered (item 2), the
      document does not name `enabled` (D1 "The rule"); `incarnation`
      new, `enabled: true`, no approval, no state. Before its first
      approval the rule starts nothing.
   2. `set` of an existing name: `Unchanged` when `rule_digest` did not
      change (nothing written). Otherwise the rule is replaced and its
      `approval` dropped, so it starts nothing until it is approved again.
      When the connector or the account changed, the entry also gets a
      new `incarnation`, exactly as a creation: the old state, journal
      and start keys are the old incarnation's, inert, and the next
      `approve` replaces the state; no journal id spans two sources. Any
      other edit keeps the incarnation and `enabled`.
   3. `enable`: first `state::delete_entry` of the rule's state (one
      state transaction), then `enabled = true`; the next pass positions
      it at the head it reads, so a re-enabled rule sees only what
      arrives after (and up to one sweep period after, the safe direction).
   4. `disable`: `enabled = false`; nothing else.
   5. `approve`: first `state::delete_entry`, then the `approval` (D1
      "Approval"), so a rule acts only on what arrives once it is approved
      (I5). A crash between them leaves an unapproved rule without state,
      which nothing sweeps.
   6. `remove`: the entry deleted in one `rules.json` transaction; then,
      still under both locks, the rule's state entry deleted in one state
      transaction and its journal file deleted beneath `.apb` with no link
      followed. Once the rules transaction committed, the removal never
      answers an error: a state entry that cannot be deleted (a state
      file that does not verify, among others) is `state_left: <reason>`,
      a journal that cannot be removed beneath the anchor (a link at
      `log`, at the journal or at `.apb`) is `journal_left: <relative
      path> (<reason>)`, and both come back on the answer
      (`RemoveOutcome`), printed by the CLI as warnings, returned by MCP
      and HTTP and shown by the dashboard after a removal; no surface
      answers `trigger_rules_unreadable` for them. What a link pointed at
      stays as it is. A `state_left` from a state file that does not
      verify is not harmless: that file stops the project's sweep (D1
      "The state and the trigger journal") until it is removed, which is
      why the answer names it. A crash in between leaves
      a state entry or a journal file that no rule names: inert, since a
      pass acts only on rules `rules.json` names. A later `approve` of a
      rule of that name deletes the state entry by name, as every
      `approve` does, but no command or pass deletes an orphan journal:
      the doctor line lists it with its path to delete (D1 "Doctor"). No
      pass deletes them by name (Cut in the fourth pass, item 5).
   Among the mutations only `approve`, `enable` and `remove` touch the
   state, and only to delete the rule's entry; the position is written by
   the pass alone.
7. The writes, in `rule.rs`:

   ```rust
   #[derive(Debug, thiserror::Error)]
   pub enum TriggerError {
       #[error(transparent)]
       Store(#[from] StoreError),
       /// The rule has errors (every issue, errors and warnings).
       #[error("the rule has errors")]
       Invalid(Vec<Issue>),
       #[error("no rule `{0}`")]
       NotFound(String),
       #[error(transparent)]
       Inbox(#[from] InboxError),
       /// A rule lock stayed held for 60 s.
       #[error("rule `{0}` is being changed or swept; try again")]
       Busy(String),
       /// Item 2.
       #[error("trigger_project_unregistered")]
       Unregistered,
       /// The rule document names `enabled` (D1 "The rule").
       #[error("trigger_enabled_not_settable")]
       EnabledNotSettable,
       /// Item 4: a write the store's read cap refused.
       #[error("rules_store_full: the rules file would exceed {0} bytes; remove a rule first")]
       StoreFull(u64),
   }
   #[derive(Debug, Clone, PartialEq, Eq)]
   pub enum SetOutcome { Created, Updated { approval_dropped: bool }, Unchanged }
   /// Validates (`check_rule` over `RuleContext::load`), then creates or
   /// replaces the entry (item 6); the warnings come back with the outcome.
   /// `doc` is the YAML or JSON document as given, read as a map first so a
   /// document naming `enabled` is `EnabledNotSettable`.
   /// Every rule time is milliseconds from the one clock
   /// (`apb_core::clock`), passed as `Millis`, never a bare integer, so no
   /// caller can pass seconds; no stored field keeps seconds.
   pub fn set_rule(root: &Path, doc: &str, by: &str, now: Millis)
       -> Result<(SetOutcome, Vec<Issue>), TriggerError>;
   pub fn set_enabled(root: &Path, name: &str, enabled: bool, by: &str, now: Millis) -> Result<(), TriggerError>;
   pub fn store_approval(root: &Path, name: &str, approval: Approval, now: Millis) -> Result<(), TriggerError>;
   /// The entry, its state entry and its journal file (item 6.6).
   pub struct RemoveOutcome { pub removed: bool, pub state_left: Option<String>, pub journal_left: Option<String> }
   pub fn remove_rule(root: &Path, name: &str) -> Result<RemoveOutcome, TriggerError>;
   /// Both locks of the rule, waited for (60 s each); what the HTTP route
   /// and `approve` hold while they compare and write. The name is checked
   /// first, before any filesystem operation.
   pub fn hold_rule_lock(root: &Path, name: &str) -> Result<RuleLock, TriggerError>;
   /// The rule name pattern, one safe path segment; checked at the lock
   /// and store boundary (`hold_rule_lock`, and the sweep before its lock)
   /// before any filesystem operation. A name that is not one is
   /// `NotFound`.
   pub fn checked_rule_name(name: &str) -> Result<&str, TriggerError>;
   /// The one rule document parser, public: the store, the CLI and the
   /// docs example test read a rule document through it.
   pub fn parse_rule_document(doc: &str) -> Result<Rule, TriggerError>;
   ```

   Each of the four takes the locks itself; a caller that already holds
   them passes them through a `_locked` variant of the same function
   taking `&RuleLock`, so a lock is never taken twice. None of them
   checks who calls: the CLI and HTTP layers do (D0 "A person's
   command").

### Validation

`trigger/validate.rs`:

```rust
/// What validation reads beyond the rule.
pub struct RuleContext {
    /// The pinned playbook, `None` when it does not resolve.
    pub playbook: Option<Playbook>,
    /// Its lifecycle, `apb_core::trust::read_lifecycle` of the resolved
    /// playbook directory (`crates/apb-core/src/trust.rs` line 79), `None`
    /// when it does not resolve.
    pub lifecycle: Option<Lifecycle>,
    /// The connector document, `None` when it is not installed. Read
    /// through `apb_core::connector::store::read_doc_contained` (no tree
    /// digest, symlink containment kept), the one read a rule's checks and
    /// ingest's delivery path share.
    pub connector: Option<ConnectorDoc>,
    /// The connector's configured account names.
    pub accounts: Vec<String>,
    /// The connector facts of the playbook's bindings, for V265.
    pub facts: BTreeMap<String, ConnectorFacts>,
}
impl RuleContext {
    /// Loads it for `rule` in the project at `root`, reading nothing else.
    pub fn load(root: &Path, rule: &Rule) -> RuleContext;
}
/// Every issue of `rule`, errors first, each with its code.
pub fn check_rule(rule: &Rule, cx: &RuleContext) -> Vec<Issue>;
```

`load` resolves the playbook with `apb_core::store::resolve`
(`crates/apb-core/src/store.rs` line 52) and parses it, loads the
connector and its accounts as `ingest.rs` `resolve_target` (line 817)
does, and the facts through `connector::resolve::validation_facts()`
(`connector/resolve.rs` line 504). The checks:

1. V260: the name; the connector is installed and has a `webhook:`
   block; the account is configured; `types` is empty or the webhook
   `names_event_type()`; the filter passes `EventFilter::check`; every
   template parses (`render::parse`).
2. V261: `cx.playbook` is `None`:
   `"playbook `{id}` version {version} does not resolve in this project"`;
   or `cx.lifecycle` is `Draft` or `Retired` (`Lifecycle`, `trust.rs`
   line 28): `"playbook `{id}` is {draft|retired}; a rule starts only an active playbook"`.
   `store::resolve` (`store.rs` line 52) does not read the lifecycle, and
   the run gate checks it on its own (`gate/mod.rs` `check_lifecycle`,
   line 1608, refusing `draft_requires_trial` and
   `retired_not_runnable`); `load` reads it so a rule is refused when it
   is saved, not only at every start.
3. V262, per param: not in `playbook.params`; `secret`; a template that
   reads event text into a param that is not `untrusted: true` (`Param`,
   `crates/apb-core/src/schema.rs` line 901: fields `secret` 915,
   `pattern` 928, `untrusted` 933); once per playbook param without
   `default` that is not `secret` and not mapped; and once per playbook
   param with `secret: true` and no `default`, mapped or not (mapping a
   secret param stays its own error):
   `"param `x` is secret and has no default: a rule cannot fill it, so no event can start this playbook"`.
   A `pattern` never
   stands in for `untrusted`: the anchored matcher accepts every value of
   a pattern such as `.*` (`crates/apb-core/src/param_pattern.rs` line
   23), while only the `untrusted` declaration makes the run treat the
   value as untrusted text (`crates/apb-core/src/untrusted.rs` line 5).
   A pattern on an untrusted param is an additional value constraint,
   checked at every start by `gate::check_params_with`:
   `"param `{name}` takes text from the event ({template}); declare it untrusted: true (a pattern only narrows the value)"`.
4. V263: `apb_core::budget::effective(playbook.budget.as_ref(), rule.budget.as_ref())`
   (`crates/apb-core/src/budget.rs` line 125) is `None`, or its
   `on_no_estimate` is `Ask`:
   `"a rule needs a budget with a token or dollar limit and on_no_estimate pause or continue"`.
5. V264: the limits of `rule.rs`; `key` empty; `params` over `MAX_PARAMS`.
6. V265 (warning): for every site of `playbook.connector_sites()`
   (`crates/apb-core/src/connector/sites.rs` line 44) binding the rule's
   connector with `accounts` absent or naming the rule's account, a
   granted function that is not `read_only` and whose idempotency in
   `cx.facts` is not `marker`:
   `"node `{site}` can write through {connector}/{account} without a send marker: its own writes come back as new events; the hourly cap bounds a loop"`.
7. V266 (warning): `external_data: false`.
8. V267 (warning): the effective `on_no_estimate` (`budget::effective`)
   is `pause` and `external_data` is true: `"this rule's runs are external data and never build estimate history, so on_no_estimate pause holds each run at its first gated node until a person resumes it; use continue, or a limit the playbook can meet without an estimate"`.

`apb trigger set` and the HTTP write refuse a rule with an error and
print every issue; the sweep checks V260 to V264 again before each
start (the playbook or the connector may have changed) and records the
event `refused` with the codes (I2: a refusal is the event's verdict).

### Templates

`trigger/render.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part { Text(String), Body(String), EventType, EventSeq, ReceivedAt }

/// Parses `text {{body.a.b}} {{event.type}} {{event.seq}} {{event.received_at}}`.
/// Any other placeholder, an empty path or an unclosed `{{` is an error.
pub fn parse(template: &str) -> Result<Vec<Part>, String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError { Missing(String), NotScalar(String), TooLong }

/// Renders against one stored event: a body path through
/// `connector::common::value_at_path` (line 117), which tells the two
/// reasons apart: `None` is `Missing`; `Some` of a non-empty string, a
/// number or a bool renders as `scalar_at_path` (line 135) would; `Some`
/// of an empty string, null, an object or an array is `NotScalar`.
/// `scalar_at_path` alone is not used: it answers `None` for a missing
/// path and a non-scalar alike. `event.type` the stored type (`Missing`
/// when untyped), `event.seq` the seq, `event.received_at` RFC 3339 UTC
/// (`clock::iso_utc`, `clock.rs` line 71). Longer than
/// `RENDERED_MAX_CHARS` is `TooLong`.
pub fn render(parts: &[Part], event: &InboxEvent) -> Result<String, RenderError>;

/// Whether the template reads event text (a body path or the type).
pub fn reads_event_text(parts: &[Part]) -> bool;
```

The start key of a match is
`format!("trigger:{incarnation}:{rendered key}")` (I3), checked with
`StartKey::parse` (`crates/apb-core/src/start_key.rs` line 45: 1 to 200
characters, no hidden character, not credential-shaped; the prefix takes
41 of them); a key that does not parse skips the event `key_invalid`
(the reason never repeats the key). Neither the state nor the journal
keeps the key; only `apb trigger test` shows its tag
(`start_key::key_tag_if_keyed`, D1 "`apb trigger test`").

### The state and the trigger journal

`trigger/state.rs` holds the signed state and the journal:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerStateFile {
    pub schema: u32,
    pub scope: String,   // "project"
    pub id: String,      // "state"
    /// As `RulesFile.project` (I5).
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub rules: Vec<RuleState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceId { pub connector: String, pub account: String }
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RuleState {
    pub rule: String,
    /// The incarnation this state belongs to (I5).
    pub incarnation: String,
    /// The source the position belongs to (one per incarnation, I5).
    pub source: SourceId,
    /// The last inbox seq whose verdict line is in the journal (I2).
    pub seen_through: u64,
}
/// The most events one pass of one rule evaluates, and the most starts it
/// makes (a fairness bound between rules; the next pass is 5 s away).
pub const MAX_EVENTS_PER_PASS: usize = 500;
pub const MAX_STARTS_PER_PASS: usize = 10;

/// Positions the rule's state at `head` (I5), by the pass only, when the
/// state of an approved, enabled rule is missing or of another
/// incarnation; the pass starts nothing more for the rule in that pass.
/// Line first (I2): the `positioned` line (id
/// `<incarnation>:positioned:<head>`) is appended and synced, then one
/// state transaction compares and sets: only while the rule's entry is
/// still missing or of another incarnation inside that transaction does it
/// set `{ incarnation, source, seen_through: head }`, replacing an entry of
/// another incarnation; it returns whether it positioned. It removes no file: the
/// journal of another incarnation is inert, listed by the doctor line with
/// its path to delete, and only `remove` deletes the files of the rule it
/// removes. The line is an attempt record and the signed state is the
/// committed result: after a crash between them the next pass reads a head
/// that may be newer and writes another attempt; readers show the last.
pub fn position_at_head(root: &Path, entry: &RuleEntry, head: u64, now: u64) -> Result<bool, TriggerError>;
/// Whether the state needs `position_at_head` before a pass: no entry, or
/// one of another incarnation. Read only, on the state the pass read after
/// it holds the rule's sweep lock; `position_at_head` decides again inside
/// its transaction.
pub fn needs_position(file: &TriggerStateFile, entry: &RuleEntry) -> bool;
/// Inside a state transaction: removes the rule's entry (what `approve`,
/// `enable` and `remove` do); absent is a no-op.
pub fn delete_entry(file: &mut TriggerStateFile, rule: &str);
```

`impl SignedDoc for TriggerStateFile`: `MAC_DOMAIN = "apb-trigger-state-v1"`,
`SCHEMA = 1`, `DIR = "trigger-state"`, `ENTRIES_KEY = "rules"`,
`NO_LINKS = true`, `belongs_to` as `RulesFile`'s (I5); always
`(Scope::Project, "state")`; every write stamps `project` and goes
through `transact(.., Unsigned::Refuse, ..)`. A state file that does not
verify or does not belong stops the sweep of that project (nothing
starts; a forged file can only stop starts), and `apb doctor` names it
with the recovery: remove the unreadable file; every approved rule then
continues from the current head of its source (the pass positions a
missing state there), skipping what arrived in between and starting no
old event again. A missing file reads as fresh and valid
(`signed_store::read`, `crates/apb-core/src/signed_store.rs` lines 154
to 160), which is why a missing state means the head, never an older
seq.

The trigger journal is `.apb/trigger-state/log/<incarnation>.jsonl`
(0600). Its lines hold codes and safe structured metadata only, from a
closed set of fields; no field holds an engine sentence, a body value or
a param value:

```rust
pub const LOG_KEEP: usize = 1000;
pub const LOG_REWRITE_AT: usize = 1200;

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogEntry {
    /// The stable line id: `<incarnation>:<seq>:<verdict>` for an event's
    /// line (`held` lines add `:<code>`, a `binding_changed` line
    /// `:<binding>` after its verdict), `<incarnation>:gap:<a>-<b>` and
    /// `<incarnation>:positioned:<head>` otherwise. One incarnation reads
    /// one source (a source change gives a new incarnation), so a seq names
    /// one event of one account. A line with an id the
    /// file holds already is never appended again.
    pub id: String,
    /// Milliseconds since the Unix epoch.
    pub at: u64,
    pub verdict: Verdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub seq: Option<u64>,
    /// A gap's range, or a `positioned` line's head twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub range: Option<(u64, u64)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub run_id: Option<String>,
    /// The reason as a code, never a sentence: a skip reason of D1
    /// "Evaluating one event" (`type`, `where`, `own_send`, `key_missing`,
    /// `key_not_scalar`, `key_too_long`, `key_invalid`, `param_missing`,
    /// `param_not_scalar`, `param_too_long`), `rate_cap`, `rate_cap_unknown`, `start_key_busy`,
    /// `start_key_conflict`, `check_rule`, or a refusal's `policy`.
    /// Checked against `[a-z0-9_]{1,64}` by the one code-pattern check
    /// `apb_core::trigger::is_code_text`; anything else is stored as
    /// `refused_other`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub code: Option<String>,
    /// The name the code is about, taken from the rule itself, never from
    /// the event: a param name (`param_missing`, `param_not_scalar`,
    /// `param_too_long`, `param_pattern_mismatch`) or the template's body
    /// path (`key_missing`, `key_not_scalar`). Checked as `code` is, plus `.`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub name: Option<String>,
    /// A `binding_changed` line's digests: the one resolved and the one the
    /// approval held (digests, no value).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub binding: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub approved: Option<String>,
    /// Validator codes (`V260` to `V266`) of a `check_rule` refusal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub codes: Vec<String>,
    /// Per condition of the filter, in the rule's order (`types` first,
    /// then each `where` entry): its index and whether it passed. Never the
    /// condition's literal nor the event's value: `ConditionResult` holds
    /// the rule's `equals` literal, the event's type and the value at the
    /// condition's path (`crates/apb-core/src/connector/event_filter.rs`
    /// lines 114 to 125),
    /// so it appears only in the dry test's response.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<ConditionMark>,
}
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionMark { pub index: u32, pub passed: bool }
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict { Skipped, Held, Started, AlreadyStarted, Refused, Failed, Gap, Positioned }

/// Why a journal was not read, typed: the journal itself (too large, a
/// link, a hard link, not a regular file), a component of the path that
/// reaches it (a link at `.apb`, at `trigger-state` or at `log`, named),
/// or another read failure. The words of a refusal are produced once, from
/// this type; no code splits another function's message.
pub enum JournalRefusal { Journal(ReadRefusal), Component(String), Io(String) }
/// The one sentence a refused journal is named with on every surface: the
/// problem once, then the remedy. For the journal itself:
/// `journal_refused: the trigger journal of `<rule>` cannot be read
/// (<reason>); remove the rule and save it again, or delete
/// .apb/trigger-state/log/<incarnation>.jsonl`. For a component: the same
/// opening with `(<component> is a link)` and the remedy `replace the link
/// at <component> with a directory`, never a save or a path through the
/// link.
pub fn journal_refused_text(rule: &str, incarnation: &str, refusal: &JournalRefusal) -> String;
/// The ids the journal file holds, read once by a pass under the sweep
/// lock (the file only grows under it), through `fsutil::beneath` over
/// `.apb` (no link followed at any component, a regular file with one
/// link only) with the read cap `JOURNAL_READ_MAX` (16 MiB). A journal
/// that is refused is the error, never an empty set. This is where the
/// rewrite to the newest `LOG_KEEP` lines happens, when the file holds
/// more than `LOG_REWRITE_AT`: one rename beneath `.apb`
/// (`Beneath::write_new`), then the sync of `log/` through the walk's own
/// descriptor (`Beneath::sync_dir`), so the ids a pass relies on are
/// durable with the file that holds them, whatever a later append writes.
/// An id the rewrite dropped belongs to an event at or below
/// `seen_through`, which no pass evaluates again.
pub fn journal_ids(root: &Path, incarnation: &str) -> Result<HashSet<String>, JournalRefusal>;
/// Under the rule's sweep lock: opens the file beneath `.apb` with no link
/// followed (`Beneath::open_read_append`: one descriptor, read and append,
/// created 0600 when missing, a link, a hard link or a non-regular entry
/// refused), first repairs its tail, then appends the lines whose id is not
/// in `ids` (adding them), then syncs the file, then `log/` through the
/// descriptor of the directory the open walked (`OpenedAppend::sync_dir`:
/// a directory swapped in at `log` after the open is never synced in its
/// place), then `.apb/trigger-state` and `.apb` through their own walks
/// (`Beneath::sync_dir`, where a link swapped in meanwhile is refused).
/// Whether the file already existed, whether a tail needed repair and
/// whether every line id was new decide nothing about these syncs:
/// presence and a matching id prove that a line is there, never that it
/// is durable (I2). An error, the repair's and every sync's included, is
/// returned before anything else is done, so the caller does not move the
/// position. The repair is `apb_core::fsutil::repair_line_tail_file(file,
/// cap, whole)` with `whole` the parse of a `LogEntry`, reading through the
/// opened descriptor with the same `JOURNAL_READ_MAX` and refusing above
/// it: a final record without its newline that parses whole gets the
/// newline, a partial one is cut to the last complete line. That is the
/// review channel's `repair_tail` (`crates/apb-engine/src/review.rs` line
/// 1307, run under the reviews lock at line 1342) moved to `fsutil` with
/// the parse as a parameter, and `review.rs` calls it with the parse of a
/// `ReviewEntry`, so both appenders share one repair. Off unix the open is
/// a checked path walk without the descriptor guarantees (a link swapped in
/// between the check and the open would be followed), it hands back the
/// same `OpenedAppend`, whose directory sync answers `Ok` as
/// `fsutil::sync_dir` does there, and its read names a link at the last
/// component `Link` and one at a parent `LinkAt`, as the unix walk does.
pub fn append(root: &Path, incarnation: &str, ids: &mut HashSet<String>, lines: &[LogEntry]) -> io::Result<()>;
/// The newest `limit` entries, newest last. This is the reader that
/// dedupes: a line whose id appeared earlier is dropped, and an
/// `already_started` line with the seq and run id of an earlier `started`
/// line is folded into it. A line that does not parse (torn, or a field or
/// verdict a later apb wrote) is skipped. Every surface reads through it
/// (`read_with_refusal` also answers the refusal, so a surface can name
/// it). It reads beneath `.apb` through `fsutil::beneath` with the same
/// cap; a refused journal reads as no lines here, and its refusal is
/// what the views carry (`journal_error`).
pub fn read(root: &Path, incarnation: &str, limit: usize) -> Vec<LogEntry>;
```

The order for every state change is the journal first (I2): the line
that explains it (an event's verdict, a gap, a `positioned` line for a
new incarnation or a position at the head) is appended and synced, then the
state transaction commits. A crash between them repeats the change on
the next pass, which plans it again from the same state and so writes a
line of the same id, not appended twice. For an event: A crash after the sync and before the transaction repeats the
event on the next pass: a repeated skip, held, refused or failed line has
the same id and is not appended again, but it is synced (the file and the
directory chain) before the state commit it justifies, since the earlier
sync may have failed; a repeated start is answered
`already_started` by its key, a different verdict and so a second line,
which `read` folds into the `started` line. A write, repair or sync that fails
leaves the state unchanged and ends the rule's pass, with `journal_write`
on the pass's answer and nothing after the event evaluated; that holds for
an event that started a run, for one that is held (`start_key_busy` and
the hourly cap's `rate_cap` alike: a hold line that could not be written
is not reported as a hold). A torn final record
a crash left is repaired before the next append, so a replayed line is
never glued to a fragment and lost to the reader. The file is
rewritten to the newest `LOG_KEEP` lines once it holds more than
`LOG_REWRITE_AT`, only at the start of a pass under the sweep lock
(`journal_ids` above); an id the rewrite dropped belongs to an event at
or below `seen_through`, which no pass evaluates again. Every operation
on a journal (its creation, rewrite, tail repair, append, read and
removal) goes through `apb_core::fsutil::beneath` over `.apb`; no
path-based `remove_file`, `rename`, `OpenOptions` or `fsutil::sync_dir`
on a trigger state path remains. The test seam that fails or observes a
sync sits on those walked calls and is compiled out of a release build.

When `journal_ids` refuses a journal (over the cap, a link, a hard link,
a link at a component of its path, unreadable), the pass does not
substitute an empty set: that rule is not evaluated, nothing is started
for it and its position does not move in that pass, the refusal is
reported once per pass on the sweep's result, and the project's other
rules go on. The rule's view carries the sentence of
`journal_refused_text` as `journal_error`, so `apb trigger show`, MCP
`trigger_list`, the HTTP list and the dashboard's rule row show it. A
listing reads each journal once.

The journal decides nothing: the position reads the state file, and
the hourly cap reads the runs' own launch records (D1 "One pass over one
rule" item 6.2.5.1).

Every surface renders a line's words from its code
(`apb_engine::trigger::view::verdict_words`, `web/src/lib/triggers.ts`
`verdictWords`): a fixed sentence per known code from the engine's table
`apb_engine::trigger::view::CODE_WORDS` (generated into `consts.gen.ts` as
`TRIGGER_CODES` and `TRIGGER_CODE_WORDS`), `refused (<code>)`
otherwise. A whole line is printed through one renderer,
`apb_engine::trigger::view::journal_line_text` (`apb trigger log`, `apb
trigger show`): the event seq, the verdict words, the condition marks
(each condition's index and whether it passed, as `conditions #0 passed,
#1 failed`), the param name, the V codes and the run id; the dashboard's
twin is `journalLineText` in `web/src/lib/triggers.ts`, rendered by
`web/src/lib/TriggerJournal.svelte`. An engine refusal's own sentence is never stored: the text
of `concurrency_busy`, for one, renders the key's display, which may hold
a param value (`crates/apb-engine/src/concurrency.rs` lines 504 and
660).

### Evaluating one event

`trigger/evaluate.rs`, pure but for the `own_run` closure it is given:

```rust
pub enum EventVerdict {
    /// Every condition passed; the prefixed start key.
    Matched { start_key: StartKey },
    /// A code of the journal's closed set and the rule's own name it is
    /// about (D1 "The state and the trigger journal").
    Skipped { code: &'static str, name: Option<String> },
}
pub fn evaluate_event(
    rule: &Rule,
    source: &InboxSource,
    event: &InboxEvent,
    incarnation: &str,
    own_run: &dyn Fn(&str) -> bool,
) -> (EventVerdict, Vec<ConditionResult>);
```

In this order, the first that fails decides the reason:

1. the filter (`source.filter().explain(event)`): reason `type` or
   `where`; the journal keeps each condition's index and whether it
   passed (`ConditionMark`), the dry test's response the whole
   `ConditionResult`;
2. the send marker, exactly as `connector/marker.rs` writes it and only
   where the rule reads: for each body path the rule's key and param
   templates read (`render::Part::Body`), the string there is checked
   with `marker::trailing_marker(text) -> Option<MarkerFields>` (new in
   `marker.rs`, the inverse of `render`, line 49: the text ends with one
   well-formed `<!-- apb-intent v1 run=.. node=.. attempt=.. nonce=.. -->`,
   trailing whitespace ignored, and that marker occurs nowhere else in it,
   the rule `carries_exactly` applies, line 126). It is `own_send` only
   when the marker's `run` names a run directory of this project that this
   installation created (`own_run(run)`, a closure `evaluate_event` takes,
   given by the sweep and the dry test alike as
   `apb_engine::trigger::own_run(root, run)`): the `run` field comes from
   the event, so it must be one safe path segment
   (`registry::is_safe_segment`) and name a real directory (no link) that
   is a direct child of this project's `.apb/runs`, with a verified origin
   stamp (`run_origin::verify`), before it counts. A keyword quoted in other text, a marker in a
   field the rule does not read (the issue body inside a comment
   delivery), or a marker naming a run another machine made, is not a
   skip. Reason `own_send`;
3. the key: `render` of `source.key`, then `StartKey::parse` of the
   prefixed key: codes `key_missing` and `key_not_scalar` (name: the
   template's path), `key_too_long`, `key_invalid`;
4. every param in name order: codes `param_missing`,
   `param_not_scalar`, `param_too_long` (name: the param). The rendered values
   are dropped: the fire renders them again from the same event.

### One pass over one rule

`crates/apb-engine/src/trigger/mod.rs` (new, `pub mod trigger;` in
`crates/apb-engine/src/lib.rs`):

```rust
/// What a start asks the start path to do (`apb_mcp::tools::trigger::fire`).
pub struct FireRequest<'a> {
    pub root: &'a Path,
    pub entry: &'a RuleEntry,
    pub source: &'a InboxSource,
    /// The stored event the run is started for; the params come from it.
    pub event: &'a InboxEvent,
    pub start_key: String,
    pub triggered_by: TriggeredBy,
    /// `apb trigger test`: the checks before preparation, no start,
    /// nothing written.
    pub dry_run: bool,
    /// The limits on new starts (the hourly cap, `rate_cap` and
    /// `rate_cap_unknown`, and the pass's start bound `starts_per_pass`),
    /// asked by the fire only after its start key lookup said the event
    /// starts a new run: a key bound to a started run answers
    /// `already_started` before any limit (deduplication before limits).
    /// `Err(code)` is the limit that holds the event. The dry test passes
    /// one that holds nothing.
    pub admit: &'a dyn Fn() -> Result<(), String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum FireOutcome {
    Started { run_id: String },
    /// The key names a run already: the same request, or (code
    /// `start_key_conflict`) the same key with other params.
    AlreadyStarted { run_id: String, conflict: bool },
    /// A dry run whose checks before preparation all passed. Never read as
    /// "would start": preparation's checks did not run (D1 "`apb trigger
    /// test`").
    PreStartPassed,
    /// A refusal, by its `policy` code, and for `param_pattern_mismatch`
    /// the first param the refusal names (a name from the rule, never a
    /// value). The event's verdict (I2).
    Refused { code: String, name: Option<String> },
    /// `start_key_busy` only: another start with the key is still
    /// preparing (its entry pending, not bound to a started run). Tried
    /// again next pass; the position stays before the event (I2).
    Busy,
    /// A limit on new starts held the event (`admit`'s code: `rate_cap`,
    /// `rate_cap_unknown` or `starts_per_pass`). The position stays before
    /// the event and the pass ends.
    Limited { code: String },
    /// The resolved approval binding is not the approval's: the rule is
    /// unapproved until a person approves it again (D1 "Approval" item 5).
    /// Carries the binding actually compared, so the sweep's journal line
    /// and its id name what was seen, with no second assessment.
    BindingChanged { binding: String },
    /// The event's verdict (I2); a code, never a sentence.
    Failed { code: String },
}
pub type Fire<'a> = dyn Fn(&FireRequest<'_>) -> FireOutcome + 'a;

/// One pass over the rules of the project at `root`: one entry per rule
/// considered (or one `"*"` entry when the rules or the state do not
/// verify). The caller has checked that this process is not a run step
/// ("Which process evaluates rules").
/// `now` is the typed clock `apb_core::clock::Millis`, milliseconds since
/// the Unix epoch, like every time of the sweep path.
pub fn sweep(root: &Path, now: &dyn Fn() -> Millis, fire: &Fire<'_>)
    -> Vec<(String, Result<RuleSweep, EngineError>)>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleSweep {
    pub evaluated: u32,
    pub started: Vec<String>,
    /// Why the pass ended before the newest event: `rate_cap`,
    /// `start_key_busy`, `change_waiting`, `starts_per_pass`,
    /// `journal_write`.
    pub stopped: Option<String>,
}
```

The engine never names `apb_mcp`: the start is a parameter, so the start
sequence stays where every other start surface builds it. A pass first
reads `rules.json` with `signed_store::read` (line 148) to list the rule
names; `Loaded.signed == false` returns the `"*"` error
`trigger_rules_unreadable` and the project is not swept. It also reads the
signed `state` file once at the project level, only to decide the one
`"*"` answer for a state file that does not verify; no rule's pass uses
that read. Then, for each
rule name whose entry is enabled and approved (`approval.rule_digest`
equal to `rule_digest(entry)`):

1. The rule name is checked (`apb_core::trigger::checked_rule_name`: the
   rule name pattern, one safe segment) before any filesystem operation,
   as `hold_rule_lock` checks it at the lock and store boundary; then
   `fsutil::try_hold_lock(&root.join(".apb/trigger-state"), &format!("{rule}.sweep.lock"))`;
   `WouldBlock` skips the rule (I4).
2. Re-read `rules.json` under the lock (`Loaded.signed` checked) and
   take the entry; one that is gone, disabled or not approved for its
   digest is released untouched. Remember its incarnation and
   `rule_digest`. Read the rule's state (the signed `state` file) now,
   under the lock, never from a read made before it.
3. The position (I5), line first (I2): `state::journal_ids` of the
   incarnation and the rewrite to `LOG_KEEP` lines when due; then, when
   `state::needs_position` on the state read in step 2 (a state missing,
   removed by a person after it did not verify, or of another
   incarnation), `Inbox::head_seq()` of the source and
   `state::position_at_head` at that CURRENT head, never at an older seq;
   it appends and syncs the `positioned` line before its state
   transaction, and inside that transaction positions only while the
   rule's entry is still missing or of another incarnation (compare and
   set; it returns whether it positioned), so a second sweeper that read
   the state before the first committed does not position again; no file
   is removed. The pass then releases the rule: it starts nothing for it in this pass.
   What arrived between `approve` or `enable` and this pass, and what
   arrived while a state was missing, is skipped; that is the safe
   direction, at most one sweep period after a command, since an old
   event replayed after its start key expired would start a second run.
4. Read the entry's source: `Inbox::open(connector, account)` (line 222),
   `read_events()` (line 590), which registers nothing; keep the events
   with `seq > seen_through`, at most `MAX_EVENTS_PER_PASS`. A read error
   is printed and the rule is tried again next pass; nothing is logged.
5. A gap: when the oldest kept event's seq is above `seen_through + 1`
   (retention removed events the rule had not seen, D1 "Inbox additions
   for rules"), a `gap` line with the range (id
   `<incarnation>:gap:<a>-<b>`) is appended and synced, then one state
   transaction moves `seen_through` to `b`, and the pass continues from the oldest kept event.
6. Each event in seq order goes through `evaluate_event` (pure):
   1. Skipped: its `skipped` line joins the open batch, kept in memory.
   2. Matched, in this order:
      1. `fsutil::flock_held(dir, "<rule>.change.lock")` is true: flush the
         batch, end (`change_waiting`, no line) (I4);
      2. re-read `rules.json` (`Loaded.signed` checked): the entry must
         be present, enabled, approved, of the incarnation and the
         `rule_digest` of step 2, else flush the batch and end (under the
         sweep lock it never differs; this read is what shows it does
         not);
      3. `check_rule` again (the playbook or the connector may have
         changed): an error puts a `refused` line with code `check_rule`
         and the validator codes into the batch, and the next event
         follows;
      4. flush the batch (item 7), so the position never passes a match
         that did not start;
      5. `fire(&FireRequest { .. })` with only the sweep lock held, with
         `TriggeredBy { rule, incarnation, source: "inbox", connector, account, event_seq: Some(seq) }`
         and the limits on new starts as `admit`. The fire looks the start
         key up first and asks `admit` only when the lookup said the event
         starts a new run (deduplication before limits): a key bound to a
         started run answers `already_started` before the hourly cap and
         the per-pass bound are applied, so a pass that crashed after a
         start and before recording it recovers at once, not after an
         hour. `admit` holds the event, in this order, when:
         1. the hourly cap: the runs this incarnation started in the last
            60 minutes reach `max_per_hour`: code `rate_cap`. The count is not kept in the
            state: `trigger::starts_in_last_hour(root, incarnation, now: Millis)`
            reads it from the runs' own records each time `admit` is asked,
            not once per pass, so a run any earlier outcome of the same pass
            left behind (a start, a hand-over that failed after preparation,
            a refusal after the manifest) counts at once. It walks the project's run directories
            with a fallible enumeration, `trigger::run_dirs_strict(root) ->
            io::Result<Vec<PathBuf>>` (new beside the count), not
            `decision::report::candidate_run_dirs` (`decision/report/mod.rs`
            line 296), which silently answers no entries when `read_dir`
            fails (line 303) while `allocate_run` can still create a run in
            a directory that can be written but not listed (`prepare.rs`
            line 576): a runs directory that cannot be read makes the cap
            count as reached (code `rate_cap_unknown`); keeps those whose journal
            was written within the last hour (a run started in the hour
            wrote its journal in the hour), and keeps in the window every
            directory whose journal (`events.jsonl`) is missing or whose
            metadata does not read (it is classified, never left out
            before classification), newest first, at most
            `run_view::STARTED_RUNS_SCAN` (500, `run_view/started.rs` line
            11, the bound of the reverse-link scan `started_runs` at line 60),
            and classifies each:
            1. attributed: its origin stamp verifies
               (`decision::report::created_here`, line 288), its manifest
               reads, its launch record is not a child's (`kind` other than
               `child`, `gate/launch.rs` line 440) and no parent is named in
               its run config (`run_config.rs` line 176), and its
               `triggered_by.incarnation` is this one: counted when its
               `run_started` (`run_history::started`, `run_history.rs` line
               52) is within the hour, and counted as a start inside the
               window when its start time cannot be read;
            2. not a start of this incarnation: origin verified, a manifest that
               reads with a launch record whose `initiator` is not
               `trigger:<rule>` (D0 item 2, `Initiator::label`), a record
               of another incarnation, a child of a
               run, or a stamped directory with no manifest file at all: not
               counted. These are the only directories left out, since
               only their remaining records prove this incarnation did not
               start them;
            3. unattributable: everything else in the window (a stamp
               that does not verify, which includes the window of
               `manifest::amend` between its manifest write and its restamp,
               `manifest.rs` lines 873 to 876, a manifest file that does not
               read, or a launch record whose `initiator` is `trigger:<rule>` but
               which lost `triggered_by`, as an older apb's
               `manifest::amend` would drop it, while it keeps the known
               string field `initiator`):
               counted toward the cap of every rule of the project. The
               reused `newest_run_dirs_where` (line 257) is not used here
               because it drops unverified directories before its cap
               (line 274), which would make the count looser. A stamped
               directory with no manifest is excluded because no work
               started there (a start writes its manifest with the launch
               record before any work): a failed preparation, a rule's included, is
               handled as on main, and `prep_try_unstarted`
               (`prepare.rs` lines 157 to 176) writes `run_started` and
               stamps the directory with no manifest; preparation never
               reached the point where a node runs. Counting it would let one
               failed start, a person's or a rule's, hold every rule of the
               project for an hour.
            A start whose line or event was lost (a crash, an event
            retention evicted) is counted all the same, since its launch
            record is written before `run_started`. The bound, decided: V264
            refuses a `max_per_hour` above 500, so the scan can always count
            to the cap; when more than 500 run directories of the project
            were written in the last hour the scan is truncated and the cap
            counts as reached (stricter, never looser, code `rate_cap_unknown`);
         2. `MAX_STARTS_PER_PASS` starts made in this pass: code
            `starts_per_pass`;
      6. the outcome. `Busy`: a `held` line `start_key_busy` (id
         `<incarnation>:<seq>:held:start_key_busy`), end the pass with the
         position before the event. `Limited { code }`: for `rate_cap` and
         `rate_cap_unknown` a `held` line with the code (id
         `<incarnation>:<seq>:held:<code>`, so a blocked event is logged
         once however many passes it waits), for `starts_per_pass` no
         line; the pass ends (`stopped: <code>`) with the position before
         the event. `BindingChanged { binding }`: the
         line, then the revocation, then the end of the pass, as D1
         "Approval" item 5 says; the position stays before the event. Every other outcome: its line
         (`started`, `already_started`, `refused`, `failed`, with the run
         id or the code) is appended and synced; then one state
         transaction sets `seen_through = seq`. Nothing about starts is
         stored; the next matching start reads the count again.
7. Flushing the batch: `state::append` of its lines (one sync for the
   batch), then one state transaction moving `seen_through` to the
   batch's last seq. Any append error ends the pass with the position
   where it was (`stopped: journal_write`); the lines that were written
   are repeated next pass under the same ids and so not appended twice.
8. At the end of the events, or when the pass ends early: flush the
   open batch, release the lock.

Exactly three verdicts keep the position before the event for the next
pass: a busy or still preparing start key, the hourly cap and a changed
approval binding. A pass that ends early (a waiting change, the per-pass
start bound, a journal write error, or a re-read showing the rule
changed) also resumes before the event. Every other outcome (started,
already started, refused, failed, skipped) moves the position past the
event.

The hourly count of item 6.2.5.1 is the only memory of starts: an
`already_started` answer adds nothing to it, since the run it names
was counted from its own launch record if it started in the hour.

No pass deletes a state entry or a journal file by name (Cut in the
fourth pass, item 5): `approve` and `enable` delete the rule's state entry and `remove` deletes
the state entry and the journal of the rule it removes, under the rule's
locks (D1 "Where rules live" item 6). The journal of an earlier
incarnation (a source change, a crashed `remove`) is never deleted by a
command or a pass: it is inert and the doctor lists it.

A pass that fails inside a rule leaves the state as its last transaction
wrote it; the cases are in "Invariants". A refusal is not retried: the
journal says why, and `apb trigger test` checks the event again once a
person fixed the cause; a later event starts normally.

Why the generation the second pass added is gone (decision of the third
pass): it served two purposes. Across a remove and a creation of the
same name it told an old state from a new one; the incarnation does
that, and also separates the journal and the start keys, which a
generation counted from 1 again could not. Within a pass it let the pass
start only fires an older version of the rule had chosen; with no
pending fire nothing is chosen ahead of its start, and the entry is
re-read under the lock right before each start (item 6.2.2). Together
they cover it, so no generation is kept.

### The fire

`crates/apb-mcp/src/tools/trigger.rs` (new, `pub mod trigger;` in
`tools/mod.rs`) `pub fn fire(req: &FireRequest<'_>) -> FireOutcome`
follows `playbook_run_for` (`server/run.rs` line 374) step by step, with
the rule in place of the arguments:

1. `wref = PlaybookRef { origin, id, version: Some(version) }` (`origin`
   from `RuleScope`).
2. `external_data = external_data::for_surface_start(root, Some(rule.external_data), None)`.
3. Render every param from `req.event`, the event the pass just
   evaluated (an error now is `Refused` with the skip code as its code).
4. The start key: `StartRequestFields { v: REQUEST_V, version, params, execution: Some("cli"), supervision: label(&RunMode::Autonomous), cache: label(&CacheRunMode::Auto), budget, unattended: true, external_data, ..Default::default() }`
   (`start_key.rs` line 98). `budget` is the canonical JSON of the
   rule's stored `BudgetRequest`:
   `rule.budget.as_ref().map(serde_json::to_value).transpose()`, an
   error being `Failed { code: "budget_unencodable" }`. That is the
   conversion `apb run` makes of its typed budget
   (`crates/apb-cli/src/run.rs` line 760) and MCP `playbook_run` of its
   argument (`crates/apb-mcp/src/server/run.rs` line 1560): absent
   fields are skipped (`skip_serializing_if`), and object keys come out
   sorted because the workspace's `serde_json` has no `preserve_order`
   feature, so `StartRequest::capture` (line 150) hashes the same bytes
   for the same rule every time. A person who edits the rule's budget
   changes the request: a later event of a key still alive is then
   `start_key_conflict`, recorded `already_started` below. The lookup is
   `tools::run::keyed_lookup_holding(root, scope_label, id, &req.start_key, false, &fields)`,
   or for a dry run `tools::run::keyed_peek` (same arguments without
   `reuse`, same mapping; D1 "`apb trigger test`"); both answer
   `KeyAnswer::{Go, Named { run_id, conflict, bound }}` over
   `apb_engine::start_key::Lookup::{Started, Conflict}`, which carry
   `bound: bool`. A start key entry that is only pending (claimed or
   attached, its owner alive, not bound to a started run) never consumes
   an event: `Named` with `bound: false`, the same request or a conflict,
   is `Busy` (`start_key_busy`, the position stays before the event and
   the next pass decides again). Only `Named` with `bound: true` is
   `AlreadyStarted` (its `run_id`, `conflict: true` for
   `start_key_conflict`: one run per rendered key is the rule's contract).
   `start_key_busy` is `Busy` (the one transient answer, I2); any other
   error `Refused { code: policy }`. A person's surfaces keep
   `tools::run::keyed_lookup` unchanged and still answer a preparing run
   as already started. On `Go`, the fire asks `req.admit()` before any
   other step: `Err(code)` is `Limited { code }` (deduplication before
   limits, D1 "One pass over one rule" item 6.2.5).
5. `gate::check_unattended(root, &UnattendedStart { unattended: true, mode: Autonomous, execution: &cli })`.
6. `ExecutionRequest { mode: Some(Cli), host_session: false, client: Some(format!("trigger:{rule}")), inherited: false }`
   and `execution::resolve_for`.
7. `LaunchContext::new(Initiator::Trigger { by }, LaunchKind::Start).with_external_data(external_data).with_approved_binding(approval.binding)`,
   the approval used only when its `rule_digest` equals the rule's.
8. `gate::check_run(root, &wref, false, false, &launch)` (never
   `acknowledge_untrusted`: a rule is no trust approval). The permit is
   compared in one place, inside it: `Gate::admit` compares the approved
   digest with the permit it resolved before `launch::decide` (D0 "Who
   starts the run" item 7), so the comparison and the decision see the
   same permit, and for `ask` the equal approval is the confirmation
   (item 8 there); no launch nonce is stored. A `binding_changed` refusal,
   read with `launch::binding_changed_of`, is
   `FireOutcome::BindingChanged { binding }` (D1 "Approval" item 5);
   a dry run reports it as a refused check `binding_changed`. Every other
   refusal stays as it is. The permit `check_run` returns is the one
   handed to the engine verbatim as the expected pins of step 13
   (`expected_digest`, `expected_profile_bundles`, `expected_children`,
   `expected_connectors`, `expected_connector_accounts`: the anti-TOCTOU
   hand-over of decision record 0003, as MCP `playbook_run` does at
   `crates/apb-mcp/src/server/run.rs` lines 717 and 718, checked by
   preparation through `ExpectedBundle`, `scheduler/prepare.rs` line
   1465), so a profile that changes after admission fails preparation's
   own bundle check and starts nothing.
9. The consent: when `permit.needs_consent()`,
   `permit.check_confirmation(Some(&Confirmation::Nonce(consent_nonce)))`
   with the approval's nonce (none: the refusal), and
   `RunConsent::irreversible(format!("rule_approval:{rule}"))`
   (`crates/apb-engine/src/consent.rs` line 58).
10. `gate::check_params_with(root, &pinned, &params, external_data)`.
11. `store::resolve`, then `tools::budget::preflight(&resolved, "cli", rule.budget.as_ref())`;
    no effective budget is `Refused { code: "trigger_budget_required" }`.
12. The notify request: `NotifyArg::Flag(b)` for `Some(b)`, none for
    `None`, through `NotifyArg::request_of` and `tools::notify::start_check`.
13. A dry run returns `PreStartPassed` here: steps 1 to 12 are what a
    surface checks before preparation; preparation's own checks
    (probation, readiness, the concurrency key, the working tree) did not
    run (D1 "`apb trigger test`"). Otherwise
    `tools::StartArgs { launch: permit.launch, params, instruction: None, expected_digest, expected_profile_bundles, expected_children, expected_connectors, expected_connector_accounts, continued_from: None, worktree: None, execution, consent, candidate_skipped: None, budget, estimate, notify, unattended: true, external_data, deadline_ms: None, start_key: Some(req) }`,
    turned into `RunOptions` by `tools::start_options(args, RunMode::Autonomous)`
    (`tools/run.rs` line 61), then `opts.require_ready = rule.require_ready`
    and `opts.waive_probation = false` (neither is a `StartArgs` field;
    preparation reads both for an unattended start, which a rule's start
    always is), and
    `opts.workdir_queue_wait = GlobalConfig::load().unwrap_or_default().server.workdir_queue_wait()`
    (`crates/apb-core/src/config.rs` line 1070): `start_options` sets it
    to `None` (`tools/run.rs` line 72), which fails a start at once with
    `WorkdirBusy` while another write run holds the tree, and that would
    make a busy tree the event's verdict; with the dashboard's wait
    (`routes/playbooks.rs` line 454) the run is admitted and waits for the
    tree in its drive. Then
    `apb_engine::start_detached(root, id, Some(version), opts)` (project
    scope) or `apb_engine::start_detached_resolved(&resolved, opts)`
    (global scope). A claim that finds the key taken after the lookup
    said free is looked up again: only a run the key is bound to answers
    `AlreadyStarted` (`EngineError::AlreadyStarted(id)`, or
    `AlreadyStarted { conflict: true }` for `start_key_conflict`, its run
    id parsed from the detail), anything else is `Busy`;
    `EngineError::WorkdirBusy` (queueing switched off,
    `server.workdir_queue_wait_seconds: 0`) is
    `Refused { code: "workdir_busy" }`; an
    `EngineError::Invalid` or `Conflict` whose text is `<code>: <detail>`
    is `Busy` for `start_key_busy`, the look-up-again rule above for
    `start_key_conflict`, else
    `Refused { code }` (`concurrency_busy`, `probation_required`,
    `not_ready`, `playbook_on_hold` and the rest); any other error
    `Failed { code: "start_failed" }`. A refusal value from any step is
    `Refused` with its `policy` (or `error`) as the code, and for
    `param_pattern_mismatch` the first entry of its `invalid` list
    (`gate/params.rs` lines 83 to 86) as `name`. The detail
    sentence is dropped here, never stored (D1 "The state and the trigger
    journal"): `concurrency_busy`'s names the key's display, which renders
    the run's params (`concurrency.rs` line 504, the sentence at line
    660).

Why the MCP crate: every check above is a `tools::` function the MCP
start, the HTTP start and the CLI already share; the engine cannot call
up into it (the dependency direction of `CLAUDE.md`).

### A rule's run keeps its driver

A rule's run parks exactly as any unattended run does on main, which is:
it does not. `deferred::may_release` (`scheduler/deferred.rs` line 42)
keeps an unattended run's driver on a deferred retry, and the wait park
(`scheduler/wait_park.rs`, its `may_release:` input at `scheduler.rs`
line 2731) keeps it on a long timer wait; this change touches neither.
A rule's run on a long timer wait therefore holds its detached driver
until the wait ends, as `apb run --unattended` in CI does, and
`docs/TRIGGERS.md` says so; letting a rule's run release its driver on a
long wait is future work.

### Probation

A rule's run never earns attended probation credit, and needs no
exclusion of its own for it. Read on efae5822: the count of attended
successes takes only entries with `unattended == false`
(`apb_core::probation::counted`, `crates/apb-core/src/probation.rs`
lines 80 to 91), and the engine's ledger writer records the run's mode
as `unattended` (`crates/apb-engine/src/probation.rs` `entry_of`, line
187). A rule's start is always unattended ("The fire" step 5), and a
child inherits its parent's unattended mode (`scheduler/node.rs` lines
5013 to 5015), recursively, so no run of a rule's tree can be counted.
`crates/apb-engine/src/probation.rs` does not change. A rule's run with
`external_data: false` leaves a ledger entry with `unattended: true`;
a default rule run, external data, leaves none (the observer returns
first, `probation.rs` line 86).

Tests: `a_rule_run_earns_no_probation_credit`
(`crates/apb-engine/tests/suite/trigger_test.rs`, through the real keyed
start with `external_data: false`: a one-node playbook that succeeds
leaves an entry with `unattended: true` and `counted` stays 0, and a
sub-playbook's child entry is `unattended: true` too) and
`a_default_rule_run_writes_no_probation_entry`.

### Approval

`tools::trigger`:

```rust
/// What a person approves for a rule: shown by every surface, posted back
/// unchanged by the dashboard, compared field by field at `approve`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalNeed {
    pub rule: String,
    /// `rule_digest` of the entry (D1 "Where rules live" item 1): every
    /// field that changes whether or how the rule fires, and its
    /// incarnation.
    pub rule_digest: String,
    pub playbook: String,
    pub version: String,
    pub playbook_digest: String,
    /// `permit.permit_digest()` of the gate's `RunPermit` for the target.
    pub permit_digest: String,
    pub launch_policy: String,
    pub launch_sources: Vec<String>,
    /// The full billing decision for the tree's executors, as
    /// `billing.binding_lines()` (D0 "Who starts the run" item 5).
    pub billing: Vec<String>,
    /// `approval_binding` of the three above: what `approve` stores and
    /// admission compares.
    pub binding: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub irreversible_sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent_nonce: Option<String>,
}
pub fn approval_need(root: &Path, entry: &RuleEntry) -> Result<ApprovalNeed, Value>;
/// Under both rule locks: a fresh `approval_need` of the stored entry,
/// compared with `shown` (the digests and the consent nonce with `ct_eq_str`,
/// every other field with `==`); any difference refuses
/// `trigger_approval_changed`. Then `store_approval_locked`.
pub fn approve(root: &Path, rule: &str, shown: &ApprovalNeed, by: &str) -> Result<Approval, Value>;
```

1. `approval_need` runs `check_run` with `Initiator::Trigger` and no
   `approved_binding`: every launch policy then refuses in `Gate::admit`
   with `launch_requires_rule_approval` carrying the resolved `binding`,
   `permit_digest`, `launch_policy` and `billing` (D0 "Who starts the run"
   item 7), which the need shows. It runs `check_run` again with
   `approved_binding` set to that binding, which admission lets through
   to `launch::decide`: `launch_requires_person` (deny) refuses the
   approval `trigger_launch_denied: only a person starts playbook `{id}`; a rule cannot start it`,
   `billing_refused_for_trigger` refuses it too, any other refusal is
   returned as is, and with the permit `needs_consent()` gives
   `consent_nonce()` and the sources.
2. `approve` takes the rule's locks (`hold_rule_lock`) before it
   computes the fresh need, so what is compared is what is stored. A
   change of the source, the params, the limits or any other field
   between showing and approving changes `rule_digest` and is refused,
   even when every launch field is equal. It writes
   `Approval { rule_digest, playbook_digest, binding, permit_digest, launch_policy, consent_nonce, approved_at, approved_by }`,
   after deleting the rule's state (D1 "Where rules live" item 6.5).
   Every rule needs an approval to fire, an `allow` playbook's too (I1). The sweep passes over a rule
   whose approval is missing or names another digest, and the start uses
   the approval only while its `rule_digest` equals the entry's ("The
   fire" step 7).
3. No passkey is involved: `approve` is a person's command under main's
   rule (D0 "A person's command"), and I1 states the boundary once.
4. The HTTP body is `ApproveBody { shown: ApprovalNeed }`
   with `#[serde(deny_unknown_fields)]`; a need with an unknown field is
   422.
5. The binding ties the approval to the exact permit, the launch policy
   in force and the billing decision, whatever the launch policy: an
   `allow` launch has no launch nonce, the irreversible consent binds
   definition digests only (`crates/apb-engine/src/consent.rs` line 113),
   and a launch floor or `policy.yaml` can make the policy stricter after
   the approval; main never lets an earlier confirmation cover a stricter
   policy (`gate/launch.rs` lines 948 to 950). `approve` compares
   `binding` with the fresh need's (a change between showing and
   approving is `trigger_approval_changed`), and every start compares it
   in `Gate::admit`, before the launch decision ("The fire" step 8), and
   only there. The guarantee is that a rule never starts a run under a
   binding other than the approved one, so revocation happens when a
   start is attempted under a changed binding: an event the start key
   answers `already_started` ("The fire" step 4, before admission)
   attempts no start and revokes nothing, and a binding that changed and
   was restored with no start attempted in between leaves the approval
   valid. A comparison before the key lookup would need a second gate
   resolution beside admission, the mechanism the eighth pass removed. At
   an attempted start, a difference, a stricter policy included, is
   `FireOutcome::BindingChanged { binding }` and revokes the approval
   durably, under the rule's sweep lock the pass holds, in this order:
   1. a `held` line `binding_changed` with `binding` the one resolved and
      `approved` the approval's (id
      `<incarnation>:<seq>:binding_changed:<resolved binding>`: the event's
      seq and the binding are both stable across a crash repeat, and the
      seq keeps a later revocation to a binding seen before from sharing an
      id), appended and synced;
   2. one `rules.json` transaction, `trigger::rule::revoke_approval_locked(root, name, incarnation, rule_digest, approved_binding)`:
      it clears `approval` only while the entry still has that
      incarnation, that `rule_digest` and an approval whose `binding` is
      `approved_binding` (compare and clear; anything
      else is left as it is);
   3. the pass ends with the position before the event.
   This is the one write to `rules.json` that is not a person's (I1): it
   can only take an approval away, never give or change one. A revoked
   rule is not swept (it is not approved), lists as needing approval,
   and starts nothing until a fresh `approve` commits, which also deletes
   its state (item 6.5 of "Where rules live"), so the next pass positions
   it at the head and the events held meanwhile never start. Restoring the old target after a
   revocation does not bring the approval back: it is gone from
   `rules.json`. A crash between
   the line and the rules transaction leaves the approval in place; the
   next pass compares again: if the target is still changed it finds the
   same digest, its line has the same id and is not appended twice, and
   it completes the revocation; if the target was restored meanwhile the
   approval stands, which is right, because no revocation committed and
   the binding is the one a person approved. A consent nonce gone stale
   still refuses as `irreversible_requires_confirmation`; the journal names
   `apb trigger approve <rule>`. Events that arrive
   while a rule waits for its first approval, or for a new one after a
   change, are not evaluated at all: approving deletes the state and the
   next pass positions the rule at the head it reads then, so the rule
   acts only on what arrives once it is approved (and up to one sweep
   period after, the safe direction, I5).

### The sweep host

`crates/apb-server/src/trigger_sweep.rs` (new), modelled on
`health_sweep.rs`:

```rust
pub(crate) const TRIGGER_SWEEP_ENV: &str = "APB_TRIGGER_SWEEP_MS";
/// 5 s, shortened by the variable in a debug build only
/// (`apb_engine::env_duration::test_override`).
pub(crate) fn period() -> Duration;
/// Wakes the task now (an append just stored an event). A no-op before
/// `spawn` ran in this process.
pub(crate) fn nudge();
/// One pass over `roots`; the number of rules considered. Errors are
/// printed (`apb trigger: <root> <rule>: <error>`) and the pass goes on.
pub(crate) fn sweep_once(roots: &[PathBuf]) -> usize;
/// Starts the task once per process (a `OnceLock`).
pub(crate) fn spawn();
```

The loop: `tokio::select!` over `sleep(period())` and the
`tokio::sync::Notify` of `nudge`, then `spawn_blocking` of `sweep_once`
over the paths of `projects::list_active()`, the registered projects
only (`crates/apb-core/src/projects.rs` line 899, as `health_sweep::spawn`, line 34, does), each root through
`apb_mcp::tools::trigger::sweep_root(root)` (which takes no clock and calls
`apb_engine::trigger::sweep(root, &Millis::now, &fire)`, every time in
milliseconds since the Unix epoch), then
`apb_engine::trigger::view::beat()`.

In `crates/apb-engine/src/trigger/view.rs`, beside the doctor line that
reads it:

```rust
pub const LISTENER_FILE: &str = "trigger-listener.json";
/// Writes `{ pid, at_ms }` atomically under the config dir.
pub fn beat();
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenerStatus { Live { age_ms: u64 }, Stale { age_ms: u64 }, Absent }
/// Live: the file is younger than 60 s and its pid is alive
/// (`liveness::pid_alive`, `crates/apb-engine/src/liveness.rs` line 86).
pub fn status(now_ms: u64) -> ListenerStatus;
```

Wiring: `mod trigger_sweep;` in `crates/apb-server/src/lib.rs` next to
`mod health_sweep;` (line 17). `run_ingest_server` (`ingest.rs` line
464) calls `crate::trigger_sweep::spawn()` before it serves, so `apb
ingest` and the dashboard's co-started listener both host it. In
`post_hook_handler`, the arm at line 790 is split: `Ok(Appended::Stored(_))`
calls `crate::trigger_sweep::nudge()` and answers 200; `Duplicate`
answers 200 as before. `apb resume --all`: `resume_all_cmd`
(`crates/apb-cli/src/run.rs`) calls
`apb_mcp::tools::trigger::sweep_root(root)` once after its health
sweep (line 2900) when `projects::registered_id(root)` names the project
and `StepCaller::from_env()` names no run or host task ("Which process
evaluates rules"; inside a step it prints
`rule sweep skipped: apb resume --all runs inside a run step` and reads
nothing), and prints each rule's started runs. `sweep_once` in the
listener makes the same check once at `spawn`; the listener is a
person's process, so it never finds one.

### `apb trigger test`

`tools::trigger`:

```rust
/// The answer of `apb trigger test`, MCP `trigger_test` and the HTTP test
/// route. Every field is typed; values appear only where `show_values` is
/// true (the CLI at a terminal outside a run step).
#[derive(Debug, Clone, Serialize)]
pub struct TestReport {
    pub rule: String,
    pub seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    pub filter: FilterVerdict,
    pub conditions: Vec<ConditionResult>,
    /// One per param of the rule, in name order.
    pub params: Vec<ParamCheck>,
    pub key: KeyReport,
    /// From the fire's own start key lookup (`FireTrace.key_use`); `None`
    /// when the key did not render.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_use: Option<KeyUse>,
    /// Exactly one line per `CheckId`, in `CheckId::ORDER`.
    pub checks: Vec<CheckLine>,
    /// Always `["probation", "readiness", "concurrency", "working_tree"]`,
    /// shown as "checked at start"; never run by a test.
    pub checked_at_start: [&'static str; 4],
}
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum FilterVerdict {
    Matched,
    /// A code of the journal's closed set (D1 "Evaluating one event") and
    /// the rule's own path or param name it is about.
    Skipped { code: String, #[serde(skip_serializing_if = "Option::is_none")] name: Option<String> },
}
#[derive(Debug, Clone, Serialize)]
pub struct ParamCheck {
    pub name: String,
    /// The rendered value, `show_values` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Characters of the rendered value; `None` when it did not render.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chars: Option<usize>,
    /// The param's `pattern` against the rendered value; `None` without a
    /// pattern or a rendered value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern_ok: Option<bool>,
    /// Why it did not render; `None` when it rendered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<RenderFailure>,
}
#[derive(Debug, Clone, Serialize)]
pub struct KeyReport {
    /// `start_key::key_tag_if_keyed` of the prefixed key; never the key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// The rendered key, `show_values` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<RenderFailure>,
}
/// `render::RenderError` as a report shows it: the rule's path, never a
/// body value.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RenderFailure { Missing { path: String }, NotScalar { path: String }, TooLong, Invalid }
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum KeyUse { Free, Used { run_id: String, conflict: bool }, Busy }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckId {
    Approved, RenderParams, StartKey, Unattended, Execution, RunGate,
    Binding, Launch, Consent, ParamPatterns, Budget, Notify,
}
impl CheckId {
    /// The order: whether the rule is approved for its current rule digest
    /// (`approved`: `rule_not_approved`), then "The fire" steps: render (3), start key
    /// (4), unattended (5), execution (6), then `check_run` (8) split in
    /// three by where its refusal comes from: `run_gate` for every refusal
    /// `check_run` gives before the binding comparison (lifecycle,
    /// `playbook_on_hold`, `imported_scripts_require_approval`, trust, pins,
    /// `gate/mod.rs` lines 731 to 741, and `policy_file_invalid` from
    /// `launch::effective_for`, lines 899 to 904), `binding` for the
    /// comparison in `Gate::admit` (`binding_changed`), `launch` for
    /// `launch::decide` (deny, billing); then consent (9), param patterns
    /// (10), budget (11), notify (12).
    pub const ORDER: [CheckId; 12] = [
        CheckId::Approved, CheckId::RenderParams, CheckId::StartKey,
        CheckId::Unattended, CheckId::Execution, CheckId::RunGate,
        CheckId::Binding, CheckId::Launch, CheckId::Consent,
        CheckId::ParamPatterns, CheckId::Budget, CheckId::Notify,
    ];
}
#[derive(Debug, Clone, Serialize)]
pub struct CheckLine { pub id: CheckId, pub state: CheckState }
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CheckState {
    Passed,
    /// The code the real start would record (`binding_changed` for a
    /// changed binding, `param_missing` for a param that did not render).
    Refused { code: String },
    /// An earlier check refused, so this one was not run, as in a real
    /// start, which stops at its first refusal.
    NotRun,
}
pub fn test_rule(root: &Path, rule: &str, seq: Option<u64>, show_values: bool) -> Result<TestReport, ToolError>;
```

The trace comes from the one function the real start runs:
`tools::trigger::fire_traced(req: &FireRequest<'_>, trace: &mut FireTrace) -> FireOutcome`,
with `FireTrace { checks: Vec<CheckLine>, key_use: Option<KeyUse> }`,
pushes one `CheckLine` as each step of "The fire" decides (`Passed` or
`Refused`) and records `key_use` from its own start key lookup (there is
no second peek), and `fire(req)` is `fire_traced(req, &mut FireTrace::default())`, so the
sweep and the test run the same steps in the same order; there is no
second implementation. With `dry_run: true` it stops after step 12, and
its `admit` holds nothing (the dry test starts nothing).
`test_rule` first writes the `Approved` line itself (the sweep never
calls the fire for an unapproved rule, so this line is the test's own,
from the stored entry). It is the one line that does not stop the trace: for an
unapproved rule the dry fire runs with `approved_binding` set to the
binding the gate resolves now (as `approval_need` does), so the other
lines say what a start would meet once it is approved. Then it takes the
trace and appends `NotRun` for every `CheckId` of `ORDER` it does not
hold, so `checks` always has twelve lines in order. A
param that cannot render gives its `ParamCheck` a `failure`, no `chars`
and no `pattern_ok`, and `RenderParams` is `Refused` with the param
code; every later line is `NotRun` and `key_use` is `None` when the key
did not render. A refusal of `check_run` before the comparison (a draft or
retired target, a hold, a trust or pin refusal, `policy_file_invalid`)
is `RunGate` `Refused` with its code; a changed binding is `Binding`
`Refused` `binding_changed`; each with every later line `NotRun`; a test never revokes anything.

It evaluates the stored event (the newest when `seq` is `None`) exactly
as the sweep does, then calls `fire` with `dry_run: true`, which runs
the checks a surface runs before preparation and stops there
(`PreStartPassed`, or the first `Refused` code). It claims no more:
probation and readiness are decided only in preparation
(`scheduler/prepare.rs` lines 775 and 791), the concurrency key is
admitted later still (line 1252) and depends on what runs then, and the
working tree is taken in the drive; the report lists those four by name
as "checked at start" and never says "would start". Every surface
(`apb trigger test`, MCP `trigger_test`, the dashboard) prints
`passes the checks before start; checked at start: probation, readiness, concurrency, working tree`
for a match whose checks passed. Without
`--event` it takes the newest stored event of the source, read with
`read_events()` (line 590); with it, the event of that seq (`None`: the
report says the event is gone). It writes
nothing: no state, no journal line, no challenge (D0, the trigger
billing arm), no run directory, no lock file, no directory, no
installation key, and it judges no reservation. It takes no sweep lock
(it reads and decides nothing), reads `rules.json` and `state.json`
with `signed_store::read` (no lock; `Loaded.signed` checked), and works
the same in a read-only MCP process (`apb mcp --read-only`, where
`process_mode::guard` refuses every write: nothing on its path calls
it). `show_values` is true only for the CLI at a terminal outside a run
step.

The two keyed MACs on its path read the existing installation key and
never create one: `apb_core::start_key::key_tag_if_keyed(&StartKey) -> Option<String>`
and `StartRequest::capture_if_keyed(&StartRequestFields) -> Option<StartRequest>`
(new in `crates/apb-core/src/start_key.rs`, over
`run_origin::tag_if_keyed`, `crates/apb-core/src/run_origin.rs` line
136). `key_tag` and `capture` go through `run_origin::tag`, whose
`ensure_key` (line 42) calls `process_mode::guard` first and may write
the key, so they would refuse in read-only mode. `None` (no key) cannot
happen once `rules.json` verified, which needs the key; it is answered
`Refused { code: "trigger_key_unavailable" }`.

The start key is peeked, never looked up:

```rust
/// `crates/apb-engine/src/start_key.rs`, beside `lookup` (line 231):
/// the same answer for `req` without writing anything. The store is read
/// with `signed_store::read` (no lock; `Loaded.signed == false` is the
/// `start_key_state_unreadable` error), the live entry found with
/// `StartKeysFile::find`, and a pending entry's owner judged with
/// `fsutil::flock_held`: held is `Busy` (answered once, no wait loop),
/// free or missing is judged as `judge` does after its probe, without
/// removing the probe's file. `checked_run` (line 107) already only reads.
pub fn peek(root: &Path, scope_label: &str, id: &str, req: &StartKeyRequest) -> Result<Lookup, EngineError>;
```

`crates/apb-core/src/fsutil.rs` gets, next to `try_hold_lock` (line 815):

```rust
/// Whether another process holds the lock file `dir/lock_name`, without
/// creating or changing anything: the file is opened read-only with no
/// link followed (`O_RDONLY | O_NOFOLLOW`, no `O_CREAT`) and probed with
/// a non-blocking shared `flock`, released at once. A missing file is
/// `Ok(false)`.
pub fn flock_held(dir: &Path, lock_name: &str) -> io::Result<bool>;
```

`apb_mcp::tools::run::keyed_peek(root, scope_label, id, raw, &fields)`
is `keyed_lookup_holding` with `capture_if_keyed` and `peek` in place of
`capture` and `lookup`, with the same `KeyAnswer`. Why not `keyed_lookup`: it calls
`start_key::lookup`, which runs inside `signed_store::transact` (its lock
creates a directory and a lock file, `signed_store.rs` line 267), and
whose `judge` (line 147) probes a pending entry with `try_hold_lock`
(creating the lock file, `fsutil.rs` line 815) and removes it again.

### Views

`crates/apb-engine/src/trigger/view.rs`:

```rust
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
pub struct RuleView {
    pub name: String,
    pub enabled: bool,
    /// `inbox github/main [issues]`.
    pub source: String,
    pub playbook: String,
    pub version: String,
    pub scope: String,
    pub incarnation: String,
    pub seen_through: u64,
    pub starts_last_hour: u32,
    /// Approved for its current digest; an unapproved rule starts nothing.
    pub approved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub last: Option<LogEntry>,
    /// Filled by `apb_mcp::tools::trigger::list` from `approval_need`.
    pub needs_approval: bool,
    /// The sentence of `journal_refused_text` when the rule's journal is
    /// refused (D1 "The state and the trigger journal"); a listing reads
    /// each journal once.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub journal_error: Option<String>,
    /// `check_rule` now (the playbook or connector may have changed).
    pub issues: Vec<IssueView>,
}
/// Takes no clock: reads `Millis::now()` itself.
pub fn list(root: &Path) -> Result<Vec<RuleView>, EngineError>;
/// The doctor's lines about files, registration, leftovers and the sweep
/// (items 1, 2, 4 and 5 of "Doctor"); the approval lines come from the
/// tools layer.
pub fn doctor_line(root: &Path) -> Vec<(apb_core::doctor::CheckStatus, String)>;
```

`IssueView` is `crates/apb-core/src/validate/mod.rs` line 134;
`CheckStatus` is `apb_core::doctor` (`crates/apb-core/src/doctor/report.rs`
line 6, imported by `crates/apb-cli/src/doctor.rs` line 14).
`tools::trigger::list(root)` (no clock) wraps `view::list` and fills
`needs_approval`; every surface lists through it. The heartbeat of
"The sweep host" and the run's rule of "The run surfaces" live in the
same module.

### Commands

`crates/apb-cli/src/trigger.rs` (new), `Command::Trigger(crate::trigger::TriggerArgs)`
in `main.rs` next to `Health` (line 618) and its dispatch next to line
1376:

1. `apb trigger list [--json]`: name, enabled, source, playbook and
   version, starts in the last hour, the last verdict, "needs approval".
2. `apb trigger show <rule> [--json]`: the rule as YAML, its
   incarnation and `rule_digest`, the approval (`approved_at` printed as
   an ISO UTC time through `apb_core::dismiss::iso_utc`, as `apb trust`
   does), `seen_through`, a refused journal's sentence, the
   last 20 journal lines, each printed through
   `apb_engine::trigger::view::journal_line_text` (D1 "The state and the
   trigger journal").
3. `apb trigger set <rule> --file <path>`: a person's (D0 "A person's
   command": no run step, the rule's name typed at the terminal); refused `trigger_enabled_not_settable` when the file names
   `enabled`; the file's `name` must equal `<rule>`; refused
   `trigger_project_unregistered` in a project that is not registered
   (D1 "Where rules live" item 2); a rules file that does not verify is
   named with the remedy sentence of D1 "Where rules live" item 4 before
   it asks anything; prints every
   issue and exits 1 on an error before asking; prints "approval
   dropped" when an approved rule changed, and "needs approval: apb
   trigger approve <rule>" after every create or change: a rule starts
   nothing until it is approved.
4. `apb trigger approve <rule>`: a person's; prints the `ApprovalNeed`
   (its `rule_digest` included)
   and the line
   `Approving rule `<rule>` lets it start <playbook> <version> with nobody at the keyboard.`,
   then asks for the rule's name typed.
5. `apb trigger enable <rule>`, `apb trigger disable <rule>`: a
   person's, the name typed; the only way to change `enabled`; `enable`
   deletes the rule's state, so the next pass positions it at the head.
6. `apb trigger remove <rule>`: a person's, the name typed; removes the
   entry, its state and its journal file. There is no
   `--yes`. A `state_left` or `journal_left` of the removal (D1 "Where
   rules live" item 6.6) is printed as a warning; the command still
   succeeds.
7. `apb trigger test <rule> [--event <seq>] [--json]`: exit 0 when the
   event matches and passes the checks before start (it never prints
   "would start"; it lists what is checked at start), 3 when it does
   not, 2 on a usage error.
8. `apb trigger log <rule> [--limit N] [--json]`: per journal line,
   through the same `journal_line_text`: the event seq, the verdict
   words, the condition marks (`conditions #0 passed, #1 failed`), the
   param name, the V codes and the run id.

### MCP

`crates/apb-mcp/src/server/trigger.rs` (new,
`#[tool_router(router = trigger_router, vis = "pub(crate)")]`, added to
`tool_router` in `server/mod.rs` line 650):

1. `trigger_list {}`: `{ rules: [ { name, enabled, source, playbook, version, starts_last_hour, last, needs_approval } ] }`, read-only.
2. `trigger_test { rule, event_seq? }`: the `TestReport` without values.

Both are in `READ_ONLY` with `ReadOnlyUse::Full`
(`server/read_only.rs` line 28; the table test in `read_only_tests.rs`
checks it), with their args in `server/args.rs`, and `docs/MCP.md` gets
their rows. Their descriptions say a person writes and approves rules
(`apb trigger` or the dashboard).

### HTTP

`crates/apb-server/src/routes/triggers.rs` (new; `pub mod triggers;` in
`routes/mod.rs`; `.merge(routes::triggers::router())` in `lib.rs` next to
line 233):

1. `GET /api/triggers`, `GET /api/triggers/{name}` (with the journal
   tail), `GET /api/triggers/{name}/approval` (`ApprovalNeed`).
2. `PUT /api/triggers/{name}` with `{ "rule": <Rule> }` (a rule naming
   `enabled` is 422 `trigger_enabled_not_settable`),
   `POST /api/triggers/{name}/enable`, `POST /api/triggers/{name}/disable`
   and `DELETE /api/triggers/{name}` with `{}`, and
   `POST /api/triggers/{name}/approve` with `{ "shown": <ApprovalNeed> }`:
   only a signed-in session (`RequestChannel::Session`); a keyless
   dashboard or a bearer key is refused 403 `trigger_requires_session`.
   No route takes a passkey token. Each holds the rule's change and
   sweep locks (`trigger::hold_rule_lock`) across its write, inside
   `spawn_blocking`. Every body is a type of its own
   with `#[serde(deny_unknown_fields)]` (`SetBody`, `ApproveBody`,
   `EmptyBody`), and `ApprovalNeed` itself derives `Deserialize` with
   unknown fields denied (D1 "Approval"): a body with a field it does
   not name is 422. The `DELETE` answer carries `state_left` and
   `journal_left` when the removal left either behind (D1 "Where rules
   live" item 6.6). The routes store their times as `Millis`, like every
   rule write.
3. `POST /api/triggers/{name}/test` with `{ "event_seq": n }`: the
   report without values; allowed on every channel, refused from a run
   step (`trigger_test_refused_in_run_step`).
4. Every write refused 409 from a run step (`StepCaller::from_env()`, as
   `routes/discover.rs` line 120 does).

Every wire type of the trigger surfaces is registered in
`crates/apb-server/src/ts_contract.rs` (the rule document `Rule` and its
parts, `TriggerDetail`, `ApprovalNeed`, `TestReport` and its parts,
`LogEntry`, `Verdict`, `ConditionMark`, `ConditionResult`, `RuleView`,
`TriggeredBy`), so the web client takes them from the generated
`web/src/lib/api.gen.ts`, never from hand copies. The journal code words
are generated into `web/src/lib/consts.gen.ts` (`TRIGGER_CODES`,
`TRIGGER_CODE_WORDS`) from the engine's table
`apb_engine::trigger::view::CODE_WORDS`. There is no journal route of
its own: the journal tail comes with `GET /api/triggers/{name}`.

### Dashboard

Plain CSS over the existing CSS variables.

1. `web/src/pages/TriggerList.svelte` (new), route `#/triggers` in
   `web/src/App.svelte` (next to `#/discover`, line 84, lazily loaded
   like the discover page, line 47) and a `Triggers` entry in
   `web/src/lib/components/Topbar.svelte` (line 53, and the `active`
   union at line 40): the list; a rule's panel with its YAML in the
   existing `CodeEditor.svelte`, Save, the approval dialog (shows the
   `ApprovalNeed`, posts it back), Enable, Disable, Remove, Test (event
   seq field, the report's conditions) and the journal tail. Buttons
   that change a rule are disabled with the reason on a keyless dashboard;
   none asks for a passkey. A rule
   not approved for its digest shows "not approved: starts nothing". A
   rule whose journal is refused shows its `journal_error` sentence on its
   row. After a removal, a `state_left` and a `journal_left` are each
   shown as a warning; a removal that left nothing shows no warning.
2. `web/src/lib/triggers.ts` (new): the fetchers (over the generated
   types; it defines no wire type by hand) and the pure helpers
   `verdictWords(e: LogEntry): string` (the words of a code from
   `TRIGGER_CODE_WORDS`, never a stored sentence), `journalLineText(e:
   LogEntry): string` (the twin of `journal_line_text`, rendered by
   `web/src/lib/TriggerJournal.svelte`), `ruleSource(r): string` and
   `triggeredByLine(by: TriggeredBy): string`, tested in
   `web/src/lib/triggers.test.ts`.

### The run surfaces

`crates/apb-engine/src/trigger/view.rs` also reads a run's rule:

```rust
/// The rule that started the run at `run_dir`: its manifest's launch
/// record's `triggered_by` (read as `run_view::started_by`,
/// `run_view/started.rs` line 46, reads `started_by`); `None` for a run no
/// rule started and for one whose record lost the field.
pub fn triggered_by(run_dir: &Path) -> Option<TriggeredBy>;
/// "started by rule `triage-new-issues` (inbox github/main, event 42)".
pub fn triggered_by_line(by: &TriggeredBy) -> String;
```

Every surface renders it with `triggered_by_line` or, on the web, its
twin `triggeredByLine`:

1. `apb runs <id>` (`crates/apb-cli/src/run.rs` next to line 2250);
2. MCP `run_status` `triggered_by` (`crates/apb-mcp/src/tools/run.rs`
   next to line 399);
3. `RunDetail.triggered_by: Option<TriggeredBy>`
   (`crates/apb-server/src/routes/runs.rs` next to lines 253 and 640)
   and `web/src/lib/RunDriveNotes.svelte` (a `triggeredBy` prop next to
   `startedBy`, line 53, passed from `web/src/pages/RunView.svelte` line
   634);
4. the HTML report's meta `"triggered by"`
   (`crates/apb-engine/src/report_html/model.rs` lines 64 and 163,
   `sections.rs` line 169).

A run whose record lost `triggered_by` shows no rule. The run itself is
an ordinary unattended run: its journal has no trigger event and its
manifest no mark (D0 "An older apb and a rule's run").

### Doctor

`crates/apb-cli/src/doctor.rs` next to the health line (line 176), from
`apb_engine::trigger::view::doctor_line(root)` (items 1, 2, 4 and
5) and `apb_mcp::tools::trigger::list` (item 3):

1. WARN `trigger_rules_unreadable: <path> does not verify (changed outside apb, written by another installation or for another project); move it aside to write rules again` (`rules_unverified_text`, D1 "Where rules live" item 4) or `trigger state unreadable: <path>; remove it (rm <path>): every approved rule continues from the current head of its source`
   (a file that does not verify, was written for another project, or is
   a link, I5);
2. WARN `<n> enabled rule(s) and no trigger sweep runs: start apb ingest, or apb dashboard with ingest.enabled`
   when `view::status` is not `Live`, and WARN
   `rules in a project that is not registered: no listener sweeps it; see apb trigger set`
   when `projects::registered_id(root)` is `None`;
3. WARN `rule `x` needs approval: apb trigger approve x` for each;
4. WARN `<n> orphan file(s) of rules no longer in force (inert, decide nothing); delete: <paths>`
   for state entries no rule names and journal files whose incarnation
   no rule names (what a crashed `remove` or a source change leaves, D1
   "Where rules live" item 6), each with the path to delete;
5. OK `<n> rule(s), <m> enabled, sweep live` otherwise.

### Docs

1. New `docs/TRIGGERS.md`: what a rule is, the YAML with every field,
   the order of conditions and skip reasons; the event as the unit (a
   match starts now or is recorded; only a busy or still preparing start
   key, the hourly cap and a changed approval binding wait for the next
   pass); the start key: the author's template is
   the dedupe (an event id for once per event, an entity id for once per
   entity), it lives `runs.start_key_days` exactly as any start key, a
   failed run holds its key until a person's `--start-key-reuse`, and a
   delivery of the key after it expired starts a new run; runs at once
   are the playbook's `concurrency:` block; a rule's run is always
   unattended, so a gate that needs a person fails it with
   `hitl_required` and it never counts toward probation; a long timer
   wait in a rule's run keeps its driver, as for any unattended run, and
   lifting this is future work; that a rule needs `allow` in the
   `billing:` section of `policy.yaml` for the basis of its executors (a
   billing `ask` needs a person's confirmation a rule cannot give:
   `billing_refused_for_trigger`); approval: every rule needs one to
   start anything, it covers the rule's digest and one binding over the
   permit digest of its target, the launch policy in force and the
   billing decision; for an `ask` policy the approval itself is the
   confirmation; any change of the rule drops it; a rule never starts a
   run under a binding other than the approved one: a start attempted
   under a changed binding (a stricter policy included) revokes the
   approval for good, while a change restored before any start was
   attempted revokes nothing; and a change of the
   rule's connector or account makes it a new rule; the boundary of an
   approval, once and plainly (I1): like main's launch floors and signed
   stores it guards against mistakes and against agents acting through
   apb's surfaces, not against another process of the same user that
   writes the signed file, and binding an approval to a passkey is future
   work; that `approve` and `enable` take effect from the head the next
   pass reads, so events that arrive within one sweep period of the
   command may be skipped; the hourly cap, counted from the runs' own
   launch records (at most 500, refused above), and conservatively: a run
   of the last hour with a manifest whose origin or rule cannot be
   established counts toward the cap of every rule of the project, while
   a stamped directory with no manifest counts for none; that a rule is
   saved only in a registered project; that `enabled` is changed only by
   `enable` and `disable`; that a rule takes no inbox consumer and records
   a gap when retention removed events it had not seen; that the journal
   holds codes, condition indexes and pass or fail, never values,
   literals or sentences; where the sweep runs (`apb ingest`, or `apb
   dashboard` with `ingest.enabled`, and once in `apb resume --all`,
   never inside a run step; two hosts never double-start); `apb trigger`
   commands, the journal and `apb trigger test` (what it checks, and the
   four checks it names as "checked at start"); that an older apb sees a
   rule's run as an ordinary unattended run; the codes V260 to V267 (and
   why `on_no_estimate: continue` fits a rule whose runs are external
   data, V267); a code example and a non-code example; and "Time-based
   starts": not in 1.0, an outside timer calling `apb run --start-key`.
2. `docs/CONNECTORS.md` line 454: "An inbound delivery starts a run only
   through a start rule a person wrote (docs/TRIGGERS.md); otherwise a
   playbook consumes the inbox by polling it ...".
3. `docs/CI.md` line 455: start rules answer event-driven runs; a
   time-based start is an outside scheduler calling `apb run
   --unattended --start-key <slot> --detach`, paired with `apb wait` as
   below.
4. `docs/GUARDRAILS.md` "Launch policy for agents" (line 423): a `rule`
   column in the decision table (`allow` runs, `ask` the rule's
   approval, `deny` refused), one sentence that a rule is changed and
   approved like a person's command (terminal with the typed name,
   signed-in dashboard) with I1's boundary, and "Billed use" (line 523):
   a rule never confirms it.
5. `docs/EXTERNAL-DATA.md`: a rule's runs are flagged unless the rule
   says `external_data: false`.
6. `docs/MCP.md`: the two tool rows.

`docs/DEPLOYMENT.md` does not change: `docs/TRIGGERS.md` says where the
sweep runs.

### Tests

1. Unit, `crates/apb-core/src/trigger/`:
   1. `rule.rs`: `a_rule_round_trips_through_yaml`,
      `an_unknown_field_is_refused` (an `attended` field included),
      `set_refuses_a_document_naming_enabled` (with `enabled: true` and
      with `enabled: false`, on a new and on an existing rule:
      `EnabledNotSettable`, nothing written),
      `each_creation_gets_its_own_incarnation`,
      `rule_digest_covers_every_firing_field` (changing each field
      changes it; key order does not),
      `a_changed_rule_drops_its_approval`,
      `an_unchanged_set_keeps_its_approval`, `a_new_rule_is_not_approved`,
      `approve_and_enable_delete_the_state_before_the_rules_write` (a
      fault after the deletion: no state and an unapproved or disabled
      rule; no inbox read by either command),
      `a_source_change_gives_a_new_incarnation` (approval dropped; a
      params edit keeps the incarnation),
      `an_approval_carries_the_binding`,
      `revoke_clears_only_the_stale_approval` (another incarnation,
      digest or approved binding: left as it is),
      `a_disable_then_set_keeps_the_rule_disabled`,
      `a_mutation_waits_for_the_rule_locks`,
      `an_unregistered_project_cannot_save_a_rule`,
      `a_rules_file_of_another_project_is_refused`,
      `a_linked_rules_file_is_refused`,
      `remove_deletes_entry_state_and_journal` (three assertions: the
      entry is gone from `rules.json`, the state entry is gone from
      `state.json`, the journal file is gone);
   2. `render.rs`: `a_template_renders_body_paths_and_the_event`,
      `an_unknown_placeholder_does_not_parse`, `an_object_is_not_scalar`,
      `a_missing_path_and_an_empty_string_differ`,
      `reads_event_text_sees_the_type`;
   3. `validate.rs`, one test per code: `v260_a_connector_without_webhook`,
      `v260_types_without_an_event_type`, `v260_a_long_name`,
      `v261_an_unknown_version`, `v261_a_draft_target`,
      `v261_a_retired_target`, `v262_a_body_value_into_a_plain_param`
      (and its pass with `untrusted: true`),
      `v262_a_pattern_alone_does_not_pass`, `v262_a_secret_param`,
      `v262_an_unmapped_required_param`, `v263_no_budget`,
      `v263_on_no_estimate_ask`, `v264_limits_out_of_range`,
      `v265_a_write_without_a_marker`, `v266_external_data_off`,
      `v267_pause_with_external_data`;
   4. `evaluate.rs`: `conditions_decide_in_order`,
      `own_send_needs_a_marker_of_this_installation_where_the_rule_reads`,
      `a_long_key_is_invalid`, `the_start_key_names_the_incarnation`,
      `a_skip_names_the_rules_path_never_the_value`;
   5. `state.rs`: `position_at_head_replaces_another_incarnation`,
      `position_at_head_writes_the_line_first`,
      `needs_position_for_a_missing_or_foreign_state`,
      `a_state_file_of_another_project_is_refused`,
      `the_journal_keeps_the_newest_thousand`, `a_torn_line_is_skipped`,
      `an_unknown_verdict_is_skipped`, `append_skips_ids_already_there`,
      `read_drops_a_repeated_id_and_folds_already_started`,
      `read_shows_the_last_position_attempt`,
      `a_code_outside_the_closed_set_is_refused_other`,
      `a_journal_line_holds_condition_marks_only` (a `where` condition
      with an `equals` literal and an event type: the stored line has
      indexes and pass or fail, neither the literal nor the type),
      `a_failed_sync_returns_an_error_before_anything_else` (a
      `cfg(test)` fault point, the pattern of
      `versioning/current/fault.rs` lines 3 and 20),
      `the_first_line_is_durable_before_the_state` (a `cfg(test)`
      recorder of the sync calls: for a first line, the file, `log/` and
      `.apb/trigger-state` are synced, in that order, before the state
      transaction runs),
      `binding_lines_differ_for_allow_and_deny` (unit in
      `gate/billing.rs`: the same prediction under `allow` and under
      `deny` gives different lines, where `nonce_lines` gives none for
      both),
      `a_torn_tail_is_repaired_before_the_next_append`;
      `crates/apb-core/src/fsutil.rs`
      `repair_line_tail_keeps_a_whole_tail_and_cuts_a_partial_one`, and
      the review channel's torn-tail tests (`review.rs` lines 1620 to
      1670) unchanged over the moved helper.
2. Unit elsewhere: `marker.rs`
   `trailing_marker_reads_only_the_exact_trailing_marker`;
   `start_key.rs` (core) `key_tag_if_keyed_creates_no_key`; `fsutil.rs`
   `flock_held_creates_nothing`; `start_key.rs` (engine)
   `peek_writes_nothing`; `connector/inbox.rs`
   `head_seq_is_zero_for_an_empty_log`; `workspace.rs` the extended
   `the_enforcement_state_directories_are_local_entries`;
   `trigger/view.rs` `a_beat_reads_live_and_an_old_one_stale`,
   `doctor_line_names_an_unverified_state`,
   `doctor_line_names_an_unregistered_project`,
   `doctor_line_names_leftovers_of_a_crashed_remove`,
   `triggered_by_line_names_the_rule_and_event`,
   `verdict_words_never_repeat_a_stored_text`; `trigger_sweep.rs`
   `the_period_defaults_to_five_seconds`, `spawn_runs_once_per_process`.
3. Unit, `gate/launch.rs` `mod tests`:
   `a_rule_start_is_decided_like_a_cli_without_a_terminal` (allow runs,
   ask refused `launch_requires_rule_approval`, deny refused),
   `an_equal_approved_binding_confirms_an_ask_launch`,
   `a_deny_playbook_is_refused_even_with_an_approval`,
   `a_rule_billing_ask_writes_nothing`, `a_rule_grant_records_its_rule`,
   `a_child_grant_records_no_rule`; unit, `gate/mod.rs`:
   `the_binding_is_compared_in_admission` (a mismatch returns
   `binding_changed` with the resolved binding before `launch::decide`
   runs: no challenge, no grant).
4. Integration, `crates/apb-engine/tests/suite/trigger_test.rs` (new),
   with a fixture inbox under the test's config dir, the fixture project
   registered with `apb_core::projects::bind_id`, every rule approved by
   the test unless it says otherwise, and two start stand-ins: a
   recording one (`Started` for a made-up id, or a scripted outcome) and
   the real keyed start (the engine's gate with `Initiator::Trigger`,
   `apb_engine::start_key::lookup`, then `apb_engine::start_detached` of a
   one-node playbook with `RunOptions.start_key`):
   1. `a_matching_event_starts_once`;
   2. `a_resent_delivery_with_a_new_id_starts_once` (real start);
   3. `an_entity_key_starts_once_per_entity` (real start);
   4. `the_hourly_cap_holds_the_position` (real start; the journal
      emptied between passes changes nothing);
   5. `the_first_pass_after_approval_only_positions` (approved, an
      event stored before the next pass: that pass writes one position
      line at the head it reads and starts nothing; an event after it
      starts);
   6. `a_removed_state_restarts_nothing_eight_days_later`;
   7. `a_second_sweeper_skips_a_held_rule`;
   8. `a_crash_between_start_and_line_starts_once` (real start);
   9. `a_start_lost_to_a_crash_and_retention_still_counts` (real start);
   10. `a_crash_between_line_and_transaction_repeats_the_event_once`;
   11. `a_journal_write_failure_leaves_the_position`;
   12. `a_busy_start_key_is_tried_again_next_pass`;
   13. `a_refusal_is_the_events_verdict`;
   14. `a_busy_concurrency_key_leaves_no_value_in_the_journal`;
   15. `an_unverified_state_starts_nothing`;
   16. `a_gap_is_logged_once_and_the_pass_goes_on`;
   17. `a_rule_registers_no_consumer`;
   18. `an_unapproved_rule_starts_nothing`;
   19. `a_reenabled_rule_starts_nothing_that_arrived_while_disabled`;
   20. `a_source_change_never_reuses_a_line_id` (the old line and id
       kept before the change; afterwards a new incarnation, a distinct
       id; the old incarnation's journal file stays, inert, and the doctor
       line lists it with its path);
   21. `a_changed_permit_revokes_the_approval` (allow and ask, A to B to
       A across a restart, a matching event attempted at B);
   21b. `a_restored_binding_with_no_start_keeps_the_approval` (A to B to
        A with only an event the start key answers `already_started`
        while at B: nothing revoked, the next new key starts under A);
   21a. `a_billing_deny_revokes_the_approval` (approved with billing
        `allow`; `policy.yaml` switched to `deny`: the next event starts
        nothing and the approval is cleared; switched back to `allow`: the
        approval does not come back);
   22. `a_crash_before_the_revocation_is_completed_or_moot`;
   23. `a_revocation_to_a_binding_seen_before_is_logged`;
   24a. `an_unreadable_runs_directory_holds_the_cap` (`.apb/runs`
        made unreadable: the next matching event is held
        `rate_cap_unknown` once over two passes and starts once the
        directory reads again);
   24. `an_unattributable_run_counts_toward_the_cap` (a manifest whose
       stamp does not verify, as in the `manifest::amend` window);
   25. `a_lost_launch_record_counts_toward_every_cap` (initiator
       `trigger:<rule>` without `triggered_by`);
   26. `a_stamped_directory_without_manifest_does_not_count`;
   27. `a_failed_hand_over_counts_at_once`;
   28. `a_disable_during_a_pass_starts_nothing_after_it`;
   29. `leftovers_of_a_crashed_remove_are_inert`;
   30. `a_rule_start_records_its_rule` (real start: `initiator:
       "trigger:<rule>"` and `triggered_by` with the incarnation; the
       manifest carries no mark);
   31. `a_rule_start_is_unattended` (real start: `manifest.unattended` is
       true; a playbook gate that needs a person fails it
       `hitl_required`; a long timer wait keeps the driver, no
       `run_paused`);
   32. `the_state_lock_is_free_while_starting`;
   33. `a_rule_run_earns_no_probation_credit` (real start,
       `external_data: false`, with a sub-playbook: both entries
       `unattended: true`, `counted` stays 0);
   34. `a_default_rule_run_writes_no_probation_entry` (real start).
5. Integration, `crates/apb-mcp/tests/suite/trigger_tools_test.rs` (new):
   `trigger_test_explains_a_skipped_event`, `trigger_test_shows_no_values`,
   `trigger_test_writes_nothing`, `trigger_test_works_in_read_only_mode`,
   `trigger_test_is_refused_in_a_run_step`,
   `trigger_test_reports_every_check_in_order` (twelve lines; an
   unapproved rule; a draft target; a param that does not render; a
   changed binding, the rule still approved after the test),
   `trigger_test_names_the_checks_at_start`,
   `trigger_test_reports_a_used_key`, `trigger_list_lists_rules`,
   `the_fire_refuses_an_untrusted_playbook`,
   `a_stale_approval_is_refused`,
   `approve_refuses_a_need_of_another_rule_digest`,
   `approve_refuses_a_changed_permit`,
   `a_changed_permit_is_caught_in_admission` (allow and ask; restored,
   the approval does not come back),
   `a_change_after_admission_fails_preparation` (held at the existing
   `Hook::KeyAdmitted`),
   `an_ask_rule_starts_without_a_launch_nonce`,
   `a_billing_ask_refuses_a_rule_start`,
   `a_stricter_policy_revokes_the_approval`,
   `a_changed_billing_decision_revokes_the_approval`,
   `a_pattern_refusal_names_the_param`,
   `the_fire_refuses_a_param_that_misses_its_pattern`,
   `an_unattended_fire_is_refused_by_probation`,
   `a_refuse_concurrency_block_refuses_the_fire`,
   `a_busy_tree_queues_the_start`,
   `a_key_conflict_is_recorded_as_already_started`.
6. Integration, `crates/apb-cli/tests/suite/trigger_cli_test.rs` (new):
   `set_refuses_without_a_terminal`, `set_refuses_in_a_run_step`,
   `disable_refuses_without_a_terminal`,
   `approve_refuses_without_a_terminal`,
   `set_needs_the_name_typed` (a pseudo terminal as `init_on_tty` in
   `crates/apb-cli/tests/init_interactive_test.rs` line 204 gives one),
   `set_refuses_an_unregistered_project`,
   `set_refuses_enabled_in_the_file`, `set_prints_every_issue`,
   `test_exits_three_for_a_skipped_event`,
   `remove_deletes_entry_state_and_journal`,
   `a_delivery_starts_a_run_end_to_end` (`apb ingest` with
   `APB_TRIGGER_SWEEP_MS=200`, an approved rule positioned by a first
   pass, a signed delivery, `apb wait <run> --timeout 60`, `apb runs
   <run>` prints "started by rule"),
   `resume_all_sweeps_new_events`,
   `resume_all_in_a_run_step_sweeps_nothing`,
   `doctor_warns_without_a_sweep`, `docs_examples_validate`.
7. Server, `crates/apb-server/tests/suite/triggers_api_test.rs` (new):
   `writes_need_a_session`, `approve_refuses_a_changed_need`,
   `approve_refuses_an_unknown_field`, `set_refuses_enabled`,
   `trigger_routes_run_blocking_work_off_the_runtime`,
   `the_run_detail_names_its_rule`.
8. Web: `verdictWords names every code` and `triggeredByLine names the
   rule and the event` (`triggers.test.ts`), `RunDriveNotes names the
   rule that started the run` (`RunDriveNotes.test.ts`).
9. The implementation review's tests, each failing before its fix (a
   seam that fails or observes a sync is compiled out of a release
   build):
   1. Durability: a first positioning whose directory sync fails, then
      a retry with another line id, syncs the directory before the
      position commits; a compaction followed by a fresh append synced
      `log/` after the rename (the seam on the walked `sync_dir`); an
      append syncs the descriptor of the directory it opened, so a
      directory swapped in at `log` after the open is not the one
      synced.
   2. The anchor: `trigger-state/log` replaced by a link to a directory
      holding `<incarnation>.jsonl`: a removal leaves that file in place
      and answers `journal_left`, and an append and a compaction are
      refused; a link at a parent component gets the component remedy,
      a link at the journal the journal remedy.
   3. A journal over its cap: no start, no position move and no growth
      across two passes, while another rule of the project still fires;
      a tail repair above the cap is refused.
   4. `journal_write`: the journal made read-only, so its ids read and
      the append fails: the pass answers `journal_write`, the position
      stays and nothing after the event is evaluated, for an event that
      started a run, a held one and a `rate_cap` hold.
   5. The store cap: rules saved up to the cap, the next save and an
      approval that would cross it refused `rules_store_full` with the
      file unchanged, every earlier rule still read, swept, approved,
      disabled and removed; a document of exactly the cap accepted, one
      byte more refused.
   6. Times: `apb trigger set`, `enable`, `disable` and an approval
      through their real entry points, and the HTTP `set` and `enabled`
      routes, store every time between a `now_ms_u64()` taken before
      and one taken after; the printed approval time is within a minute
      of now.
   7. Removal: `state_left` and `journal_left` from the core answer,
      the CLI warning, the HTTP answer and the dashboard's toast.
   8. The `claim_answer` race: two claims of one key ordered by a
      barrier keyed by root (`hold_after_lookup`, beside
      `hold_after_attach`).
   9. A rule listing reads each journal once.
   10. The journal corpus test restores read, write and search on its
       temp project's read-only scripts before the directory is dropped,
       and its `Case::drop` never panics.
   11. A rule whose target is a global playbook that shares its id with
       a project playbook starts the global playbook, its run names the
       global origin, and its start key lives in the global scope, so
       the next event of the same key answers the run it started.

### Wrong implementations

1. Deduping on the provider id only. Caught by
   `a_resent_delivery_with_a_new_id_starts_once`.
2. Keeping a match the cap holds in a queue and moving `seen_through`
   past it. Caught by `the_hourly_cap_holds_the_position`.
3. Moving `seen_through` past a `start_key_busy` answer. Caught by
   `a_busy_start_key_is_tried_again_next_pass`.
4. Retrying a refusal on every pass instead of recording it. Caught by
   `a_refusal_is_the_events_verdict`.
5. Moving `seen_through` before the event's line is synced, or after a
   failed write. Caught by `a_journal_write_failure_leaves_the_position`
   and `a_failed_sync_returns_an_error_before_anything_else`.
6. Appending a repeated line, or a reader that shows a repeat twice.
   Caught by `a_crash_between_line_and_transaction_repeats_the_event_once`
   and `read_drops_a_repeated_id_and_folds_already_started`.
7. Copying an engine refusal's sentence, a condition literal or an event
   value into the journal. Caught by
   `a_busy_concurrency_key_leaves_no_value_in_the_journal` and
   `a_journal_line_holds_condition_marks_only`.
8. Starting inside the state transaction. Caught by
   `the_state_lock_is_free_while_starting`.
9. Storing rendered param values in the state or the journal. Caught by
   `trigger_test_shows_no_values` and an `rg` over the state and journal
   files in `a_matching_event_starts_once`.
10. Passing `acknowledge_untrusted: true` because "a person wrote the
    rule". Caught by `the_fire_refuses_an_untrusted_playbook`.
11. Treating an approval as valid after the rule or its binding changed,
    or comparing anywhere but in `Gate::admit` before `launch::decide`.
    Caught by `a_changed_rule_drops_its_approval`,
    `approve_refuses_a_changed_need`, `the_binding_is_compared_in_admission`,
    `a_changed_permit_is_caught_in_admission` and
    `a_stricter_policy_revokes_the_approval` and
    `a_billing_deny_revokes_the_approval`.
12. Keeping an approval after its binding changed, so restoring the old
    target revives it. Caught by `a_changed_permit_revokes_the_approval`.
13. Sweeping a rule that is not approved for its digest. Caught by
    `an_unapproved_rule_starts_nothing`.
14. Letting `set` change `enabled`. Caught by
    `set_refuses_a_document_naming_enabled` and
    `a_disable_then_set_keeps_the_rule_disabled`.
15. Reading the inbox head in `approve` or `enable`, or committing the
    rules change before deleting the state. Caught by
    `approve_and_enable_delete_the_state_before_the_rules_write` and
    `the_first_pass_after_approval_only_positions`.
16. Positioning a missing state at an older seq. Caught by
    `a_removed_state_restarts_nothing_eight_days_later`.
17. Counting the hourly cap through an enumeration that answers nothing
    when a directory cannot be read (caught by
    `an_unreadable_runs_directory_holds_the_cap`), or from anything but
    the runs' launch records,
    skipping a run with a manifest that cannot be attributed, or counting
    a directory without a manifest. Caught by
    `a_start_lost_to_a_crash_and_retention_still_counts`,
    `an_unattributable_run_counts_toward_the_cap`,
    `a_lost_launch_record_counts_toward_every_cap` and
    `a_stamped_directory_without_manifest_does_not_count`.
18. Calling `check_probation` or `fit::assess` from the fire instead of
    setting `RunOptions`. Caught by
    `an_unattended_fire_is_refused_by_probation`.
19. Starting a rule's run attended, or with an `attended` field read
    from anywhere. Caught by `a_rule_start_is_unattended` and
    `an_unknown_field_is_refused`.
20. A dry run that says "would start", or runs a preparation check of
    its own. Caught by `trigger_test_names_the_checks_at_start`.
21. Keying the state, the journal or the start key by the rule's name.
    Caught by `leftovers_of_a_crashed_remove_are_inert` and
    `each_creation_gets_its_own_incarnation`.
22. Registering an inbox consumer for a rule. Caught by
    `a_rule_registers_no_consumer`.
23. A pass that starts without probing the change lock or re-reading the
    entry. Caught by `a_disable_during_a_pass_starts_nothing_after_it`.
24. Sweeping from a run step. Caught by
    `resume_all_in_a_run_step_sweeps_nothing`.
25. Calling `keyed_lookup` or `key_tag` from the dry fire. Caught by
    `trigger_test_writes_nothing` and `trigger_test_works_in_read_only_mode`.
26. Accepting a `pattern` in place of `untrusted: true` for event text.
    Caught by `v262_a_pattern_alone_does_not_pass`.
27. Leaving `SignedDoc::belongs_to` and `NO_LINKS` at their defaults.
    Caught by `a_rules_file_of_another_project_is_refused` and
    `a_linked_rules_file_is_refused`.
28. Rendering through `scalar_at_path`. Caught by
    `a_missing_path_and_an_empty_string_differ`.
29. Leaving `workdir_queue_wait` as `start_options` sets it. Caught by
    `a_busy_tree_queues_the_start`.
30. Saving a rule in a project with no registered id. Caught by
    `an_unregistered_project_cannot_save_a_rule`.
31. Matching `own_send` on a substring or a field the rule does not
    read. Caught by
    `own_send_needs_a_marker_of_this_installation_where_the_rule_reads`.
32. Keeping the incarnation across a source change. Caught by
    `a_source_change_never_reuses_a_line_id`.
33. Appending after a torn record without repairing it. Caught by
    `a_torn_tail_is_repaired_before_the_next_append`.

## Code map

New files (none exists on efae5822; checked):

| # | File | What |
|---|---|---|
| 1 | `crates/apb-core/src/trigger/mod.rs` | module |
| 2 | `crates/apb-core/src/trigger/rule.rs` | rule, store, incarnation, `rule_digest`, locked writes |
| 3 | `crates/apb-core/src/trigger/render.rs` | templates |
| 4 | `crates/apb-core/src/trigger/validate.rs` | V260 to V267 |
| 5 | `crates/apb-core/src/trigger/state.rs` | state, `position_at_head`, trigger journal with line ids |
| 6 | `crates/apb-core/src/trigger/evaluate.rs` | one event |
| 7 | `crates/apb-engine/src/trigger/mod.rs` | sweep, `FireRequest`, `FireOutcome`, `starts_in_last_hour`, `run_dirs_strict`, `own_run` |
| 8 | `crates/apb-engine/src/trigger/view.rs` | `RuleView`, list, `doctor_line`, heartbeat, `triggered_by`, verdict words, `CODE_WORDS`, `journal_line_text` |
| 9 | `crates/apb-mcp/src/tools/trigger.rs` | fire, approval, test, `sweep_root`, list |
| 10 | `crates/apb-mcp/src/server/trigger.rs` | MCP tools |
| 11 | `crates/apb-cli/src/trigger.rs` | `apb trigger`, typed confirmation |
| 12 | `crates/apb-server/src/trigger_sweep.rs` | sweep host |
| 13 | `crates/apb-server/src/routes/triggers.rs` | HTTP, session checks, body types, `spawn_blocking` |
| 14 | `web/src/pages/TriggerList.svelte` | dashboard page |
| 15 | `web/src/lib/triggers.ts` | fetchers over the generated types, helpers, `journalLineText` |
| 16 | `web/src/lib/triggers.test.ts` | web tests |
| 17 | `docs/TRIGGERS.md` | the doc |
| 18 | `crates/apb-engine/tests/suite/trigger_test.rs` | engine tests |
| 19 | `crates/apb-mcp/tests/suite/trigger_tools_test.rs` | MCP tests |
| 20 | `crates/apb-cli/tests/suite/trigger_cli_test.rs` | CLI tests |
| 21 | `crates/apb-server/tests/suite/triggers_api_test.rs` | server tests |
| 22 | `docs/superpowers/specs/2026-11-20-start-rules-design.md` | public record |

Changed files:

| # | File | What |
|---|---|---|
| 23 | `crates/apb-core/src/lib.rs` | `pub mod trigger;` |
| 24 | `crates/apb-core/src/connector/inbox.rs` | `head_seq` |
| 25 | `crates/apb-core/src/connector/marker.rs` | `trailing_marker` |
| 26 | `crates/apb-core/src/launch.rs` | `TriggeredBy`, `LaunchRecord.triggered_by` |
| 27 | `crates/apb-core/src/workspace.rs` | `LOCAL_ENTRIES` and its test |
| 29 | `crates/apb-core/src/start_key.rs` | `key_tag_if_keyed`, `StartRequest::capture_if_keyed` |
| 30 | `crates/apb-core/src/fsutil.rs` | `flock_held`, `repair_line_tail`, `repair_line_tail_file`, `open_read_append_private_in` |
| 31 | `crates/apb-engine/src/lib.rs` | `pub mod trigger;` |
| 32 | `crates/apb-engine/src/gate/launch.rs` | `Initiator::Trigger`, `LaunchContext.approved_binding`, `TRIGGER_POLICY`, `BINDING_CHANGED`, arms, `approval_binding`, `binding_changed_of` |
| 33 | `crates/apb-engine/src/gate/billing.rs` | `BILLING_TRIGGER`, `binding_lines` |
| 33a | `crates/apb-engine/src/gate/mod.rs` | the approval binding compared in `Gate::admit` |
| 40 | `crates/apb-engine/src/start_key.rs` | `peek` |
| 41 | `crates/apb-engine/src/report_html/model.rs` | "triggered by" |
| 42 | `crates/apb-engine/src/report_html/sections.rs` | "triggered by" |
| 43 | `crates/apb-mcp/src/tools/mod.rs` | `pub mod trigger;` |
| 44 | `crates/apb-mcp/src/tools/run.rs` | `run_status.triggered_by`, `keyed_lookup_holding`, `keyed_peek` |
| 45 | `crates/apb-mcp/src/server/mod.rs` | `mod trigger;`, `trigger_router` |
| 46 | `crates/apb-mcp/src/server/args.rs` | `TriggerTestArgs` |
| 47 | `crates/apb-mcp/src/server/read_only.rs` | two `READ_ONLY` rows |
| 48 | `crates/apb-cli/src/main.rs` | `Command::Trigger` |
| 49 | `crates/apb-cli/src/run.rs` | "started by rule" line, the pass in `resume_all_cmd` |
| 50 | `crates/apb-cli/src/doctor.rs` | trigger lines |
| 51 | `crates/apb-server/src/lib.rs` | `mod trigger_sweep;`, the routes |
| 52 | `crates/apb-server/src/ingest.rs` | `spawn`, `nudge` |
| 53 | `crates/apb-server/src/routes/mod.rs` | `pub mod triggers;` |
| 54 | `crates/apb-server/src/routes/runs.rs` | `RunDetail.triggered_by` |
| 56 | `crates/apb-server/src/ts_contract.rs` | new TypeScript types |
| 57 | `web/src/lib/api.gen.ts` | regenerated |
| 58 | `web/src/App.svelte` | the route |
| 59 | `web/src/lib/components/Topbar.svelte` | the entry |
| 60 | `web/src/lib/RunDriveNotes.svelte` | `triggeredBy` |
| 61 | `web/src/lib/RunDriveNotes.test.ts` | its test |
| 62 | `web/src/pages/RunView.svelte` | passes `triggeredBy` |
| 65 | `docs/CONNECTORS.md` | line 454 |
| 66 | `docs/CI.md` | line 455 |
| 67 | `docs/GUARDRAILS.md` | launch policy and billing |
| 68 | `docs/EXTERNAL-DATA.md` | rule runs flagged |
| 69 | `docs/MCP.md` | two tools |
| 70 | `crates/apb-engine/tests/main.rs` | test module |
| 71 | `crates/apb-mcp/tests/main.rs` | test module |
| 72 | `crates/apb-cli/tests/main.rs` | test module |
| 73 | `crates/apb-server/tests/main.rs` | test module |
| 74 | `CLAUDE.md` | code map |
| 75 | `AGENTS.md` | code map |
| 77 | `crates/apb-engine/src/review.rs` | `repair_tail` calls `fsutil::repair_line_tail` |
| 78 | `crates/apb-core/src/fsutil/beneath.rs` | `OpenedAppend`, `open_read_append`, `sync_dir`, `write_new`, `remove`, `LinkAt` (the implementation review) |
| 79 | `crates/apb-core/src/signed_store.rs` | the write refused over the reader's cap, `StoreError::TooLarge` |
| 80 | `crates/apb-mcp-client/src/session.rs`, `tap.rs`, `testing.rs` | the close's drain, `count_drained`, the stub's end-of-input and orphan modes |

Records of this change: the proposal, the design, the task order and the spec delta, 4 in all (the `wait-park` delta is removed in the tenth pass); this document is the proposal and the design as one public record.

Total: 22 new, 45 changed, 4 records: 71 files (84 after the ninth
pass). The cuts are listed in the "Cut in the ..." sections.

Unchanged and named because earlier passes changed them:
`crates/apb-engine/src/manifest.rs`, `scheduler/prepare.rs`,
`scheduler.rs`, `scheduler/deferred.rs`, `scheduler/wait_park.rs`,
`scheduler/unattended.rs`, `next_action/unattended.rs`,
`crates/apb-engine/src/probation.rs`, `review/passkey_proof.rs`,
`crates/apb-server/src/routes/passkeys.rs`, `web/src/lib/passkeys.ts`,
`crates/apb-cli/tests/suite/journal_corpus_test.rs`,
`crates/apb-core/src/validate/guarded.rs`, `crates/apb-engine/src/event.rs`
and `deadline_hooks.rs`. Also unchanged: `run_view.rs` and
`run_view/started.rs`, `crates/apb-core/src/probation.rs`,
`crates/apb-engine/tests/suite/wait_test.rs`, `crates/apb-cli/src/consent.rs`
(`ask_typed` is used as it is), `docs/HOWTO-authoring.md`,
`docs/DEPLOYMENT.md` and every dependency
file (`Cargo.toml`, `Cargo.lock`, `THIRD-PARTY-NOTICES.txt`): no new
dependency. `web/src/lib/consts.gen.ts` is regenerated with the trigger
code words (`TRIGGER_CODES`, `TRIGGER_CODE_WORDS`).

Integration test files are declared in the crate's `tests/main.rs` with
`#[path = "suite/<name>.rs"] mod <name>;` in alphabetical place (as
`crates/apb-engine/tests/main.rs` line 264 declares
`connector_contract_inbox`).

## Release notes

The change is not tagged here. List in the final report:

1. Start rules: `apb trigger` (`set`, `list`, `show`, `approve`,
   `enable`, `disable`, `remove`, `log`, `test`), the dashboard's
   Triggers page, MCP `trigger_list` and `trigger_test`; nothing starts
   without a rule a person wrote and approved.
2. New refusals: `launch_requires_rule_approval`,
   `billing_refused_for_trigger`, `trigger_budget_required`,
   `trigger_requires_session`, `trigger_project_unregistered`,
   `trigger_approval_changed`, `trigger_enabled_not_settable`,
   `trigger_test_refused_in_run_step`.
3. A rule is changed and approved only by a person: at a terminal with
   its name typed, or in a signed-in dashboard. Every rule needs an
   approval to start anything; any change of the rule drops it, and a
   start attempted under a changed approval binding (permit, launch
   policy, billing) revokes it, so no run starts under another binding.
   A rule is saved only in a registered project.
4. Each matching event starts its run at once or is recorded with its
   reason; the rule's key is the start key, so one key starts one run
   while it lives (`runs.start_key_days`). Runs at once are the
   playbook's `concurrency:` block; a rule caps starts per hour.
5. A rule's run is always unattended and keeps its driver on a long
   wait, as any unattended run; an older apb sees it as an ordinary
   unattended run.
6. The sweep runs in the ingest listener (`apb ingest`, or `apb
   dashboard` with `ingest.enabled`) and once in `apb resume --all`;
   it never runs inside a run step, and `apb doctor` warns when enabled
   rules exist and no sweep runs.
7. A rule's run names its rule (`triggered_by`) on `apb runs <id>`, MCP
   `run_status`, the dashboard and the HTML report.
8. Time-based starts are not in this release: an outside timer calling
   `apb run --start-key` is the way.

## Cut in the third pass

The second review found crash and race windows in machinery this change
owned. Instead of patching each window, the machinery was cut so that
the engine's existing paths carry it:

1. The pending queue and every held fire (`PendingFire`, `MAX_PENDING`,
   `full_at`, `held_logged`, the three-attempt retry, `due_at`, the
   verdicts `matched`, `coalesced` and `event_gone`, `Inbox::event`, the
   tests `a_full_queue_holds_the_cursor`,
   `the_consumer_cursor_stops_before_pending_events`,
   `a_reenabled_rule_drops_its_old_pending_fires`). Why: a held fire was
   a second position beside `seen_through`, with its own crash windows
   and its own cap arithmetic. Now a match starts now or is the event's
   verdict (I2); the only things that wait are `start_key_busy` and the
   hourly cap, and both wait by not moving `seen_through`.
2. The rule's concurrent cap and its `active` list (`max_concurrent`,
   `MAX_CONCURRENT_MAX`, the `checked_run` sweep over `active`, the
   test `the_concurrent_cap_frees_when_a_run_ends`). Why: it counted
   runs the engine already counts, drifted from them on a target edit,
   and could be exceeded inside one pass. Runs at once are the
   playbook's `concurrency:` block, unchanged from main, admitted under
   its own transaction (`concurrency::admit`).
3. Coalescing windows (`coalesce`, `COALESCE_MAX_SECS`, `coalesce_ms`,
   `first_received_at`, `events`, fire ids, `TriggeredBy.events`,
   `TriggeredBy.fire`, the tests
   `a_burst_within_the_window_fires_once_with_the_newest` and
   `an_event_after_the_window_opens_a_new_fire`, V264's window bound).
   Why: the window contradicted the start key, which outlives it. The
   rendered key is the author's template and the only dedupe (I3).
4. Generations (`RuleEntry.generation`, `RuleState.generation`,
   `reset_to` run by every mutation, the test
   `every_mutation_raises_the_generation`). Why: the incarnation and the
   re-read of the entry under the lock before each start cover both of
   their purposes (D1 "One pass over one rule", last paragraph).
5. The epoch prefix of journal line ids and the per-name journal file:
   one journal file per incarnation, ids that only grow in it.
6. The canonical-root fallback of the project key (`dir:<root>`,
   `probation::project_keys` made `pub(crate)`, the consumer's project
   hash, the test `a_copied_workspace_file_does_not_share_a_consumer`).
   Why: the listener sweeps registered projects only, so a rule of an
   unregistered project would never run; saving now requires
   registration.
7. Files folded or left alone: `trigger/log.rs` into `state.rs` (the
   outbox and the journal are one mechanism), `trigger/listener.rs` into
   `view.rs` (the doctor reads the heartbeat there), the run's rule from
   `run_view/started.rs` and `run_view.rs` into `trigger/view.rs`,
   `crates/apb-core/src/probation.rs` and `docs/DEPLOYMENT.md` unchanged.

## Cut in the fourth pass

The third review found more windows in the machinery the third pass had
kept. It was cut again, each point checked against the code on efae5822
first:

1. The run guard name `rule_start` (`GuardedFeature::RuleStart`, the
   fourth parameter of `run_guard_of`, the `ALL` count notes, the guard
   reads of `rule_start`, the tests `rule_start_is_a_known_feature`,
   `run_guard_of_names_rule_start_only_when_asked`,
   `run_guard_keeps_rule_start_from_the_journal`,
   `a_guard_alone_reads_record_lost`) and the reliance on `+guard`. Why:
   main strips `+guard` (`manifest.rs` lines 547 to 550) and accepts it,
   so a journal cut after `run_provenance` was resumable by main; the
   new `+rule` mode is refused by every reader since 0.23 before it
   reads the journal, and with it no reader needs the guard.
2. The passkey purposes of `set`, `enable`, `disable` and `remove`
   (the `action` field of the purpose, the `set` digest that needed an
   incarnation not yet made, the CLI's `trigger_passkey_required` on
   them, the test `set_refuses_with_a_passkey_enrolled`). Why: none of
   them can make a rule fire, since every rule needs an approval and any
   digest change drops it; `approve` alone takes the passkey, bound to
   the stored digest. `ApprovalNeed.needed` goes too: every rule needs
   its approval.
3. `enabled` in `Rule` and every way `set` could change it. Why: a
   `set` with `enabled: true` could switch a rule on without moving its
   position. It lives in `RuleEntry` now, changed only by `enable` and
   `disable`.
4. The per-rule inbox consumer and every retirement step (`consumer_name`,
   the acks of `set`, `enable` and `align`, `Inbox::retire` in this
   change, the `Retire` results, the tests
   `create_acks_the_consumer_at_the_head`,
   `a_source_change_moves_start_seq_and_retires_the_old_consumer`,
   `remove_retires_the_consumer`, `retire_releases_retention`). Why:
   retention never drops an unacknowledged event by age and an account
   with no consumer has nothing acknowledged (`inbox.rs` lines 624 to
   627 and 705 to 712), the size cap drops events a consumer would not
   protect either (line 720), so a consumer bought nothing and cost a
   cleanup with its own crash windows. Of the two cases the brief named,
   the first holds: no fixed per-project consumer is needed.
5. Orphan pruning in the sweep (`state::prune`, item 8 of the pass, the
   test `prune_returns_an_orphan`). Why: it deleted by name and could
   delete a recreated rule's state. Only `remove` deletes, under the
   rule's locks; leftovers are inert, discarded by a new incarnation's
   first pass and reported by `apb doctor`.
6. "Would start" in `apb trigger test` (`FireOutcome::WouldStart`,
   `warnings`). Why: preparation's checks do not run in a dry run; the
   report now names them as checked at start.
7. The outbox (`RuleState.outbox`, `next_line`, `held`, `gap`,
   `flush`, `append_missing`, numeric line ids, the batch-into-outbox
   step). Why: clearing it did not make the journal durable. The line is
   now appended and synced before the position moves, with an id that
   makes a repeat a no-op.
8. Every free-text `detail` of the journal and of `FireOutcome`. Why: an
   engine refusal's sentence can carry a param value. The journal holds
   codes and names from a closed set.

## Cut in the fifth pass

The fourth review found no blocker; its five majors and two minors were
checked against the code on efae5822 and closed by cutting:

1. The separate reset path for a source change (the `reset` verdict, the
   `<incarnation>:reset:<start_seq>` ids, `AlignOutcome::Moved` on a new
   source, the test `a_source_change_moves_the_position`). Why: inbox
   seqs are per account (`inbox.rs` lines 211 and 356), so a seq of the
   new source could reuse an id of the old one. A source change now gives
   a new incarnation, exactly as a creation; one incarnation reads one
   source, and the one position line left (`positioned`, for a creation,
   a new incarnation or a grown `start_seq`) is written before its
   transaction.
2. The `starts` list and `StartRecord` of the signed state, and the
   rule that an `already_started` answer adds a start. Why: a start whose
   line was lost and whose event retention evicted was never counted
   again. The hourly cap reads the runs' launch records through the
   reverse-link scan; V264's upper bound falls from 600 to the scan's
   500.
3. The append that trusted the file's tail. Why: a torn record glued to
   the next line hid both from the reader. The appender repairs the tail
   first, with the review channel's repair moved to `fsutil`.

## Cut in the eighth pass

1. `gate::assess_permit`, the comparison before `check_run` and the one
   after it, and the barrier `Hook::TriggerPermitAssessed`
   (`deadline_hooks.rs` leaves the file list). Why: two comparisons on
   two permits left a window between them, and an `ask` launch still
   failed its stored nonce before either could see the change. The
   approved digest now travels on `LaunchContext.approved_permit` and is
   compared once, in `Gate::admit`, on the permit that is then decided
   and handed on as the expected pins.
2. The stored launch nonce of an approval (`ApprovalNeed.launch_nonce`,
   `Approval.launch_nonce`, `LaunchConfirmation::RuleApproved`). Why: for
   `Initiator::Trigger` an approval whose permit digest equals the
   resolved permit is the confirmation an `ask` policy needs; a nonce
   was a second binding of the same permit that could only disagree.
3. The minimal failure manifest (`ManifestExecution::rule_only`) and its
   tests. Why: it lacked the run's other immutable fields (`unattended`,
   `readiness_required`), so a resume read it as attended. A rule start
   whose preparation fails before the full manifest now leaves no run
   directory at all; the predecessor-binary test keeps its two journals.
4. The once-per-pass hourly count. Why: a run a failed hand-over left
   behind was not counted for the rest of the pass. The count is read
   before each matching start.

## Cut in the tenth pass

The ninth review again found its blockers and one major in the two
mechanisms every pass had to repair. The feature was narrowed so that
neither is needed:

1. The rule's `attended` option. Why: with it a rule's run could earn
   attended credit and needed its own probation exclusion. A rule's run
   is always unattended; attended credit requires `unattended == false`
   (`apb_core::probation::counted`, `probation.rs` lines 80 to 91, over
   the mode the engine's writer records, `apb-engine/src/probation.rs`
   line 187), and a child inherits its parent's unattended mode
   (`scheduler/node.rs` lines 5013 to 5015). So the exclusion is cut:
   `from_rule`, `triggered_by` copied to children by `LaunchGrant::child`,
   the parent-manifest lookup, and their tests.
2. The park of a rule's run (the `specs/wait-park/spec.md` delta, the
   `rule_started` parameter of `may_release`, `rule_started` itself, the
   doc and comment edits of `scheduler/unattended.rs` and
   `next_action/unattended.rs`, and every park test). Why: it existed
   only to release an unattended run's driver; a rule's run now keeps
   its driver as any unattended run on main does, and lifting that is
   future work.
3. The `+rule` manifest mode suffix, the shared suffix parser change
   (`suffixes`, `is_rule_started`, the `is_guarded` rework), the
   failed-preparation special handling (`Unstarted.rule_start`, the
   removal through the refused-admission cleanup), the child marking,
   `RuleStart` and its `RecordLost`, and the predecessor-binary corpus
   row T1. Why: with 1 and 2 an older binary that does not know a rule
   sees an ordinary unattended run and behaves correctly. A rule start
   whose preparation fails is handled as main handles any failed start.
   `triggered_by` stays, `serde(default)`, for display and the hourly
   count.
4. The passkey proof of an approval (`Approval.proof`, `ApprovalProof`,
   `rule_approval_challenge`, the `verify_challenge` split, the
   `trigger_approve` passkey purpose, `approval_unproven`,
   `trigger_passkey_required`, their tests). Why: the ninth review showed
   the ceremony could not carry the assertion as specified, and the
   approval's boundary is now stated once instead: like main's launch
   floors and signed stores it guards against mistakes and agents acting
   through apb's surfaces, not a same-user process writing the file;
   binding an approval to a passkey is future work. The approval binding
   compared in admission stays.
5. The head read at `approve` and `enable`. Why: an event could arrive
   between the head read and the commit. Those commands now delete the
   rule's state before their rules write, and the first pass positions
   the rule at the head it reads then, starting nothing in that pass.
6. Condition literals and event values in the journal (`LogEntry`'s
   `ConditionResult`). Why: `ConditionResult` holds the rule's `equals`
   literal and the event type. The journal keeps `ConditionMark` (index,
   passed); the dry test's response keeps the full results.

## Review record

First pass, the closure pass of 2026-11-20, on main efae5822:

1. Renamed the change from `start-rules-and-schedules` to `start-rules`
   in every self-reference (titles, the public record's name, the target
   and temp dirs, the validate command).
2. Removed the schedule group (owner decision: schedules come after 1.0):
   design D4, `specs/schedules/`, tasks section 6, the codes V275 to
   V277, the `jiff` dependency and `THIRD-PARTY-NOTICES.txt`, `cron.rs`,
   `schedule.rs`, `schedule_test.rs`, the overdue suggestion, and every
   D4 line of the proposal and the release notes. Added "Deferred to
   after 1.0" with what keeps the format extensible.
3. Re-verified every file:line on efae5822 (the draft read 316c8180 and
   fd49e0da). Drifted and fixed: `check_run` 690 to 692; `decide` 830 to
   863; `billing_confirmation` 1002 to 1035; `launch_confirmation` 1033
   to 1066; `refusal` 1104 to 1137 and its nonce write 1148 to 1181;
   `signed_store` `dir` 89 to 127, `read` 110 to 148, `transact` 219 to
   267, `file` 140; inbox `read` 354 to 396, `ack` 383 to 425,
   `read_events` 459 to 590, `min_cursor` 493 to 624, `last_seq` 519 to
   650, `check_consumer` 803 to 934; the deferred call 1090 to 1116;
   `drive_inner` 697 to 720 and `driver_claim` 715 to 738;
   `resolve_target` 805 to 817; `Issue` 90 to 93, `Severity` 84 to 87;
   `Param` 876 to 901 with `secret` 915, `pattern` 928, `untrusted` 933;
   `read_id` 81 to 84; `LOCAL_ENTRIES`'s last entry is `runs-archive/`,
   not `health/`, and its test is at line 329; `Health` 613 to 618 and
   its dispatch 1363 to 1376; `apb runs` started-by line 2167 to 2250;
   `report_html/model.rs` 47 and 139 to 64 and 163, `sections.rs` 161 to
   169; the doctor's health line 165 to 176; the discover page loader is
   line 47, not 144; `docs/CONNECTORS.md` 413 to 454, `docs/GUARDRAILS.md`
   381 and 481 to 423 and 523, `docs/CI.md` item 9 at 302;
   `tests/main.rs` 263 to 264.
4. Code that contradicted the draft: `check_probation` is a preparation
   check with another signature (`root, scope, loaded, pins, waive,
   overridden`) that no surface calls, and readiness is
   `RunOptions.require_ready` checked in preparation; the fire now sets
   `RunOptions` (`require_ready`, `waive_probation: false`) instead of
   calling either, since `StartArgs` has neither field.
   `run_origin::tag` named for the key MAC is replaced by the existing
   `start_key::key_tag`. `Appended::Stored` shares one match arm with
   `Duplicate` at `ingest.rs` line 790; the arm is split.
5. Sweep host decided against reuse: the ingest listener (both `apb
   ingest` and the dashboard's co-started listener go through
   `run_ingest_server`) plus one pass in `apb resume --all`; the
   dashboard-only spawn and its second `spawn` call are dropped, since
   only the listener stores events. No new daemon.
6. Closed R1 (a MODIFIED delta of wait-park's requirement in
   `specs/wait-park/spec.md`), R2 (the waker resumes an authorized run
   with no person; probation and readiness are start checks only) and R3
   (the unattended next action already maps a park; a comment changes).
   The `detached` parameter is renamed `rule_started` (what it is).
7. Crash windows written out; the start key closes each. The
   `firing_since` field and its two-minute retry are dropped: the
   per-rule sweep lock is an OS lock released by a dead process, so a
   pending fire is simply fired again and the start key answers
   `already_started`. `AlreadyStarted` now records the run in `active`
   and `starts` when absent, so a crash before transaction 2 still
   counts against the caps. A `start_key_conflict` is recorded as
   `already_started` (one run per rendered key); `start_key_busy` and
   `concurrency_busy` keep the fire pending.
8. Coordination with eval-doubles and inbox-waits: shared files and
   symbol ownership named; `forget_consumer` renamed `retire` to match
   inbox-waits' identical function; `head_seq` is a new public function
   and `last_seq` stays private; no `deadline_hooks.rs` change (the
   `Hook::TriggerBeforeFire` hook is replaced by a lock probe in the
   test); the rule consumer's shape never matches inbox-waits' reserved
   `run_` names.
9. The consumer's project key uses `projects::registered_id` (the
   registry, bound to the root) instead of `workspace::read_id` (a file
   that travels with a copied checkout), and 16 hex instead of 8; the
   name limit is 40 so the consumer stays a 62-character snake name.
10. `enable` moves `start_seq` to the head and acks the consumer, so a
    re-enabled rule fires no backlog; the sweep drops pending fires at
    or below a new `start_seq` (replaced by the second pass, item 2: the
    rule generation and source decide the reset). `remove_rule` takes the rule's sweep lock
    before it deletes the state, journal and consumer.
11. Validator block: V260 to V266 used, V267 to V279 reserved, checked
    free on efae5822 and against both in-flight changes (eval-doubles
    V280 and V281, inbox-waits V284 to V286).
12. File estimate 76 (24 new, 47 changed, 5 records), down from 84
    (raised to 87 by the second pass, item 16).
13. tasks.md: a precondition task 0 naming each symbol with its exact
    file, the tests named in every task, the full gate list as the last
    task; every spec requirement is under 500 characters.

Second pass, 2026-11-20 (items 1, 2, 5 and the generation and
`active` parts of 6 and 9 were superseded by the third pass, which cut
the machinery they patched): the design review of that day (13 findings and
two notes on main), each verified against the code on main efae5822 and
disposed of as follows. The fixes are the invariants I1 to I6
("Invariants of the rule store and the sweep") applied; the section was
written first and the cases enumerated against it.

1. Blocker 1, a rule edit can launch an obsolete rule. Verified: the
   draft read `rules.json` once per pass, `set_rule` and `set_enabled`
   took no sweep lock, `remove_rule` deleted the entry before taking it.
   Fixed (I2, I3): every mutation (`set`, `enable`, `disable`, `approve`,
   `remove`) takes the rule's sweep lock first (`hold_lock_waiting`, 60
   s) and holds it through the rules write, the state reset and the
   inbox ack or retire; the pass re-reads the rule under its lock and
   again before each fire, firing only the generation transaction 1
   used; `MAX_FIRES_PER_PASS` bounds the wait. Test
   `a_disable_during_a_pass_starts_nothing_after_it`.
2. Blocker 2, a source change suppresses events or fires old work.
   Verified: only `start_seq` moved; `max(seen_through, start_seq)`
   skipped the new account up to the old seq, and pending fires with
   `first_seq` above the new head survived. Fixed (I4): `RuleEntry.generation`,
   `RuleState.generation` and `RuleState.source`; any difference resets
   `seen_through` to the new source's head, drops pending fires, retires
   the old consumer and journals one `reset` line (`state::reset_to`,
   run by the mutation and completed by the next pass after a crash).
   `active` and `starts` are kept per rule name. Test
   `a_source_change_resets_the_position`.
3. Blocker 3, an automated run satisfies attended probation. Verified:
   `crates/apb-engine/src/probation.rs` `entry_of` (line 148) excludes
   only `started_by` (line 197); `apb_core::probation::counted` (line 80)
   counts every eligible attended success. Fixed: `from_rule` beside
   `from_step`, read through `trigger::rule_started` (the launch record or
   the guard), so a rule-started run is never eligible whatever its gate
   mode (D1 "Probation"). Test `a_rule_started_run_earns_no_probation_credit`.
4. Blocker 4, the person-only guard does not establish a person.
   Verified: `step_env.rs` lines 1 to 7 say the check guards mistakes,
   not cleared variables; the dashboard's `StepCaller` reads the server's
   environment. Fixed as decided, without a new credential (D0 "A
   person's command"): MCP and run steps refused; the CLI only at an
   interactive terminal with `consent::ask_typed` (the rule's name), the
   dashboard only through `RequestChannel::Session`, and with a passkey
   enforced only the dashboard with a fresh assertion (a `trigger`
   purpose of the existing ceremony, its token taken once under the
   rule's lock). The design and `docs/TRIGGERS.md` say plainly that the
   environment and terminal checks guard mistakes only and that the
   passkey is the credential a run step does not have. `disable` loses
   its earlier exception (no terminal, no keys): it changes whether a
   rule fires and resets its position, so it is a mutation like the
   others.
5. Major 5, a full queue loses later matches. Verified: the draft logged
   `pending_full`, dropped the match and moved `seen_through` past it.
   Fixed (I5): evaluation stops before the first match that cannot be
   queued, `seen_through` and the cursor stay behind it, and at most one
   `held` line (`pending_full:<seq>`) per pass, only when the blocked seq
   changed (`RuleState.full_at`). The `PendingFull` verdict is gone. Test
   `a_full_queue_holds_the_cursor`.
6. Major 6, a crash erases the explanation. Verified: transaction 1
   moved `seen_through` and stored the gap before any journal line was
   written. Fixed (I5): every transaction writes its lines into
   `RuleState.outbox` with ids `<epoch>.<n>`; `log::flush` appends the
   lines whose id the journal lacks, then clears them, first in every
   pass and after every transaction. Test
   `a_crash_before_the_journal_append_loses_no_line`.
7. Major 7, `trigger_test` is not read-only. Verified: `keyed_lookup`
   (`tools/run.rs` line 120) calls `start_key::lookup` (line 231) inside
   `signed_store::transact` (line 267, lock directory and file), whose
   `judge` (line 147) creates and removes a probe lock file; and
   `run_origin::tag` (line 126) goes through `ensure_key`, which calls
   `process_mode::guard` first (line 43), so it refuses in a read-only
   process. Fixed: `start_key::peek` over `signed_store::read` and the new
   `fsutil::flock_held`, `key_tag_if_keyed` and `capture_if_keyed` over
   `run_origin::tag_if_keyed`, and `tools::run::keyed_peek`; the test
   takes no sweep lock (D1 "`apb trigger test`"). Tests
   `trigger_test_writes_nothing`, `trigger_test_works_in_read_only_mode`,
   `peek_writes_nothing`, `flock_held_creates_nothing`.
8. Major 8, an older binary can drop the only marker. Verified:
   `manifest::amend` (`manifest.rs` line 860) reads and rewrites the typed
   manifest. Fixed: the run guard `rule_start` (`GuardedFeature::RuleStart`,
   last in `ALL`, after `EvalDoubles` when that merged first; the second
   to merge raises the count), written by `run_guard_of` for a run whose
   launch record names a rule and kept by `run_guard` from the journal;
   readers ask `trigger::rule_started` (record or guard). A run no rule
   started writes the journal it wrote before (rows W1 and W2 unchanged);
   corpus row T1 added. The proposal's "no new event field" stands; it
   no longer says the journal is untouched.
9. Major 9, signed files are not project-bound. Verified: the defaults
   of `SignedDoc` (`signed_store.rs` lines 53 and 58) accept any file the
   MAC covers, through a link. Fixed (I6): `project` in both files,
   `NO_LINKS = true` and `belongs_to` over `probation::project_keys`
   (made `pub(crate)`), exactly as `ProbationFile`. Tests
   `a_rules_file_of_another_project_is_refused`,
   `a_linked_rules_file_is_refused`,
   `a_state_file_of_another_project_is_refused`.
10. Major 10, V262 lets event text in as trusted input. Verified:
    `param_pattern::compile` (line 23) anchors any pattern, `.*`
    included, and only `untrusted` feeds the analysis (`untrusted.rs`
    line 5). Fixed: event text requires `untrusted: true`; a pattern is
    an additional constraint. Test `v262_a_pattern_alone_does_not_pass`.
11. Major 11, the dedupe test cannot exercise dedupe. Verified: the
    recording stand-in never touched a key store. Fixed: test 2 uses the
    real keyed start of tests 8 and 12 (the engine's gate with
    `Initiator::Trigger`, `start_key::lookup`, `start_detached` with
    `RunOptions.start_key`), so the second fire is answered by the real
    key store.
12. Minor 12, cited primitives behave otherwise. Verified:
    `signed_store::read` answers `Ok(Loaded { signed: false })` (line
    148), and `scalar_at_path` (line 135) answers `None` for a missing
    path and a non-scalar alike. Fixed: every read checks `Loaded.signed`
    explicitly; `render` classifies through `value_at_path` (line 117).
    Test `a_missing_path_and_an_empty_string_differ`.
13. Minor 13, the budget's type. Verified: `StartRequestFields.budget` is
    `Option<serde_json::Value>` (`start_key.rs` line 120; the review cited 123). Fixed: the
    canonical conversion `rule.budget.as_ref().map(serde_json::to_value).transpose()`,
    the one `apb run` (`run.rs` line 760) and MCP (`server/run.rs` line
    1560) use, with sorted keys (no `preserve_order`), an error being
    `Failed { code: "budget_unencodable" }`.
14. Note, inbox size retention can evict unacknowledged events.
    Pre-existing on main (`enforce_retention`, `inbox.rs` line 687);
    rejected for this change: the inbox's retention is not this change's
    to redesign, and I5 names it as the one loss a crash or a held queue
    does not cause. The sweep logs it as a `gap` once.
15. Note, start keys expire after `runs.start_key_days`. Pre-existing and
    accepted: said in "Invariants" and in `docs/TRIGGERS.md`.
16. File estimate 87 (24 new, 58 changed, 5 records), up from 76: eleven
    changed files, each a fix above (Code map rows 72 to 82).

Code that contradicted the review: nothing; every finding held on
efae5822. Two details beyond it: `run_origin::tag` refuses in a
read-only process even when the key exists (`ensure_key` calls
`process_mode::guard` before it looks), which is why the dry fire needs
the `_if_keyed` variants and not only a lock-free lookup; and the
passkey ceremony has no generic purpose, so rule changes add a fourth
purpose next to `decision`, `enroll_new` and `remove`, its token taken
the way `remove_handler` takes one.

Third pass, 2026-11-20: the second design review (three blockers, five
majors, two minors, two notes on main), each verified against the code
on main efae5822. Per the owner's decision the owned machinery was cut
("Cut in the third pass") instead of patched, and the invariants were
written first ("Invariants", I1 to I5).

1. Blocker, the concurrent cap can be exceeded in one sweep. Verified:
   transaction 1 counted chosen fires against `max_per_hour` only.
   Disposition: removed with the cap (cut item 2). Runs at once are the
   playbook's `concurrency:` block; the hourly cap is checked against
   the signed `starts` before every single start (D1 "One pass over one
   rule" item 6.2.5.1), so one pass cannot exceed it either.
2. Blocker, remove and recreate can fire an old pending event. Verified:
   a recreated rule got generation 1 again and `reset_to` matched the
   old state. Fixed: no pending fire exists any more (cut item 1), and
   every creation gets a random incarnation that binds the state, the
   journal, the consumer and the start keys; state of another
   incarnation is discarded on the first pass (I5). Test
   `a_recreated_rule_never_inherits_the_old_state`.
3. Blocker, the older-binary guard is absent during a resumable crash
   window. Verified: `run_guard` is appended at `prepare.rs` line 1644,
   after `run_started` (1594) and `run_provenance`; and the manifest's
   `+guard` mode, written at 1581 before both, is decided by
   `manifest_guard` (`manifest.rs` line 588), which names approvals and
   person-only gates only, so it is not written for every guarded run.
   Fixed: a rule start also sets the manifest guard (line 1499, D0 "The
   run guard of a rule's run" item 1), which an apb from 0.23 on refuses
   before it reads the journal; the journal guard stays for checkpoints.
   The window is closed for this feature. Corpus row T1 and
   `a_rule_started_run_carries_the_rule_start_guard`.
4. Major, approval can attach to a rule the person did not review.
   Verified: `ApprovalNeed` had no rule digest. Fixed: `rule_digest`
   (every field that changes whether or how the rule fires, and the
   incarnation) is in the need shown, in the comparison at `approve`
   (under the rule's locks) and, through the need's digest, in the
   passkey purpose (D1 "Approval"). Tests
   `approve_refuses_a_need_of_another_rule_digest`,
   `approve_refuses_a_changed_need`.
5. Major, editing the target playbook lifts the concurrent cap.
   Disposition: removed with the `active` list (cut item 2).
6. Major, coalescing after the first window has contradictory outcomes.
   Verified: the start key `trigger:<rule>:<rendered key>` outlived the
   window (`start_key.rs` line 205). Disposition: windows removed (cut
   item 3); the key is per the author's template and dedupes for
   `runs.start_key_days` like any start key, tested through the real key
   store (`an_entity_key_starts_once_per_entity`,
   `a_resent_delivery_with_a_new_id_starts_once`).
7. Major, a listener never sweeps an unregistered project. Verified:
   `list_active` enumerates registry entries (`projects.rs` line 899).
   Fixed: saving a rule requires a registered id
   (`trigger_project_unregistered`, D1 "Where rules live" item 2); the
   canonical-root fallback is removed (cut item 6). Tests
   `an_unregistered_project_cannot_save_a_rule`,
   `set_refuses_an_unregistered_project`.
8. Major, the disable-during-pass test cannot pass under the lock.
   Verified: the pass held the sweep lock through every fire. Fixed: a
   mutation takes a change lock the pass probes before every start and
   yields to (I4); the test asserts the serialization rule (no start
   after the disable committed) and that the disable waited for the
   current start at most.
9. Minor, the HTTP approval body lacks a deserializable type. Fixed:
   `ApprovalNeed` derives `Deserialize` with unknown fields denied, and
   every HTTP body is its own type with unknown fields denied. Test
   `approve_refuses_an_unknown_field`.
10. Minor, the guard-only run surface has no value. Fixed: `RuleStart`
    with `Known` and `RecordLost`, rendered by every surface (D1 "The
    run surfaces"). Tests `a_guard_alone_reads_record_lost`,
    `the_run_detail_names_its_rule`.
11. Note, inbox size retention can evict unacknowledged events.
    Pre-existing on main (`inbox.rs` line 673); rejected for this change
    as before: a `gap` line is journaled once.
12. Note, start-key deduplication ends after `runs.start_key_days`.
    Pre-existing and now the stated contract (I3, `docs/TRIGGERS.md`).
13. File estimate 81 (22 new, 54 changed, 5 records), down from 87.

What the code showed beyond the review:

1. `tools::start_options` sets `workdir_queue_wait: None`
   (`tools/run.rs` line 72), so a start while another write run holds
   the tree fails at once with `WorkdirBusy`, which under I2 would make a
   busy tree the event's verdict. The fire sets the wait the dashboard's
   start uses (`routes/playbooks.rs` line 454, `config.rs` line 1070),
   so the run is admitted and waits in its drive. Test
   `a_busy_tree_queues_the_start`.
2. There is no command that only registers a project: every apb command
   in a project with `.apb` registers it (`main.rs` line 1110,
   `projects::touch`) unless registration is off (`APB_NO_REGISTRY=1`,
   `CI` set, `registry: false`; `projects.rs` line 207). The refusal
   names that and `apb projects list`; the tests register their fixture
   with `projects::bind_id`, since `CI` turns `touch` off.
3. The manifest's `+guard` mode does not reach an apb older than 0.23,
   which ignores the execution block; this is main's accepted limit for
   approvals, and it is said, not changed.

Fourth pass, 2026-11-20: the third design review (one blocker, eight
majors, one minor, three notes on main), each verified against the code
on main efae5822. As decided, the machinery was cut again ("Cut in the
fourth pass") rather than patched.

1. Blocker, the compatibility fix does not reject a binary built from
   main. Verified: `ManifestExecution::base` (`manifest.rs` lines 547 to
   550) strips `+external` and `+guard`, so main accepts `cli+guard`;
   it does not strip an unknown suffix (`execution_mode`, line 605,
   parses the rest with `ExecutionMode::parse`, `execution.rs` line 56,
   `cli` and `host` only, and refuses anything else, lines 607 to 616;
   a resume reads it first, `scheduler/resume.rs` line 617). Fixed: the
   `+rule` mode, written before `run_started` (D0 "The manifest mode of
   a rule's run"); the run guard name is dropped. Task 7.2 tests a
   journal cut after `run_provenance` against the predecessor
   `v1.0.0-beta.7` and both historical readers.
2. Major, a passkey-protected creation needs a digest that does not yet
   exist. Disposition: removed with the `set` purpose; only `approve`
   takes a passkey, bound to the stored digest, which exists by then
   (D0 "A person's command"). Tests `set_needs_no_passkey`,
   `set_takes_no_token`, `approve_needs_a_fresh_assertion`.
3. Major, `set` can re-enable a rule and process events that arrived
   while it was disabled. Verified: `enabled` was a `Rule` field outside
   the digest. Fixed: `enabled` is not in `Rule`; a document naming it
   is refused `trigger_enabled_not_settable`; only `enable` (moving the
   position to the head) and `disable` change it. Tests
   `set_refuses_a_document_naming_enabled`,
   `a_disable_then_set_keeps_the_rule_disabled`,
   `enable_moves_start_seq_to_the_head`.
4. Major, retirement loses its recovery information. Disposition:
   removed with the consumer. Retention read on efae5822: age retention
   drops only events at or below the lowest consumer cursor (`inbox.rs`
   lines 705 to 712), which is 0 with no consumer (lines 624 to 627); the
   size cap (line 720) drops regardless of consumers. A rule registers
   none (cut item 4); test `a_rule_registers_no_consumer`.
5. Major, orphan pruning can delete a recreated rule's live state.
   Disposition: pruning removed (cut item 5); leftovers are inert and
   reported. Test `leftovers_of_a_crashed_remove_are_inert`.
6. Major, `trigger test` can say "would start" when the start refuses.
   Verified: probation and readiness run only in preparation
   (`prepare.rs` lines 775 and 791), the concurrency key later (1252).
   Fixed by claiming less, as decided: the report gives the filter, the
   rendered params and key, the key's use, the checks before
   preparation, and names probation, readiness, concurrency and the
   working tree as "checked at start"; it never prints "would start".
   No shared assessment is added. Test
   `trigger_test_names_the_checks_at_start`.
7. Major, sweeping from a run step consumes events as refusals.
   Verified: `check_unattended` refuses from a step (`gate/unattended.rs`
   line 83) and `for_surface_start` inherits the step's flag
   (`external_data.rs` line 196). Fixed: `apb resume --all` skips the
   rule sweep in a run step before reading anything, and the listener is
   never one. Test `resume_all_in_a_run_step_sweeps_nothing`.
8. Major, copied engine refusal text carries values. Verified:
   `concurrency_busy` names the key's display (`concurrency.rs` line
   660), rendered from params (line 504). Fixed: the journal's closed
   set of fields, codes and rule-side names only. Test
   `a_busy_concurrency_key_leaves_no_value_in_the_journal`.
9. Major, clearing the outbox does not make the journal durable.
   Verified: inbox `append_line` (`inbox.rs` line 810) writes and
   flushes without a sync. Fixed: the outbox is cut; each line is
   appended and synced (the directory too on creation) before the
   position moves, with a stable id; `state::read` dedupes. Tests
   `a_crash_between_line_and_transaction_repeats_the_event_once`,
   `a_journal_write_failure_leaves_the_position`,
   `a_failed_sync_returns_an_error_before_anything_else`.
10. Minor, V261 has no path to the lifecycle. Verified: `store::resolve`
    (`store.rs` line 52) does not read it; the gate does
    (`gate/mod.rs` line 1608). Fixed: `RuleContext.lifecycle` from
    `trust::read_lifecycle` (`trust.rs` line 79); draft and retired
    refused. Tests `v261_a_draft_target`, `v261_a_retired_target`.
11. Notes on main (retention by size, the key lifetime, the environment
    checks not authenticating a person): pre-existing, as before; the
    third is now narrower, since a process that clears the variables can
    write a rule but cannot approve one past an enrolled passkey.
12. File estimate 81 (22 new, 54 changed, 5 records), unchanged in count:
    `validate/guarded.rs` leaves the list and `manifest.rs` joins it.

What the code showed beyond the review: the size cap of inbox retention
drops the oldest events whether or not a consumer acknowledged them, so
the per-rule consumer of the third pass never protected what it seemed
to; and the third pass's own premise that `+guard` stops a current
binary was wrong, which the review found.

Fifth pass, 2026-11-20: the fourth design review (no blocker, five
majors, two minors, five notes on main), verified against the code on
efae5822 ("Cut in the fifth pass").

1. Major, approval does not bind the whole permit. Verified: an `allow`
   launch returns before any confirmation (`gate/launch.rs` line 1072),
   and the consent nonce binds definition digests only (`consent.rs`
   line 113, `tree_digest`). Fixed: `ApprovalNeed` and `Approval` carry
   `permit.permit_digest()` (`gate/mod.rs` line 604, `gate/launch.rs`
   line 747), compared at `approve` and before every start whatever the
   policy; a difference unapproves the rule (one `permit_changed` line,
   no start) until a person approves again. Tests for both interleavings:
   `approve_refuses_a_changed_permit` and
   `a_changed_permit_unapproves_the_rule`.
2. Major, journal ids collide after a source change. Verified: seqs are
   per account directory (`inbox.rs` lines 211 and 356). Fixed by the
   cut: a source change gives a new incarnation; the reset path and its
   ids are gone. Test `a_source_change_never_reuses_a_line_id`.
3. Major, torn appends have no recovery. Verified: the review channel
   already repairs its tail under its lock (`review.rs` `repair_tail`,
   line 1307, called at 1342), and `event.rs` (line 2743) only tolerates
   a torn tail when reading. Fixed: that repair moves to
   `fsutil::repair_line_tail` with the parse as a parameter, the review
   channel calls it, and the trigger appender runs it under the sweep
   lock before every append. Test
   `a_torn_tail_is_repaired_before_the_next_append`.
4. Major, a reset commits before its line. Fixed: `plan_align` decides
   from the state as read, the `positioned` line is appended and synced,
   then `align` commits; the same for a gap. Test
   `a_crash_after_the_positioned_line_is_logged_once`.
5. Major, retention can erase a committed start from the cap. Fixed by
   the cut: the cap counts the incarnation's runs from their launch
   records (the reverse-link scan of `run_view/started.rs` line 60, bound
   `STARTED_RUNS_SCAN` = 500 at line 11, over
   `decision/report/mod.rs` line 257), filtered to directories written in
   the last hour; V264 refuses a cap above 500, and a truncated scan
   counts as the cap reached. Test
   `a_start_lost_to_a_crash_and_retention_still_counts`.
6. Minor, the spec still names the run guard. Fixed: "launch record or
   the manifest's execution mode" in the parks requirement.
7. Minor, the probation test conflicts with external data. Verified:
   the observer writes nothing for an external data run
   (`probation.rs` line 86). Fixed: the design sentence is qualified,
   test 24 runs the real start with `external_data: false`, and
   `a_default_rule_run_writes_no_probation_entry` asserts no entry for
   the default; task 4.2 names both.
8. Notes on main (size retention, the key lifetime, the environment
   checks, children of step-started runs losing `started_by`, and
   `atomic_write_private` not syncing the directory): pre-existing, not
   this change's. The fourth note does not touch a rule's run, which has
   no step starter; the fifth matters only for the signed files' rename,
   whose loss on a host crash leaves the earlier signed version, which
   every reader handles.
9. File estimate 82 (22 new, 55 changed, 5 records): `review.rs` joins
   the list for the shared tail repair.

Sixth pass, 2026-11-20: the fifth design review (no blocker, four
majors, two minors, four notes on main), all in what the fifth pass
added, verified against the code on efae5822.

1. Major, sub-playbooks can earn probation credit. Verified:
   `LaunchGrant::child` (`gate/launch.rs` line 437) builds a fresh record
   with `started_by: None` (line 446), and a child inherits the attended
   mode (`scheduler/node.rs` line 5015). Fixed: `LaunchGrant::child`
   copies `triggered_by`, and preparation marks every child of a rule's
   run with `+rule` from its parent's manifest, pinned or resolved live,
   so no run of the tree earns credit and no older apb drives one. Test
   `a_child_of_a_rule_run_earns_no_probation_credit`. That step-started
   children lose `started_by` on main stays a pre-existing note.
2. Major, the hourly cap treats unverifiable starts as absent. Verified:
   `newest_run_dirs_where` filters through `created_here` before its cap
   (`decision/report/mod.rs` line 274), and `manifest::amend` writes the
   manifest before restamping (`manifest.rs` lines 873 to 876). Fixed:
   the count walks `candidate_run_dirs` (line 296) itself and counts a
   run of the last hour whose origin or rule cannot be established
   toward the cap of every rule of the project; the docs say so. Tests
   `an_unattributable_run_counts_toward_the_cap` (the amendment window)
   and `a_lost_launch_record_counts_toward_every_cap`.
3. Major, an `ask` launch bypasses the changed-permit handling.
   Verified: `check_run` (`gate/mod.rs` line 692) hands the final permit
   to `Gate::admit` (line 884), which runs `launch::decide` (line 924),
   and the launch nonce covers the permit digest (`gate/launch.rs` line
   828). Fixed: the part of `check_run` before `admit` is split out as
   `gate::assess_permit` (nothing committed; every trust, lifecycle,
   health, import and pin refusal as before), `check_run` is it plus
   `admit`, and the fire compares the permit digest before calling
   `check_run`. Tests `the_permit_is_compared_before_the_launch_decision`
   and the `ask` half of `a_changed_permit_revokes_the_approval`.
4. Major, the invalidation disappears when the target is restored.
   Fixed: a mismatch writes the `permit_changed` line (with both digests)
   and syncs it, then clears the approval in `rules.json` under the
   rule's sweep lock by compare and clear; only a fresh approve brings an
   approval back, moving the position to the head. Recovery between the
   line and the write is stated (D1 "Approval" item 5). Tests
   `a_changed_permit_revokes_the_approval` (A to B to A across a restart)
   and `a_crash_before_the_revocation_is_completed_or_moot` (both cases).
   I1 now says the engine writes `rules.json` only to revoke.
5. Minor, the fire cannot supply the journal id. Fixed:
   `PermitChanged { permit_digest }` carries the digest compared; the id
   is `<incarnation>:permit:<digest>`; dedupe tested in
   `a_changed_permit_revokes_the_approval`.
6. Minor, incompatible source-change assertions. Fixed: the test keeps
   the old line and id before the change, then asserts the new
   incarnation, a distinct id and the deletion of the old journal.
7. Notes on main: unchanged, pre-existing.
8. File estimate 83 (22 new, 56 changed, 5 records): `gate/mod.rs` joins
   for `assess_permit`. The child marking needs no further file:
   preparation reads the parent's manifest beside the child's directory.

Seventh pass, 2026-11-20: the sixth design review (three majors, two
minors, five notes on main), verified against the code on efae5822.

1. Major, the permit compared is not the permit admitted. Verified:
   `check_run` reloads and resolves again (`gate/mod.rs` line 724), and an
   `allow` launch needs no confirmation (`gate/launch.rs` line 1073).
   Fixed: the digest of the permit `check_run` returns is compared with
   the approval again, and only that permit is handed to the engine as
   the expected pins (the hand-over of decision record 0003, as
   `server/run.rs` lines 717 and 718 do, verified in preparation at
   `prepare.rs` line 1465); a mismatch is the same `PermitChanged`. The
   `assess_permit` comparison stays only for the `ask` case. Race test
   `the_admitted_permit_is_compared_again`, held at a new
   `deadline_hooks::Hook::TriggerPermitAssessed`.
2. Major, a failed preparation leaves a resumable run without the mark.
   Verified: `prep_try_unstarted` (`prepare.rs` lines 160 to 176) writes
   `run_started` and stamps with no manifest; main resumes it with
   `--from-node` (`resume.rs` lines 611 and 111). Fixed: for a rule start
   it first writes a minimal write-once manifest with the `+rule` mode;
   if that fails the directory is removed whole as a refused admission
   removes its run (`prepare.rs` lines 1253 to 1262), no `run_started`,
   the key freed, the event `failed`. Tests
   `a_failed_rule_preparation_carries_the_mark`,
   `a_mark_that_cannot_be_written_removes_the_run`, and journal (c) of
   corpus row T1 against the predecessor.
3. Major, state recovery can replay old events. Verified: a missing
   signed file reads as fresh and valid (`signed_store.rs` lines 154 to
   160), so a fresh state at an old `start_seq` replays events whose key
   expired. Fixed by a cut: `RuleEntry.start_seq` is gone; the position
   lives only in the state, written by `approve` and `enable` under the
   rule's locks at the current head (before their rules transaction),
   and a missing or foreign state of an approved rule is positioned by
   the pass at the CURRENT head with one line, skipping what arrived
   meanwhile, the safe direction. The doctor says: remove the unreadable
   file; the rule continues from the head. Test
   `a_removed_state_restarts_nothing_eight_days_later`.
4. Minor, task 1.5 before the engine module exists. Fixed: moved right
   after task 4.1.
5. Minor, the dry-test response is untyped. Fixed: `TestReport`,
   `FilterVerdict`, `ParamCheck`, `KeyReport`, `RenderFailure`, `KeyUse`,
   `CheckId` with its fixed `ORDER`, `CheckLine` and `CheckState`
   (`passed`, `refused` with a code, `not_run`), the render failure of a
   param, and the trace collected by `fire_traced`, the one function the
   real start runs. Task 6.1 asserts them
   (`trigger_test_reports_every_check_in_order`).
6. Notes on main: unchanged, pre-existing.
7. File estimate 84 (22 new, 57 changed, 5 records): `deadline_hooks.rs`
   joins for the race barrier.

Eighth pass, 2026-11-20: the seventh design review (three majors, one
minor, six notes on main), verified against the code on efae5822. Two
majors were in mechanics earlier passes added; those were replaced by
simpler ones ("Cut in the eighth pass").

1. Major, the failure marker turns an unattended run attended on
   resume. Verified: a manifest without `unattended` reads false
   (`manifest.rs` line 385) and `unattended::mode` prefers it
   (`scheduler/unattended.rs` line 49). Disposition: the marker is cut; a
   rule start whose preparation fails before the full manifest leaves no
   run directory (refused-admission cleanup, `prepare.rs` lines 1253 to
   1262), the key is freed and the event is `failed`. The failures after
   the full manifest (`prepare.rs` lines 1594 to 1687, and the hand-over
   at `entry.rs` lines 884 to 895) leave a normally marked run. Test
   `a_failed_rule_preparation_leaves_no_run_directory`; the predecessor
   test keeps its two journals.
2. Major, the second comparison misses an `ask` race. Verified:
   `Gate::admit` (`gate/mod.rs` line 884) runs `launch::decide` (line
   924) before returning, so a stored nonce failed first. Disposition:
   one comparison inside `Gate::admit` on `LaunchContext.approved_permit`
   (`gate/launch.rs` line 277), before `decide`, returning
   `permit_changed` with the resolved digest; for `Initiator::Trigger`
   an equal approval confirms `ask`; no launch nonce is stored; a billing
   `ask` still refuses `billing_refused_for_trigger` and the docs say a
   rule needs billing `allow`. Tests
   `a_changed_permit_is_caught_in_admission` (allow and ask, restored: no
   approval back), `a_change_after_admission_fails_preparation` (held at
   the existing `KeyAdmitted`), `an_ask_rule_starts_without_a_launch_nonce`.
3. Major, failed starts escape the cap within a pass. Verified:
   `hand_to_detached_driver` (`entry.rs` line 884) abandons a fully
   prepared run with its launch record. Fixed: the count is read before
   each matching start. Test `a_failed_hand_over_counts_at_once`.
4. Minor, the mixed child fixture is impossible. Verified: a parent pins
   every child or none (`node.rs` line 4925). Fixed: two fixtures, a
   pinned parent and an explicitly ungated one, the live child marked
   from its parent's manifest.
5. Notes on main: pre-existing. The tenth (a dead pending-key owner whose
   journal ends after `run_started` is judged void, `start_key.rs` line
   126) is main's start key semantics and outside this change.
6. File estimate 83 (22 new, 56 changed, 5 records): `deadline_hooks.rs`
   leaves the list.

Ninth pass, 2026-11-20: a fresh reviewer's findings on the eighth pass
(two majors, eleven minors, seven notes on main), verified against the
code on efae5822.

1. Major M1, a stored approval can be forged. Verified: rules.json is
   guarded by the installation key every process of the user can read
   (`run_origin.rs` lines 23 to 27), and a person-only gate's decision
   carries the full assertion the drive re-verifies (`review.rs` lines
   436 to 441, `passkey_proof::verify`, line 174). Fixed: with a passkey
   enforced, `Approval.proof` holds the full assertion over
   `rule_approval_challenge(rule, incarnation, rule_digest, binding)`;
   the sweep re-verifies it through `passkey_proof::verify_challenge`
   (split out of `verify`) at step 2 and before each start; an approval
   without a valid proof is unapproved (one `approval_unproven` line, no
   start). Without an enforced passkey the limit is stated plainly. Tests
   `a_forged_approval_without_proof_starts_nothing`,
   `approve_stores_a_proof_the_sweep_verifies`.
2. Major M2, an approval confirms a stricter policy. Verified: the
   eighth pass bound the permit only; main refuses an earlier
   confirmation for a tree that became stricter (`gate/launch.rs` lines
   948 to 950). Fixed: one binding, `approval_binding` over the permit
   digest, the effective launch policy and its sources, and the billing
   lines, stored in `Approval.binding` and compared once in `Gate::admit`
   before `launch::decide`; any change, stricter included, is the
   revocation. The outcome is renamed `BindingChanged { binding }` (code
   `binding_changed`, `BINDING_CHANGED`, `approved_binding`) everywhere.
   Tests `a_stricter_policy_revokes_the_approval`,
   `a_changed_billing_decision_revokes_the_approval`.
3. Minor 1, the stale `RuleApproved` in the proposal: removed.
4. Minor 2, the false `is_guarded` claim. Verified: `manifest.rs` lines
   623 to 629 strip `+external` and test `ends_with("+guard")`. Fixed:
   one suffix parser for every reader, `cli+guard+rule` reads guarded and
   rule-started; the round-trip test covers every combination with and
   without `+external`.
5. Minor 3, three spec and design disagreements. (a) `key_missing`: the
   design's form (code plus name) in both. (b) the pattern refusal: the
   code can name the param (`gate/params.rs` lines 83 to 86, `invalid`),
   so `Refused` and the journal carry `name`, as the spec says. (c) a
   recreated rule: the design's form (replaced at its approval) in both.
6. Minor 4, check lines missing. Fixed: `CheckId` has twelve lines,
   `approved` first (informational, the rest still evaluated) and
   `run_gate` before `binding`, with their codes.
7. Minor 5, `trigger_test` from a run step. Decided the simpler way:
   refused by name, `trigger_test_refused_in_run_step`.
8. Minor 6, revocation ids repeat. Fixed:
   `<incarnation>:<seq>:binding_changed:<binding>`. Test
   `a_revocation_to_a_binding_seen_before_is_logged`.
9. Minor 7, a stamped directory without a manifest. Decided (corrected
   within this pass): it does not count toward any rule's cap. A rule's
   own starts are serialized by its lock and a rule start that fails
   before its manifest leaves no directory (eighth pass), so such a
   directory is another initiator's failed start or a rule start that
   died before any work began; neither is a started run of the rule, and
   counting it would let one failed start by a person hold every rule
   for an hour. Directories with a manifest whose origin or attribution
   cannot be established still count. Test
   `a_stamped_directory_without_manifest_does_not_count`.
10. Minor 8, `own_send` too broad. Fixed: only the exact trailing marker
    `marker.rs` writes (`render`, line 49; `carries_exactly`, line 126), at
    body paths the rule reads, naming a run this installation created.
    Tests in `marker.rs` and `evaluate.rs`.
11. Minor 9, budget and external data. Fixed: V267 warns on
    `on_no_estimate: pause` with external data; the example uses
    `continue`; the docs explain it.
12. Minor 10, three tasks.md defects: task 1.3 runs the `gate::` filter;
    the failed-preparation test is written first in task 4.2; task 0.1 no
    longer names `try_lock_dir`.
13. Minor 11, blocking locks in handlers. Fixed: the trigger routes run
    their blocking work in `spawn_blocking`. Test
    `trigger_routes_run_blocking_work_off_the_runtime`.
14. Notes on main: pre-existing, outside this change.
15. File estimate 84 (22 new, 57 changed, 5 records):
    `review/passkey_proof.rs` joins.

Tenth pass, 2026-11-20: the ninth design review (two blockers, three
majors, three minors, three notes on main), verified against the code on
efae5822. As decided, the feature was narrowed so the two mechanisms
behind both blockers and one major are no longer needed ("Cut in the
tenth pass"); the narrowing rests on facts checked first: attended
credit requires `unattended == false` (`apb-core/src/probation.rs` lines
80 to 91; the engine's writer records the mode, `apb-engine/src/probation.rs`
line 187), and a child inherits its parent's unattended mode
(`scheduler/node.rs` lines 5013 to 5015).

1. Blocker, the passkey approval cannot produce a proof. Verified:
   `take_token` returns a credential prefix, not the assertion
   (`apb-server/src/passkeys.rs` line 130), and the challenge endpoint
   resolves decision purposes only (`routes/passkeys.rs` line 480).
   Disposition: the passkey proof and the `trigger_approve` purpose are
   removed (cut item 4); `approve` is a person's command under main's
   rule, and the boundary is stated once (I1, D0 "A person's command",
   `docs/TRIGGERS.md`).
2. Blocker, a crash exposes a rule run to an older binary before `+rule`
   exists. Disposition: the mark is removed (cut item 3). A rule's run is
   always unattended and never parks, so to an older binary it is an
   ordinary unattended run and behaves correctly; no publication order is
   needed.
3. Major, an event before approval can start a run. Verified: `approve`
   read the head before its commit while the inbox appends under its own
   lock (`inbox.rs` line 343). Fixed by removal (cut item 5): `approve`
   and `enable` read no head; they delete the state before their rules
   write, and the first pass positions the rule at the head it reads
   then, starting nothing in that pass. Test
   `the_first_pass_after_approval_only_positions`.
4. Major, a live child's failure is handled with a value computed too
   late. Disposition: moot; the child marking and the failed-preparation
   special handling are removed (cut items 1 and 3).
5. Major, the journal stores values. Verified: `ConditionResult` holds
   the `equals` literal and the event type (`event_filter.rs` lines 114
   and 123). Fixed: the journal keeps `ConditionMark { index, passed }`;
   the dry test's response keeps the full results. Test
   `a_journal_line_holds_condition_marks_only`.
6. Minor, a retried position line need not have the same id. Fixed: the
   position line is an attempt record, the signed state the committed
   result, a repeated line after a crash is expected and readers show the
   last. Test `read_shows_the_last_position_attempt`.
7. Minor, the spec retained "changed permit". Fixed: "changed approval
   binding".
8. Minor, a task named an assertion contrary to the store. Fixed:
   `remove_deletes_entry_state_and_journal` with the three assertions.
9. Notes on main: pre-existing.
10. File estimate 71 (22 new, 45 changed, 4 records), down from 84.

Eleventh pass, 2026-11-20: the tenth design review (no blocker, two
majors, three minors, three notes on main), verified against the code on
efae5822.

1. Major, billing changes do not always revoke. Verified:
   `nonce_lines` returns nothing unless the billing policy is `ask`
   (`gate/billing.rs` line 84). Fixed: the binding takes
   `billing.binding_lines()`, the full effective billing decision in every
   case: the settings line, the required policy, the sorted sources and
   every predicted entry of every executor with the policy it falls
   under. Tests `a_billing_deny_revokes_the_approval` (allow to deny to
   allow; revoked at the deny, not back after) and
   `binding_lines_differ_for_allow_and_deny`.
2. Major, the first journal line may not survive a power loss. Fixed:
   when `append` creates `log/` it also syncs its parent
   `.apb/trigger-state` (and that one's parent when it created it), after
   the file and `log/` and before any state commit. Test
   `the_first_line_is_durable_before_the_state`.
3. Minor, recreation cannot clean the old journal. Disposition: no
   cleanup by `approve` or a pass; an orphan journal or state of an
   incarnation no rule names is inert and the doctor line lists it with
   its path to delete; only `remove` deletes the files of the rule it
   removes. The source-change test now expects the old journal to stay.
4. Minor, the spec contradicted the mutation contract. Fixed: the spec
   says `approve` and `enable` delete the rule's position state and only
   `remove` deletes the journal.
5. Minor, the count rationale. Fixed: a failed preparation, a rule's
   included, is handled as on main and can leave a stamped directory with
   `run_started` and no manifest (`prepare.rs` line 157); it is excluded
   from the cap because no work started there.
6. Notes on main: pre-existing.
7. File estimate unchanged: 71 (`gate/billing.rs` was already in the
   list).

Twelfth pass, 2026-11-20: the eleventh design review (no blocker, two
majors, one minor, two notes on main), verified against the code on
efae5822.

1. Major, the hourly cap can fail open. Verified: `candidate_run_dirs`
   returns no entries when `read_dir` fails (`decision/report/mod.rs`
   line 303), and `allocate_run` can still create a run in a writable,
   unlistable directory (`prepare.rs` line 576). Accepted and fixed: the
   count uses a fallible enumeration, `trigger::run_dirs_strict`; an
   unreadable runs directory or entry counts as the cap reached, held
   `rate_cap_unknown` once per blocked seq. Test
   `an_unreadable_runs_directory_holds_the_cap`.
2. Major, a duplicate key bypasses revocation. Rejected, with the
   contract restated instead of a second comparison: the binding is
   compared in exactly one place, admission of an actual start; the
   guarantee is that a rule never starts a run under a binding other
   than the approved one, and revocation happens when a start is
   attempted under a changed binding. An event answered `already_started`
   attempts no start and revokes nothing; a binding changed and restored
   with no start attempted leaves the approval valid, since every run
   still starts under the approved binding. Reason: a comparison before
   the key lookup would need a second gate resolution beside admission,
   the mechanism the eighth pass removed. Every sentence that promised
   revocation on any observed change (I1, the cases table, D1
   "Approval", the docs item, the release notes, the spec) now says
   this. Test `a_restored_binding_with_no_start_keeps_the_approval`.
3. Minor, the task's test count. Fixed: task 4.2 names the design's 37
   engine tests (1 to 34 with 21a, 21b and 24a).
4. Notes on main: pre-existing.
5. File estimate unchanged: 71.

### Implementation review

Five review rounds of the implementation (2026-11-21), each by an
independent reviewer, changed the text above where they found a rule
missing or a test that could not fail; their rules are now part of the
body and no section overrides another. Round 1's fixes (the sweep
state, start key holds, journal durability and path safety, one journal
line renderer, generated web types, V262 for a secret that cannot be
filled, `apb trigger log` and `show` printing why) went into the body
when they were made.
Round 2: every journal creation, rewrite, repair and append syncs the
file and its directories whatever a retry writes; a save never writes a
store its reader refuses (`rules_store_full`); every journal operation
goes beneath `.apb` with no link followed; a journal over its cap stops
its rule, never reads as empty; rule times in milliseconds; the
`claim_answer` race test; the client's dropped-line count; the corpus
test's permissions. Round 3: the client's drain rules; the
`journal_write` stop keeps its proof; directory syncs through the walked
descriptors; one sentence for a refused journal; a removal never answers
an error after it removed (`journal_left`); the tests that pinned them.
Round 4: tests that read `/proc` run on Linux only; the drain is bounded
whether or not bytes flow and counts toward the session's byte total;
`state_left` beside `journal_left`; the remedy names where the refusal
sits, from a typed reason; the append syncs the directory it opened;
`Millis` at every rule write; the `rate_cap` hold's `journal_write`
test; one journal read per listing. Round 5: the drain drops only the
overlong line, up to its first newline; every drained byte counts before
classification; the orphan tests assert their orphan and run on Linux
only; the non-unix append returns `OpenedAppend` and names a leaf link
`Link`. The non-unix half is read against the unix twin, not compiled
by any gate of this change. These sit in D0 "The MCP client's count of
a closed session", D1 "Where rules live" items 4 and 6.6, D1 "The state
and the trigger journal", "Views", "Commands", "HTTP", "Dashboard" and
"Tests" item 9.

After the rebase onto the merged inbox waits and global playbooks, one
review of where they meet this change found no defect: a rule's run is
unattended, so an inbox wait in it keeps its driver like any long wait,
and a first visit starts at the log's head, so the event that started
the run is not consumed again; an inbox wait's site grants no function,
so V265 never names it; a rule whose playbook is global starts through
the resolved global definition, journals the `global` origin, and keys
its start key by scope. Test 9.11 pins the last point.

## Implementation notes

Building the design met the code in the places the implementation
review records above; no decision above is changed here.
