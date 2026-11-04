# Executor identity and the executor report

Date: 2026-11-03. Status: accepted. Author-facing usage: `docs/STATS.md`
("Per executor digest", "Executor report", "The run estimate"),
`docs/HOST-INTEGRATION.md` ("The model actually used"), `docs/DECISIONS.md`
(the `retry_advice` label and "Stored thresholds"), `docs/MCP.md`
(`run_status`, `run_retro_context`) and `docs/QUALITY.md` (stats).

## 1. Goal

A node's profile names a chain: its own executor, its fallbacks and any
routed tiers. Until now the journal said that a chain moved on, but rarely
why, and the figures built on it mixed things that differ:

1. The reason a step closed was taken when the process exited, so a
   rejected `success_check`, a protected-path change, a quota or a run of
   failed infrastructure retries all read as `failed` on the next step.
2. A `--now` pause or an abort during an attempt read as a failure, so the
   chain could spend on the next fallback after the operator stopped the
   run. The closing answer's composer even let an aborted run end
   succeeded.
3. A host attempt was credited to the model its last submission named,
   although an attempt can span several host tasks (a question and its
   follow-ups) that report different models or none, and the profile's
   hint was sometimes shown as the model that ran.
4. Stats and estimates grouped runs by profile and bundle only, so a run
   with a run-local `ephemeral_executor` override equal to its base was
   merged with the base chain.
5. Retry-advice labels compared agent and model, which a host hint or a
   rebind can change without changing what ran.

This change records why each step was entered, ends the chain on a stop,
folds every attempt into one identity, gives every binding an executor
digest, and reports per executor what each chain step did.

## 2. Close reasons

1. `CloseReason` is one vocabulary for `fallback_triggered.reason`,
   `attempt_started.exec.entered.reason` and the host task's `fallback_of`:
   `failed`, `expired`, `timed_out`, `interrupted`, `cancelled`,
   `question_timeout`, `unstartable`, `superseded`, `check_rejected`,
   `protected_paths`, `quota`, `auth`, `budget`, `transient`,
   `agent_failed`, `routing`, `run_paused`, `run_aborted`,
   `supervisor_interrupted`. On the wire it is a plain string, read from
   any string, so a reason a newer apb names never fails the journal; a
   reason this apb does not know is shown only when it is a short slug
   (`[a-z0-9_]{1,40}`), else as `other`.
2. The close is decided at the attempt's final verdict, where
   `attempt_finished` is journaled, not at process exit. Precedence, first
   match wins: a stop (`run_paused`, `run_aborted`,
   `supervisor_interrupted`); a host closure other than succeeded and
   failed; `unstartable`; `protected_paths`; `check_rejected`; `quota`,
   `auth`, `budget`, `transient` from the failure classification;
   `timed_out`; `interrupted`; `agent_failed`; `failed`. A reason the drive cannot
   establish reads `failed`, never a guess; agent text never chooses it.
3. The first attempt of every step after the first one walked journals
   `exec.entered: { from_step, closed_attempt, reason }` at spawn, where
   `closed_attempt` is the journal seq of the attempt that closed the step
   before. Every `fallback_triggered` carries the same reason and
   `executed_by` (`cli`, `host` or `scenario`). A tier cascade keeps
   `routing`. A deferred retry re-enters its own step and records no entry.

## 3. Stops end the chain

1. After a failed attempt the drive reads its stop context: a run abort,
   or a `--now` pause marked graceful by the pause fanout, ends the chain
   walk with no transition and no next step; the attempt is journaled
   `interrupted`. A supervisor interrupt names itself
   (`supervisor_interrupted`) and keeps the retry and the fallback.
2. The closing answer's composer has its own pausable kill flag, so a
   `--now` pause reaches it. A stopped composer writes no transition, no
   answer, no `node_finished` and no result snapshot; the resume composes
   again from the first step. A composed answer that raced a pause is
   kept.
3. The composer's attempts journal the model apb requested (the hint in
   host mode) and their chain position like every agent attempt, and in
   host mode its transitions name `host` with the hints as models.

## 4. The identity fold

1. `execution_identity::fold` folds a run's journal into one
   `ExecutionIdentity` per attempt: the node and item, the attempt
   number, the binding snapshot and step it ran on, the digests of that
   binding, the model apb requested and, for a host attempt, every host
   task with the model its submission reported (none when the engine
   closed the task).
2. The reported model of a host attempt is `one` only when every task
   reported a model and all are the same model; `mixed` when all reported
   and they differ; `partial` when some reported none; `none` otherwise.
   Only `one` credits an outcome to a model; a hint never does.
3. An attempt journaled without `exec` (an older apb) has no position:
   every surface says "position not recorded" and never attributes the
   current binding to it.
