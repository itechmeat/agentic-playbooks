# Proposal: run feedback that outlives the run

## Why

A run ends and what people learned from it mostly evaporates. Reading apb
against the question "does the next run benefit from this one?" found three
concrete gaps:

1. A real failure never becomes a regression scenario. Scenario files
   (`.apb/playbooks/<id>/evals/scenarios/<name>.yaml`, the types in
   `crates/apb-core/src/scenario/format.rs`) are only read: the loader is
   `apb_core::scenario::load` (`crates/apb-core/src/scenario/mod.rs`
   line 104) and nothing in apb writes one except the demo
   (`crates/apb-core/src/demo.rs` line 98). The journal of a failed run holds
   every answer a scenario scripts (`attempt_finished` status, failure kind,
   `rejected_for`, `node_finished.output`, `review_decided`,
   `decision_made.answers`), but turning it into a scenario is hand work, so
   it is rarely done.
2. A person's correction is used once. A review note (`review_decided.note`,
   `crates/apb-engine/src/event.rs` line 674), a note a person posts to a node
   (`supervisor_action` `context_append`, line 480 and `supervisor_action`
   line 55), a human answer to a question (`question_answered`, line 1664) and
   apb's own rejection of a claimed success (`attempt_finished.rejected_for`,
   line 309) are counted by `apb stats` as interventions
   (`crates/apb-engine/src/run_stats/interventions.rs`) but never grouped
   across runs. When the same correction is made in run after run, nothing
   offers to put it into the playbook, and after a fix nothing tells whether
   the correction stopped.
3. A run's success is judged only at its end. The baseline check reads its
   sources before the first acting node and at the finish
   (`crates/apb-engine/src/scheduler/baseline.rs`), and the run is over. Many
   routines are only proven later: a deploy is healthy an hour later, a
   customer replied within three days, a CRM deal moved to `won`, a sent
   invoice was paid. Today a person has to come back and look.

Two examples of what the change gives, one code routine and one not:

1. Code: an `apb-feature-delivery` run fails because the reviewers reject
   the same missing test three times. `apb scenario capture <run> --name
   missing-test-loop` previews a scenario that replays exactly those answers;
   the person saves it, edits `expect` to the routing the playbook should
   take, and every later version is played against it. The rejection
   recurs in two runs, so the corrections ledger proposes one prompt line for
   the implementing node; the person adds it through the playbook editor
   and marks the version that carries it. A
   `follow_up` reads the deploy's health endpoint one hour after the run.
2. Non-code: a CRM follow-up routine sends offers. A person's review note
   "always attach the price list" recurs; the ledger proposes the line, the
   person edits it into the playbook and marks the fix. `follow_up: { after: 3d, ... }` reads the
   deal's stage three days later and the run outcome shows `met` or
   `not met`; with notifications opted in the person hears about it.

## What changes

1. `apb scenario capture <run-id> --name <slug> [--node <id> ...]`, MCP
   `run_scenario_capture` (preview only) and `POST
   /api/runs/{id}/scenario-capture`: one tool-layer function builds a
   scenario from a finished run's strictly read journal, scrubbed and
   redacted, shows it as a preview, and writes it only on a second explicit
   act of a person (the CLI or the dashboard) bound to the previewed
   digest, through a no-clobber write (capability
   `scenario-capture`). What the format cannot replay faithfully is refused
   with the full list, never captured half. The scenario format gains a
   scripted timeout (`timed_out: true` on an attempt answer): the scripted
   agent ends the attempt as a timeout at once, with no waiting, so a
   timed-out attempt replays and is captured. The field is left out of the
   serialized form when false, so every existing scenario keeps its digest.
2. A corrections ledger per playbook (`.apb/corrections/<scope>/<id>.json`),
   collected at every run end next to known problems, signed, scrubbed and
   locked through the store known problems already use (extracted once into
   `apb_core::signed_store`). An entry that recurs in two runs is
   `proposed` with a draft prompt line for a person to edit and apply
   through the existing ways of changing a playbook; the person then marks
   the version that carries the fix, the entry is `fixed`, and it is
   `reopened` when its rate per run on that version does not fall below the
   base rate snapshotted at marking. Shown only to people: never in a prompt, refused from a run step
   (capability `corrections-ledger`). It reads PR 1's optional
   `attempt_finished.rejection_fingerprint` as a grouping key when present,
   and nothing else from PR 1.
