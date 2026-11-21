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
   only a busy start key and the hourly cap (counted from the runs' own
   launch records) wait for the next pass. Nothing is queued or merged.
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