4. Every run surface shows the same row, `ExecutionIdentityView`:
   `apb runs <id>` (one `model` line per attempt and a `hints:` line),
   MCP `run_status` and `run_report` (`attempt_models`, `model_mismatch`,
   `hint_follow`), the HTTP run detail and the dashboard's attempt panel.
   `apb doctor --run` warns on a mismatch in the same words and lists
   each step an attempt entered with its reason. The retrospective counts
   `fallback_reasons` per node and names the model's source (`cli`,
   `host`, `mixed`, `partial`, `unreported`).

## 5. The executor digest

1. `executor_digest` is `x1:` plus a sha256 over the binding's chain in
   order (agent, model, invocation, SOUL delivery), its routed tiers and
   whether it is an ephemeral override. Machine fields (the resolved
   executable and its fingerprint) are left out, so a reinstall elsewhere
   keeps the group.
2. An override equal to its base chain has its own executor digest: what
   ran was chosen for this run. The manifest's `profile_digest` and
   `bundle_digest` are unchanged, so the trust gate and the run permit
   compare what they compared before; the manifest bytes do not change.
3. The digest is recomputed from the manifest entry, so runs an older apb
   made are grouped by executor too. One resolver
   (`manifest::resolve_executor`) serves the run start, a rebind and the
   pre-start estimate key, so the two keys cannot drift.

## 6. Stats and estimates

1. `apb stats` digest groups and `--compare` changes name the executor
   and an override. Quality rows of a node criterion list their levels
   per executor group (`by_executor`) when the graded runs bound the node
   to more than one; promotion and the comparable key never read it.
2. The run estimate matches a node's history by profile, bundle and
   executor digest and counts each mismatched run under the outermost part
   that differed (`profile_mismatch`, `bundle_mismatch`,
   `executor_mismatch`); its text names the part (`no history with this
   executor`, `skill set` or `profile`). `apb run --estimate` takes
   `--overrides`, and a run with overrides starts with the estimate of its
   own bindings. `estimate.json` carries `key_version: 2`; a snapshot
   without it reads as the profile-only key and says so.

## 7. The executor report

Per node, `executors` lists each executor group with:

1. `steps`: one row per chain position (binding, step, source,
   `executed_by`) with attempts, successes, the median attempt time, the
   cost over the attempts that reported one and its split by basis, the
   reported models and the closes by reason. Attempts without a position
   form one row with no `step`. A declared step no attempt reached is
   listed as "never ran", only when every attempt's position is recorded:
   an attempt of unknown position may have run on any declared step.
2. `transitions`: from step, to step, reason, count. A transition an
   older apb journaled without a reason reads `not_recorded` and is never
   reclassified.
3. `hint_follow` for host groups: of the hinted host attempts, how many
   reported the hint, another model, or none. The rule is one function
   shared with the run surfaces.

The text lists a node when a group has more than one step, a transition
or a hint line; the JSON always carries the groups. The dashboard's stats
card shows the same report, with `not recorded` kept apart.

## 8. Retry-advice labels and their thresholds

1. A retry-advice decision is labelled under a rule chosen from its
   journal alone: `exec/1` when both attempts journal `exec` (a CLI
   attempt is the same executor when its binding and step are; a host
   attempt when both reported one and the same model; unlabelled
   otherwise) and `exec/0`, the agent and model comparison, for an older
   journal. `apb decisions report` keeps one row per rule.
2. Thresholds of a use whose labels have rule versions live in
   `decisions-thresholds-v2.yaml` with their rule, and are looked up only
   there under the current rule, so a threshold measured under `exec/0`
   or stored in the legacy file never enforces `exec/1`. The legacy file
   is never written in a new shape, so an older apb keeps reading it.
3. `apb decisions thresholds set` is refused inside a run step (exit 3):
   only a person stores a threshold.

## 9. Journal and older binaries

No new event type and no new file in the run directory. The new fields
(`exec.entered`, `fallback_triggered.executed_by`, the composer's `exec`
and `model`) are optional and dropped by older readers; nothing routes on
a reason. The journal corpus checks it with released binaries: row X1 (a
new-binary run with a rejected primary, its fallback, a paused composer
and a synthetic host transition is read, resumed and stopped by beta.1 and
0.25.0, routing as the new binary does) and row X2 (a beta.1 fallback run
read by the new report: the transition `not_recorded`, no positions, the
executor digest from the manifest, no declared step claimed as never
run).

## 10. Non-goals

The agent CLI's own version in the identity; changing which failures are
retried or how they are classified; rewriting historical journals or
manifests; detecting a model a CLI does not report; recommendations to
reorder or drop fallbacks; changing the quality promotion key; whether a
host's report of its model is true.