3. `follow_up: { after, grace?, check }` at playbook level: one baseline-shaped
   check (connector or script source, absolute rules), read once `after` the
   run succeeded by a detached waker (the pause waker's lease pattern), or by
   `apb resume --all` when the machine was off, and journaled as a new event
   `follow_up_observed` after `run_finished`, with `met`, `not_met`,
   `unknown` or `missed` (capability `follow-up-check`). It shows in the run
   outcome, `apb stats` and an opt-in notification transition `follow_up`.
   Validator codes V183 to V186; V187 to V190 stay reserved.

## Out of scope

1. Capturing list nodes, host-mode runs, supervisor directives that move a
   run (`node_retry`, `run_continue_from`, a patch, a rebind, a revival),
   an aborted run, an execution served from the node cache, a fatal check
   rejection, a journal with an unknown event type, interrupted attempts, a timed-out script node, a signalled wait whose
   payload the journal does not hold, and an enforced baseline. The
   scenario format has no answer that reproduces them exactly (an
   interruption is a control decision, not an answer; `ItemAnswers` needs
   per-item attempts the journal keys by `item` across batches; a directive
   has no boundary; the scripted timeout of this change covers agent
   attempts only). A capture of such a run is refused with every gap named.
2. The ledger creating a candidate version from its draft. Creating the
   candidate and recording the acceptance would take two locks (the
   candidate lock and the ledger lock), and a crash between them leaves a
   live candidate the ledger does not know, which needs a recoverable
   pending record and reconciliation. In this change the draft is shown for
   a person to edit and apply with the existing tools (the dashboard
   editor, the version tools, a forward patch promoted with `apb
   candidate`), and the person marks the version that carries it.
3. A success_check as a draft fix. A check needs a script, and the ledger
   has no model to write one; the `marker` form would only restate an
   output. The draft is one prompt line, and the entry names the node so a
   person can add a check by hand.
4. Retry advice in the ledger. It is a decision model's answer, not a
   person's correction, and `apb decisions` already reports it.
5. Fuzzy grouping (similarity thresholds, stemming). Grouping is an exact
   key over normalized tokens or the engine's fingerprint, so it is
   deterministic and explainable; two wordings that do not share a key stay
   two entries, and a person dismisses the duplicate.
6. Per-entry deltas in `apb stats --compare`. Interventions there already
   count corrections per version; the per-entry count that decides `fixed`
   and `reopened` lives in the ledger, which counts every observed run per
   version itself.
7. A `change` rule in a follow-up (`decrease`, `no_new`, ...). A follow-up
   reads once and has no before value; comparing with the baseline's after
   snapshot is a later change (V186 refuses it now).
8. A `project_checks` follow-up source. The project's own tests days later
   measure the commits made since, not this run (V185 refuses it).
9. Making an older apb skip `follow_up_observed` silently. It is appended
   after `run_finished`, so an apb that predates this change finds an
   unknown event after the last checkpoint; its read-only surfaces skip and
   count it, its stop path allows it, and its engine paths (resume
   `--from-node`, a continue-from, a notify sweep) refuse with the upgrade
   message. Following it with a second `run_finished` would make every
   reader count a second end and send a second `finished` notice. The new
   event is a checkpoint for this apb and every later one.

## Impact

1. Crates: `apb-core` (new `signed_store.rs`, the known-problems store on
   top of it, new `corrections/`, new `follow_up.rs`, the baseline rule
   `check`, the scenario writer and the scripted timeout
   (`scenario/format.rs`, `scenario/validate.rs`), `validate/follow_up.rs`, the baseline rule
   helpers taking a label, `connector/sites.rs`, `notify::Transition`,
   `fsutil::write_new_no_clobber`), `apb-engine` (new
   `scenario/capture.rs`, the scripted timeout in `scenario/adapter.rs`,
   new `corrections/`, new `follow_up/`, `run_end.rs`,
   the `follow_up_observed` event, `baseline/read.rs` site, notify
   transitions and notice, `pause/resume_all.rs`, `waker.rs`, run outcome,
   run stats, the HTML report through `sections()`), `apb-mcp` (new
   `tools/corrections.rs`, `tools/scenario_capture.rs`, server tools and
   the read-only table), `apb-cli` (`apb scenario capture`, `apb
   corrections`, `__pause-waker --follow-up`), `apb-server` (routes and ts
   contract), `web` (corrections card, follow-up block, capture button,
   journal line).
2. Bundled content: none changes. Docs: `docs/EVALS.md`, new
   `docs/CORRECTIONS.md`, `docs/HOWTO-authoring.md`, `docs/NOTIFY.md`,
   `docs/STATS.md`, `docs/MCP.md`.
3. Journal: one new event type `follow_up_observed` (a checkpoint, appended
   only after a succeeded `run_finished`). No new field on any existing
   event. The ledger and the scenario files are outside the journal.
4. Behaviour changes for release notes: every run end of a playbook whose
   runs carry corrections writes `.apb/corrections/` (git-ignored like
   `.apb/problems/`); a playbook with `follow_up` leaves a sleeping
   `apb __pause-waker` process after a successful run; `apb resume --all`
   also reads due follow-ups; an apb older than this release refuses to
   resume or continue a run that has a `follow_up_observed`.
