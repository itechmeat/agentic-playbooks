# Intervention counters, run search fields and two checklist examples

Date: 2026-11-03. Status: accepted. Author-facing usage: `docs/STATS.md`
("Interventions"), `docs/MCP.md` (`playbook_stats`, `run_retro_context`,
`runs_list`), `docs/HOWTO-authoring.md` (params `search`, "Linking runs,
commits and tracker records", "Check a document against a checklist") and
`docs/scenarios/`.

## 1. Goal

1. Whoever improves a playbook needs to know how often its runs needed
   someone: a gate decided through the review channel, a question a person
   answered, an operator pause, a supervisor's retry. `apb stats` counted
   waits, not the interventions themselves, and an older journal does not
   say who decided, so a naive count reads its absence as zero.
2. A person looking for "the run for customer Acme" had only run ids and
   playbook names. A param the author marks searchable should find the
   run, without exposing a secret and without a stored index to keep in
   step.
3. Two examples show a checklist review: one where an agent fixes the
   document, one where a person does.

## 2. Interventions

### 2.1 The fold

`apb_engine::run_stats::interventions::interventions(events, index,
writer)` is a pure function of one journal, its execution index
(`crate::identity`) and its writer: the oldest apb that wrote it, from
`run_provenance.apb` and every `run_resumed.apb` (`None` when any of them
recorded none). Nothing new is journaled.

| Category | Counted from | Over |
|---|---|---|
| Planned review | `review_requested` closed by the next `review_decided` or `review_withdrawn` of its node | gate visits, split `channel`, `automatic`, `withdrawn`, per option and per `via` |
| Question | `question_answered.answered_by`; `question_abandoned` apart | questions asked |
| Automatic decision | applied `decision_made` of `completion_check`, `retry_advice`, `supervisor_triage`, `review_triage`, `routing` | decisions asked, per use |
| Operator action | pause, resume, abort, expedite `by: operator` (`by: host` is `expedite_by_host`) | runs |
| Supervisor correction | `node_retry`, `run_continue_from`, `context_append`, an interrupt, `profile_rebound`, `patch_applied`, pause, resume, abort, expedite `by: supervisor` | node executions (node-targeted), runs (run-level) |
| Engine action | `infra_retry`, `timeout_continuation`, `quota_probe`, `attempt_stalled`, `interaction_downgraded`, `retry_superseded_by_stop`, a billing hold, a refused directive, pause, resume, abort, expedite `by: engine` | never in a total |

A gate decision is automatic when an applied `review_triage` decision of
the same visit, `decided_by` (`model`, `unattended`) or `via` (`engine`,
`unattended`) says so; any other decision came through the review channel.
`triage_retry` and `retry_advice` markers are the effects of automatic
decisions and are counted once, through the decision. An unknown
`supervisor_action` name is `other`, outside every total.

### 2.2 Honest totals

1. The intervention total counts channel decisions, `human` and
   `supervisor` answers, operator actions and supervisor corrections. The
   word is "interventions", never "human interventions": `apb note` posts
   the same `context_append` as the MCP tool, so the supervisor channel
   does not tell an agent from a person. No actor field is added.
2. Absence is not zero. A run whose writer is older than 1.0.0-beta.3
   (`ATTRIBUTION_SINCE`) reads a gate decision with neither `via` nor
   `decided_by` as unattributed, which leaves its review and total figures
   unknown; an unattended answer count is unknown for it too. A pause,
   resume, abort or expedite without `by` is `actor unknown`, never the
   supervisor, and leaves the operator, run-correction and total figures
   of its run unknown. Requests, decisions, questions, directives, rebinds
   and patches are known for every run.
3. Every version count is `KnownCount { value, known_runs, runs }`, shown
   as `3 (known in 2 of 3 runs)` when they differ. Medians and means are
   over the runs where the total is known.
4. Duplicates: a request closes once (the oldest open one of its node); an
   interrupt is counted once per control entry (its `control seq`), however
   many attempts it stopped; automatic decisions are keyed by `(use, node,
   item, execution ordinal, attempt)`, so a cached replay after a resume
   counts once and the same question in a later execution counts again.

### 2.3 Surfaces

`VersionStats.interventions: Option<InterventionStats>` (absent without a
finished run), `RetroReport.interventions: RunInterventions`; the `apb
stats` text block, `--json`, `GET /api/stats`, MCP `playbook_stats` (new,
read-only, the same report), the dashboard's `InterventionsCard`,
`run_retro_context` and `{{run.retro}}`. Scenario and eval runs are left
out like every other figure (`run_history::counts_as_history`).

### 2.4 Non-goals

Proving a human; suggesting a change when interventions grow; per-person
attribution; a taxonomy of review options beyond per-option counts.

## 3. Run search fields

### 3.1 Declaration

`Param.search: bool` (`serde(default)`, written only when true). Per param
rather than a top-level list, so no name can dangle. Validator errors:
V160 a searchable param that is `secret: true`; V161 one whose name looks
secret (`token_shape::name_looks_secret`, the rule `run_params::sensitive`
applies); V162 more than 8 per playbook. Every param type is allowed. An
older apb drops the key and runs the playbook unchanged; the key is part of
the content digest like any other.

### 3.2 Values

`apb_core::run_search` (no IO): a field's value is the run's param value
from `run.yaml`, normalized by type: `text` trimmed, compared after
Unicode lowercasing; `enum` exactly; `int` as `i64`; `bool` as `true` or
`false`. A value over 200 characters is `too_long`, a credential-shaped
one (`looks_like_credential`) or one the run redactor would change (it
equals a secret of the run's tree, or the redactor withholds the run) is
`withheld`; neither is returned or matched. A missing value is `absent`,
an `int` that is not a number `invalid`. Matching is equality, terms
joined with AND, at most 8 per query, `name=value` (CLI, MCP) or
`name:value` (HTTP).

### 3.3 Which runs have fields

Only the run's own start definition declares them: the digest
`run_started.definition` journaled, read from `runs/<id>/definitions/`
(no-follow, regular file, digest checked, at most 1 MiB), never the
`playbook.yaml` a patch may have replaced, so a patch or a later version
never widens what an old run exposes. A run without a stored start
definition, without `run.yaml` or without a searchable param is
`not_indexed`; a filtered listing leaves it out when terms are given and
reports `not_indexed: N`, so "no match" never hides "not searchable". A
term against a withheld or too long value is a non-match counted as
`withheld: N`. Child runs use their own definition. A run's `scope` is
`run_provenance.origin` (`project` or `global`), `project` without one.
Scenario runs live under `.apb/scenario-runs/` and are never listed.

### 3.4 Derived, not stored

`list_runs` already reads every run's view; `list_runs_filtered` computes
the fields in the same pass, in memory. Nothing is persisted: no file to
clean, rebuild or leak, no second source of truth. The definition is read
only for a listing that asks (a filter, or `search=1` on HTTP), so the
plain listing's answer is unchanged.

### 3.5 Surfaces

`RunSummary` gains `scope` and `search` (`SearchFields`: `indexed` with
`{ name, state, value? }` per field, or `not_indexed`), present only in a
filtered listing; every row still passes the run redactor. MCP `runs_list
{ workspace?, playbook?, scope?, fields? }` (served by the read-only
server), `GET /api/runs?workspace=&playbook=&scope=&field=name:value`
(repeatable, more than 8 is 400; `search=1` lists the fields without
filtering), and `apb runs [--playbook ID] [--scope project|global]
[--field name=value]... [--json]` with the footer `N runs have no search
fields`. With any filter the answer is `{ runs, not_indexed, withheld }`;
without one it stays the plain array. The dashboard's run list has a
filter row (`RunSearchFilter.svelte`) offering the field names the listed
runs have. When a project and a global playbook share an id and no scope
is given, both match and each row names its scope.

### 3.6 Non-goals

Full-text, prefix or range search; searching node outputs; a persisted
index; re-indexing old runs against a newer declaration.

## 4. Two checklist examples

1. `examples/playbooks/design-doc-check.yaml` (code): `check` gives every
   checklist item a verdict with a quote, writes `check-report.md`
   (`results:`) and publishes `must_failed`, `should_failed`, `unchecked`
   and `coverage`; condition nodes route items without a verdict to one
   bounded re-check and then a person, no failed MUST item to `done`, and
   the rest to `revise` (the checklist protected) and `recheck` (the failed
   items only, bounded), then a `human_review` with `accept_open` (finish
   success `accepted_with_open_findings`) and `reject`.
2. `examples/playbooks/document-checklist.yaml` (non-code): the same check,
   a person edits the document and chooses `fixed`, `accept_open` or
   `reject`; no `protect`, no tests, no code.
3. `coverage` carries `CHECKLIST: <checked>/<total>`, the goal's marker:
   with `outputs.fields` the node output is the status file's JSON, so a
   marker only in the reply would not be found. The second criterion is
   manual, and one `quality` criterion asks that every fail cites the
   document. Coverage is the agent's count; apb does not parse the
   checklist, and the examples say so. A structured rules register is
   deferred.
4. Three quick scenarios each (`clean`, `must-fail-then-fixed`,
   `open-findings`; `clean`, `fixed-by-person`, `open-findings`), the
   reference scenarios `docs/scenarios/design-doc-check.md`
   and `docs/scenarios/document-checklist.md`, and
   `demo_playbooks_test.rs` validating both, playing the scenarios and
   running the code example on a stub agent to show one stored
   `check-report.md` per check execution.
