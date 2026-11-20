# Proposal: before running alone

## Why

A routine earns the right to run alone, and today nothing makes it earn
it. On main e76aa42e (1.0.0-beta.7 with people-in-the-loop #288 and typed
inbox reads #287 merged on top), two gaps stand out:

1. A list node runs every item before a person sees one. The batch loop
   admits every remaining item until none is left
   (`crates/apb-engine/src/scheduler/list/batch.rs` `run_batches`, line
   68); the only brake is the per-item retry
   (`crates/apb-core/src/list/verdict.rs` `transition`, line 161). A bad
   prompt over 200 follow-up mails or 40 files burns every item, and a
   visit whose items are mostly rejected still runs to the last one.
2. Nothing says whether a playbook is fit to run alone. The fit report has
   compatibility, setup, trust and consent and no readiness
   (`crates/apb-engine/src/fit/report.rs` `FitReport`, line 75); the
   unattended start refuses only supervised and host modes and a step of
   an attended run (`crates/apb-engine/src/gate/unattended.rs`
   `check_start`, line 49). A new version can run unattended on its first
   run.

| Decision | Code example | Non-code example |
|---|---|---|
| D1 pilot and early stop | a mechanical rename over 40 files with `pilot: { items: 3, then: review }`: three files run, the person reads them and answers `continue` | 200 personal follow-up mails with `pilot: { items: 3 }` and `stop_when: { needs_look_at_least: 10 }`: three drafts first, and the list ends after ten rejected drafts instead of two hundred |
| D5 readiness and probation | a nightly fix routine runs unattended only after 3 attended successes of its exact definition and scripts | a CRM clean-up refuses `--require-ready` while its writing node has no deterministic check |

Examples of the whole change:

1. Code: a mechanical rename over 40 files as a list node with `pilot: {
   items: 3, then: review }` and `stop_when: { needs_look_at_least: 5 }`:
   three files run, a person reads them and answers `continue`, and the
   visit ends early if five files need a look; `apb run --unattended
   --require-ready` refuses the same playbook while it has no budget.
2. Non-code: 200 follow-up emails with a pilot of 3 and an early stop at
   10 drafts needing a look; the mail routine runs unattended only after
   3 attended successes of this exact version.

Reminders for open waits (D4 of the draft) moved to the change wait-park
in design review round 2, whole. The pilot gate is a review request, so
once wait-park lands it is reminded like any gate.

This change is the second of two cut from one draft. The first,
people-in-the-loop, is merged (#288); this change uses its gate drive,
its channel checks and its pending review block and redefines none of
them. On merged main a `review_detail` is journaled only for a
`human_review` that uses `fields`, `show` or `max_scope`; the pilot gate
is a plain gate and journals none (design "Decisions taken in the closure
pass", decision 1).

## What changes

1. Carried over from the review of #288 (capability `review-scope`,
   design "Carried over from the review of #288"): a decision on a gate
   that uses `max_scope`, `fields` or `show` must name the request it
   answers. Today such a gate consumes decisions by count, so after a
   scope change withdraws request A and the gate asks again with B, a
   decision a person made on A and posted without a request is taken as
   B's. The post check now refuses a decision on such a gate that names
   no request or another request's (`decide_again`, a conflict that
   tells the caller to read the request again), and the drive consumes
   only an entry stamped for the open request, through the stamped path
   person-only and bound gates already use (without their passkey
   clause). The request a decision names is compared with the open
   request and stamped on one journal read, so a request asked again
   between a check and the stamp never receives a decision made for the
   one before. The scenario's scripted decision now names its request; the
   engine's own decisions and the dashboard already do, and the CLI, MCP
   and the HTTP route take it as an argument that every printed command
   and next action already carries. While the run's definition snapshot does not read, a run
   whose run guard names `review_fields` or `review_max_scope` refuses a
   decision the same way instead of reading the gate as a plain one.
   Plain gates keep today's behaviour and journal shape.
2. List pilot and early stop (capability `list-pilot`): `list.pilot:
   { items, then: review | continue, unattended: fail | continue }` runs
   the first items of a visit, then stops at a gate on the list node with
   the options `continue`, `revise` (the pilot items run again with the
   reviewer's note in their prompt, except an item whose send is
   unknown, which stays in the slice needing a look) and `stop`; it
   takes no field values. `list.stop_when:
   { needs_look_at_least, needs_look_share, min_settled }` ends a visit
   early, read only outside the pilot slice, after its gate, and after a
   batch's own pause or stop is honoured. Both are folded by `ListFold`,
   so resume, supervisor retry and `rerun: not_passed` read them. The
   pilot gate is a plain review request on the list node; its pending
   block carries the slice from the list fold, and its decision names
   the request it answers and is consumed only from an entry stamped for
   that request (the mechanism of item 1, so a decision made before a
   supervisor withdrew the request never answers a later one). An unattended run continues past a pilot only when it
   declares `unattended: continue` and every slice item passed. A
   scenario may answer a pilot gate in the same visit answer as the
   list's items. Validator V240 to V242. Two new
   journal events, `list_pilot` and `list_stopped`; one new run guard
   name, `list_pilot`.
3. Readiness and probation (capability `unattended-readiness`): a
   `readiness` section in the fit report (`apb adopt`, MCP
   `playbook_adopt_report`, the dashboard fit card); a start warning for
   `--unattended`, and `--require-ready`, which run preparation enforces
   on the loaded definition and every pinned sub-playbook (`not_ready`).
   Proof is only a check apb runs whose failure fails something (a
   script success check, a script item check under `on_needs_look:
   fail`, an enforced script goal criterion); boundaries are judged per
   effect domain (the working tree, each connector, and the network: an
   actor apb infers can reach it is not bounded externally unless an
   isolation this machine enforces closes the network), and a `finish`
   with a prompt is listed as an actor without a local boundary; the
   irreversible-gate walk follows failure transitions from the nodes a
   run reaches, carrying whether a gate was passed. Probation:
   `unattended: { after_attended_successes: N }` in the playbook or
   `policy.yaml`, counted from a signed probation ledger per playbook and
   project (`.apb/probation/<scope>/<id>.json`), to which every run end
   adds one entry per run id, once, after its terminal event is in the
   journal and with the outcome read from it: eligible only for a production run
   that learns, succeeded, and executed one definition from start to end
   with no patch, rebind, migration or override. The start check reads
   only the ledger: eligible attended successes whose trust digest
   (definition and scripts) and definition digest are those of the
   loaded definition, for every playbook of the run's tree; checked once
   in run preparation and refused `probation_required`, and again on
   the replacement definition of an in-run patch of an unattended run
   (with readiness when the start asked for `--require-ready`), refused
   as a patch with the same codes; a ledger that
   does not verify refuses and `apb doctor` names it. Waived by `apb run
   --waive-probation`, always asked as the playbook id typed at an
   interactive terminal and inherited by every child run, or by the
   machine `policy.yaml`. Runs that ended before this change have no
   entry and do not count. Probation holds with this apb or newer only:
   an older apb ignores the block (a recorded limit). Validator V249 and
   V250.

## Out of scope

1. Any change to the retention archive: probation has its own ledger and
   never reads run directories or archive summaries.
2. The scope at a form gate, typed fields on a gate and the derived
   diagram: merged with people-in-the-loop, which this change builds on.
   The pilot gate shows no work scope, like every plain gate.
3. Starting runs by themselves (rules on inbox events or on a time):
   the later change start-rules-and-schedules, which uses this change's
   `check_probation`, `PROBATION_REQUIRED`, `NOT_READY` and
   `fit::readiness::not_ready_detail`.
4. A `require_ready` argument on MCP `playbook_run` and the HTTP start:
   they return the readiness block; refusing on it stays a person's
   terminal flag and the later rules' own setting.

## Where the code differs from the plan

1. "Change the prompt (forward patch, then a re-run of the pilot items)":
   a forward patch only changes later runs
   (`crates/apb-engine/src/forward_patch.rs` lines 1 to 6, "The
   supervisor improves the playbook for the NEXT runs"). `revise` instead
   re-runs the pilot items with the reviewer's note as a `## reviewer
   note` section of every later item prompt of the visit. A prompt change
   in this run stays the existing in-run patch
   (`crates/apb-engine/src/scheduler/patch.rs` `apply_patch`, line 34);
   the list node renders whatever definition is in force when it is
   dispatched again, so no new code is needed for it.
2. "A validator warning for an acting agent_task with no deterministic
   check": every existing playbook with an agent step would warn. V250
   fires only for a playbook that declares the top-level `unattended:`
   block (it says it means to run alone); the readiness report lists the
   gap for every playbook.
3. `apb fit` is `apb adopt` in the code
   (`crates/apb-cli/src/main.rs` line 192, `Command::Adopt`), which
   prints the JSON of MCP `playbook_adopt_report`.
4. "Probation by exact digest": a run journals the digest of the exact
   `playbook.yaml` it executes (`run_started.definition`,
   `crates/apb-engine/src/run_definitions.rs` `digest`, line 43) and the
   version's trust digest, which covers its scripts
   (`run_provenance.digest`, `crates/apb-engine/src/scheduler/prepare.rs`
   line 701, `loaded.trust_digest()`). The ledger entry keeps both and
   the check compares both, so a changed script restarts the count (an
   earlier draft of this proposal said no run records the trust digest;
   that was wrong).
5. "Count the attended successes": overrides are not journaled; the
   start keeps them in the run's `run.yaml` (`RunConfig.overrides`), and
   with overrides it journals the digest of the serialized effective
   playbook. The ledger reads `run.yaml` for eligibility, and the
   check's definition equality refuses such a run as well.
6. "The pilot gate shows its scope": on merged main only a form gate
   journals a scope (`NodeKind::needs_review_detail`,
   `crates/apb-core/src/schema.rs` line 643). The pilot gate shows its
   slice, its round and its counts from the list fold instead.

## Impact

1. Crates: `apb-core` (new `readiness.rs` and `probation.rs`;
   `list::PilotSpec`, `list::StopWhen`; the top-level schema field
   `unattended`; `policy_file` section `unattended`; `workspace`'s
   git-ignored `probation/`; validator modules `validate/pilot.rs` and
   `validate/readiness.rs`; one new guarded feature; the scenario answer
   of a pilot gate; `effects::node_inferred` made public), `apb-engine` (new `scheduler/list/pilot.rs`,
   `fit/readiness.rs` and `probation.rs`; list fold and view, batch
   runner, drive loop, review gate and channel, progress, run outcome,
   next actions, scenario capture, fit, unattended gate, the engine's
   unattended start, `manifest.rs` (`probation_waived`,
   `readiness_required`), `run_config.rs`
   (`ChildExpectation::load_definition`), the journal's terminal
   observer (`terminal_hook.rs`, a second slot for the ledger's writer),
   the patch arm (`scheduler/patch.rs`: probation and readiness of a
   patch on an unattended run); carried over from #288: the review post check,
   the drive's consumption of a form gate's decision, the doctor's
   `reviews` row and the scripted scenario decision), `apb-mcp` (the
   unattended start's readiness answer; the `review_decide.request`
   argument doc), `apb` CLI package (`apb run --require-ready` and
   `--waive-probation`, the `probation` line of `apb doctor`, the install
   of the ledger's terminal observer in `main.rs`, one
   journal-corpus row), `apb-server` (ts contract), `web` (list progress
   panel, review panel, fit card).
2. Bundled content: none.
3. Journal: two new event types, neither a checkpoint, each safe to skip
   up to the node's `node_finished` or a `run_paused`: `list_pilot` and
   `list_stopped`. No new field on any event. `review_detail` is
   unchanged. One new guarded feature name, `list_pilot`, appended after
   `review_max_scope` (`GuardedFeature::ALL` grows from 11 to 12, or from
   12 to 13 after wait-park); it is a run guard only, not a manifest
   guard. One new state file per playbook and project, the probation
   ledger.
4. Behaviour changes for release notes: design "Release notes".
5. Built on main e76aa42e with people-in-the-loop and typed inbox reads
   merged and archived. Validator codes V240 to V242, V249 and V250 of
   the reserved V240 to V259 (people-in-the-loop holds V243 to V247; V248
   and V251 to V259 stay reserved for this change family). New names
   checked free on main: the events `list_pilot` and `list_stopped`, the
   guard `list_pilot`, the refusal codes `probation_required` and
   `not_ready` (the latter also a next-action field name, a different
   namespace), the flags `--require-ready` and `--waive-probation`, the
   `policy.yaml` key `unattended`, the store directory `probation/` and
   the MAC domain `apb-probation-v1`.
6. Size: 83 to 85 changed files (design "Size" and "Files"), inside the
   70 to 90 of the 1.0 plan.
7. The carried-over fix adds no journal event, field or guard name; the
   refusal code `decide_again` was checked free on main.
8. Compatibility (design D5 "Older apb binaries"): an apb older than
   this change ignores the top-level `unattended:` block and starts such
   a playbook unattended without probation; the playbook format has no
   way to demand a minimum apb (`requires` names files and commands
   only). From 1.0.0-beta.1 on it rejects a definition with `pilot` or
   `stop_when` (it neither starts such a playbook nor resumes such a
   run); 0.25.0 drops the two fields and runs every list item with no
   pilot gate and no early stop, the standing contract of an older
   binary meeting a new playbook field. It reads a `policy.yaml`
   with the new key as invalid, and neither reads nor writes the
   probation ledger.
9. Shared files with wait-park, which can be built in parallel and
   merged first: `validate/guarded.rs`, `policy_file.rs` and its test,
   `event.rs`, `scheduler.rs`, `next_action/unattended.rs`,
   the engine `tests/main.rs`, `journal_corpus_test.rs`,
   `docs/HOWTO-authoring.md`, `docs/GUARDRAILS.md`, `CLAUDE.md`,
   `AGENTS.md` and the generated `api.gen.ts`; each change adds beside
   the other (tasks 0.2 names the order).
