# Run feedback that outlives the run: scenario capture, a corrections ledger, the follow-up check

Design record for the run-feedback change: turning what a finished run
leaves behind into three things a later run or a person can use. It builds
on one thing the node-contracts change added, the optional
`attempt_finished.rejection_fingerprint`, and uses nothing from the
recurring routines change. Read with `docs/EVALS.md` (capturing a run as a
scenario), `docs/CORRECTIONS.md` (the ledger) and
`docs/HOWTO-authoring.md` (the `follow_up` block), which document the
user-facing side of everything below.

## Why

A run ends and what people learned from it mostly evaporates. Reading apb
against the question "does the next run benefit from this one?" found
three concrete gaps:

1. A real failure never becomes a regression scenario. Scenario files
   (`.apb/playbooks/<id>/evals/scenarios/<name>.yaml`, the types in
   `apb_core::scenario`) are only read: the loader is
   `apb_core::scenario::load`, and nothing in apb writes one except the
   demo. The journal of a failed run holds every answer a scenario scripts
   (the `attempt_finished` status, the failure kind, `rejected_for`,
   `node_finished.output`, `review_decided`, `decision_made.answers`),
   but turning it into a scenario is hand work, so it is rarely done.
2. A person's correction is used once. A review note
   (`review_decided.note`), a note a person posts to a node
   (`supervisor_action` `context_append`), a human answer to a question
   (`question_answered`) and apb's own rejection of a claimed success
   (`attempt_finished.rejected_for`) are counted by `apb stats` as
   interventions (`apb_engine::run_stats::interventions`) but never
   grouped across runs. When the same correction is made in run after
   run, nothing offers to put it into the playbook, and after a fix
   nothing tells whether the correction stopped.
3. A run's success is judged only at its end. The baseline check reads its
   sources before the first acting node and at the finish
   (`apb_engine::scheduler::baseline`), and the run is over. Many routines
   are only proven later: a deploy is healthy an hour later, a customer
   replied within three days, a CRM deal moved to `won`, a sent invoice
   was paid. Today a person has to come back and look.

Two examples of what the change gives, one code routine and one not:

1. Code: an `apb-feature-delivery` run fails because the reviewers reject
   the same missing test three times. `apb scenario capture <run> --name
   missing-test-loop` previews a scenario that replays exactly those
   answers; the person saves it, edits `expect` to the routing the
   playbook should take, and every later version is played against it.
   The rejection recurs in two runs, so the corrections ledger proposes
   one prompt line for the implementing node; the person adds it through
   the playbook editor and marks the version that carries it. A
   `follow_up` reads the deploy's health endpoint one hour after the run.
2. Non-code: a CRM follow-up routine sends offers. A person's review note
   "always attach the price list" recurs; the ledger proposes the line,
   the person edits it into the playbook and marks the fix. A `follow_up`
   reads the deal's stage three days later and the run outcome shows
   `met` or `not met`; with notifications opted in the person hears about
   it.

## D1. Scenario capture from a finished run

`apb scenario capture <run-id> --name <slug> [--node <id> ...]`, MCP
`run_scenario_capture` (preview only) and `POST
/api/runs/{id}/scenario-capture` build a scenario from a finished run's
journal, scrubbed and redacted, show it as a preview, and write it only on
a second explicit act of a person bound to the previewed digest, through a
no-clobber write. What the format cannot replay faithfully is refused with
the full list of gaps, never captured half.

### The scripted timeout (format change)

A real attempt times out when the adapter reports
`AgentFailure { class: ErrorClass::Timeout }`: the attempt loop journals
`attempt_finished.status: "timed_out"`, and a node whose last attempt
timed out finishes `timed_out`. The scenario format had no way to say
this. The attempt answer gains `timed_out: true`, optionally with
`output` (the text the attempt left, as a real timeout's partial output).
The field is left out of the serialized form when false
(`skip_serializing_if`), so the digest of every existing scenario is
unchanged.

V146 (`attempt_shape`) is extended: a timed-out attempt carries only an
optional output; `status`, `failure_kind`, `outputs`, `check`,
`read_only` or `question` beside it is an error. It may stand as the
`then` of a question.

The scripted adapter (`apb_engine::scenario::adapter`) answers
`timed_out: true` by returning an `ErrorClass::Timeout` agent failure at
once, the message `scenario: the attempt reached its deadline` plus the
output when there is one. Everything after the attempt (the `timed_out`
status, the node's final status, retries, fallbacks, the timeout
continuation, the supervisor wake `NodeTimeout`) is the real drive's,
exactly as for a real timeout. No sleep, no deadline watcher.

A timed-out attempt counts against `max_retries`, the way the drive
spends a retry on it. A deadline kill is never a free transient: when the
killed session can be resumed, the drive gives it one continuation that
consumes the retry budget, and otherwise abandons the step for the next
executor. A scripted agent has no session to continue, so the node's own
retry budget is what replays the captured run's later attempts of the
visit; the scenario's counted attempts therefore include a timed-out
answer (only `transient` and `quota` failure kinds are left out, as
before).

Wrong implementations to avoid:

1. A new `ScriptedStatus::TimedOut`. The status enum is shared with child
   and publish answers, which would then accept a status they cannot
   have; a child answer with `status: timed_out` stays a parse error.
2. A scripted `failure_kind: timeout`. A timeout is a status, not a
   failure class; the failure classifier would read the detail as
   transient.
3. `timed_out` serialized when false (every existing scenario's digest
   would move).
4. Returning a process-exit failure with a timeout-looking message: the
   attempt is then `failed`, and the replayed node would not end
   `timed_out`.
5. Sleeping until the node's timeout. The adapter returns at once; a
   scenario on a node with a 600 second timeout plays in well under it.
6. Accepting a timed-out attempt with `status` or `outputs` (V146).

### The capture fold

`apb_engine::scenario::capture` is a pure fold of what a run left plus
one read of its run config; it writes nothing. It returns the built
`apb_core::scenario::Scenario`, the exact YAML text a save writes (only
ever the serializer's output for it), the scenario digest, the playbook
id and the version the run executed, a count of the texts the scrubber
changed (never their list), and one line per fact a person should know
before saving (the run's version is not `current`, a param was withheld,
a redaction may have changed routing). When the run cannot be captured it
returns every gap, a code with an optional node and a detail, sorted by
node then code.

### Refusals before any reading

Each is a gap with no node; all are collected, not the first only:

| Code | When |
| --- | --- |
| `not_finished` | the run fold is not `Succeeded` or `Failed` |
| `aborted` | the run ended with `run_aborted`: a scenario has no answer that stops a run |
| `journal_unreadable` | the strict journal read fails: a torn or broken line |
| `unknown_events` | any event of an unknown type was skipped, wherever it sits: its meaning cannot be replayed |
| `not_production` | the manifest origin is not a production run |
| `secrets_unresolved` | the run's scrubber cannot be built (a referenced connector secret is missing) |
| `host_mode` | the journal has a `host_task_requested` |
| `baseline_enforced` | the run's playbook snapshot has `baseline_check.enforce: true` |
| `directive` | a supervisor `node_retry` or `run_continue_from`, a `patch_applied`, a `profile_rebound` or a `nodes_revived`: a directive has no boundary a scenario could script |
| `param_withheld` | a param the sensitive-params rule flags is declared without a default |

The journal is read with the strict `event::read_all`, never
`read_all_lossy_tail`: that reader skips unknown events and forgives a
torn last line, which would silently drop facts from a capture that must
be whole. `read_journal` runs once as well, only to see whether it
skipped any unknown event.

### Answers, per node kind

Visits are the execution ordinals of the journal's execution index,
written as `Visit::Nth(ordinal)`; the attempts of an execution are its
`attempt_finished` events in journal order. The captured decisions worth
naming:

- An `agent_task` (not a list node) and a `finish` with a prompt get one
  attempt answer per `attempt_finished`. A success without a rejection is
  `status: succeeded`; when the execution's `node_finished.output` parses
  as a JSON object it goes into `outputs` and `output` is left out (the
  scripted agent writes it to the status file and the engine stores it as
  compact JSON, so a template that reads `nodes.<id>.output.<field>`
  renders the same value); otherwise `output` is the text.
- A rejected attempt (`rejected_for` set) is scripted as
  `status: succeeded` with `check: reject` and the attempt's rejected
  output, never as `status: failed`: the rejection is apb's verdict on a
  success claim, and replaying it as a plain failure would change the
  node's status and visit count.
- A `rejected_output` without `rejected_for` is a fatal rejection. The
  format has no fatal check answer: gap `fatal_rejection`.
- A failure is `status: failed` with the same `failure_kind` word when
  the journal names one (`transient`, `quota`, `auth`, `budget`,
  `agent`), and plain `status: failed` when it does not.
- A `read_only_violated` before the attempt's finish becomes
  `read_only: { dirty: <paths> }` on the attempt.
- A `question_asked` inside the attempt makes it
  `{ question: { text, answer or timeout: true }, then: <the rest> }`,
  the answer from the matching `question_answered`; nested questions
  nest through `then`.
- A timed-out attempt is `{ timed_out: true }`, with its partial output,
  scrubbed, as `output`, whatever its `failure_kind`. Any other status
  (`interrupted`, `cancelled`, an unknown word) is gap `attempt_status`.
- A `script` node gets `ScriptAnswer { exit, stdout }` from its
  `node_finished` (exit 0 succeeded, 1 failed); a timed-out script node
  is gap `attempt_status`.
- A `human_review` gets `ReviewAnswer { decision, note }` from its
  `review_decided` (the note left out when empty); a `review_withdrawn`
  is a `directive` gap.
- A judge node gets the judge answers from its `decision_made` with the
  judge node use site, each answer as `{ value, p, confidence }`; a
  decision error of `unavailable`, `timeout`, `rate_limited`, `auth`,
  `budget` or `invalid` becomes a judge-unavailable answer of that kind;
  a `cancelled` decision is gap `judge_cancelled`.
- Judge edges of any node are keyed by the edge address from
  `decision_made.edge_addresses`; a decision without them (an older
  journal) is gap `judge_edge_address`.
- A `wait` maps a `wait_timeout` to `timed_out: true`, a timer wait to
  `elapsed: true`, and a signalled wait to `signal:` the payload from
  its `node_finished.output`; a wait whose payload the journal does not
  hold is gap `wait_signal`.
- A child playbook node gets `{ status, output }` (the output parsed as
  JSON, else a string); a `publish` node gets `{ decision, status }`
  from its `review_decided` (the one with the plan digest) and its
  `node_finished`.
- A list node is gap `list_node`: the format needs per-item attempts,
  and the journal keys them by item across batches.
- An execution settled from the node cache is gap `cache_hit`: a
  scenario run bypasses the cache, so the replay would ask for an
  attempt the journal does not hold.
- `start` and a `finish` without a prompt get no answer.

The scenario asserts `baseline: skip` when the playbook has a
`baseline_check` (its comparisons are not asserted) and a `goal` entry
from each script goal check the run journaled (`passed` gives `pass`,
else `fail`).

### Run fields and expectations

`schema: 1`, the id, and the title `Captured from run <run id>`.
`instruction`, `params` and `unattended` come from the run's config; a
param the sensitive-params rule flags, or that the playbook declares
secret, is left out with a warning. `decisions` carries the mode of the
run's first judge node and judge edge decision, when present.
`on_missing: error` and `on_extra: error`; no `versions` and no
`playbook_digest`, because the scenario is meant for later versions.
When the run's version is not `current`, a warning says so.

Expectations: `expect.run.outcome` `[succeeded]` or `[failed]` from the
fold; `expect.finish`, the finish node the run ended at; per node in
scope the `node_finished.status` of its last execution and its number of
executions as `expect.visits` (an agent task with two attempts in one
execution has `visits: 1` and one visit answer with two attempts); and
`expect.templates_empty: report`, because a real run may legitimately
render an empty reference and the capture does not turn that into a
failure. No `route.path` (the settlement order of a parallel batch is
not stable), no `edges`, no `joins`. Nodes in scope are the requested
ones when given (each must exist in the playbook, else gap
`unknown_node`), else every node with a `node_finished`.

### Scrubbing and redaction

Every string that goes into the scenario (outputs, review notes,
question texts and answers, rejected outputs, the instruction, param
values, script stdout) goes through the run's scrubber, and every JSON
value through its value redaction, before it is placed; each changed
text is counted in `redacted`. After building, the scrubber's refusal
check runs over the serialized scenario: anything still flagged is the
gap `credential_shaped`, with no path or text in the detail, only the
node. The YAML is produced only by the serializer, never by string
concatenation, so captured text cannot add keys to the file. Then the
YAML must parse and validate against the run's playbook snapshot with no
error; an error is a bug.

A redaction can change routing (an `output_field` edge that read the
redacted value). The preview says so in a warning whenever anything was
redacted.

### Saving

`apb_core::scenario::write_new(playbook_dir, name, yaml)` writes
`<playbook_dir>/evals/scenarios/<name>.yaml` atomically and never
overwrites: it refuses an invalid name, an existing file, and a link
below the playbook directory. It rests on the new
`fsutil::write_new_no_clobber`, which writes a synced temporary file in
the same directory and hard links it into place, so an existing target
fails with `AlreadyExists` and two racing saves of one name produce
exactly one winner; `fsutil::atomic_write` renames over an existing
file and would have let the second save win.

### The tool layer and its surfaces

One function serves every surface,
`apb_mcp::tools::scenario_capture::capture(root, run_id, args, step)`.
The arguments are the name, the optional node list, and, for a save, the
digest of the preview the person read. A preview is allowed anywhere
except a run step (the run-step guard runs first, for preview and save
alike): the capture reads run text that an agent step must not be handed
back as a tool answer to plant a file.

Saving takes a channel: a person (the CLI on a terminal or with `--yes`,
and the dashboard) or an agent (MCP). A save from the agent channel is
refused `save_by_person`: an agent outside a run can read the digest, so
a digest proves nothing about consent. A save recomputes the capture,
compares the digest with the one the person read, and refuses
`stale_preview` when they differ or the digest is missing; then it
writes through `write_new` into the playbook directory the run's
playbook resolves to, and `AlreadyExists` is the refusal
`scenario_exists`. A capture that found gaps maps to a conflict whose
message starts `not_capturable:` and lists `code (node): detail` per
gap.

Surfaces, all through this one function:

1. CLI: `apb scenario capture <run-id> --name <slug> [--node <id> ...]
   [--save] [--yes] [--json]`. Without `--save` it prints the preview
   (the YAML, the warnings, the redacted count, the digest) and, on a
   terminal, asks `Save as <path>? [y/N]`; a yes saves with the
   preview's digest. `--save` without a terminal needs `--yes`. Exit 0
   saved or previewed, 2 refused, 1 an error.
2. MCP: `run_scenario_capture { run_id, name, nodes? }`, preview only
   (the MCP argument type has no save or digest). Its description says
   to show the person the YAML; they save it with the CLI or on the
   dashboard. It writes nothing, so it is a full read-only use.
3. HTTP: `POST /api/runs/{id}/scenario-capture`; a refusal is 409, an
   unknown field 422.
4. Dashboard: on a finished production run's page, a "Capture as
   scenario" button opens a box with a name field (the grammar checked
   as typed), the preview YAML, the warnings, and a Save that posts with
   the digest.

### Wrong implementations to avoid

1. Writing the file on the first call, or a preview not bound by digest
   (a save with another capture's digest is refused and no file exists).
2. Capturing part of a run and leaving the rest to `on_missing` (a run
   with one list node and one agent task is refused with `list_node` and
   writes nothing).
3. A rejected attempt scripted as `status: failed` (the replay's node
   status and visit count would differ from the run's).
4. The node output put in `output` when it is a JSON object (the replay
   then stores the text, not the fields a template reads).
5. Unscrubbed text: a captured YAML must hold neither a configured
   secret value nor a credential-shaped token.
6. YAML assembled by string concatenation (an output holding a newline
   and `expect:` would round-trip as structure, not as a string value).
7. Visits counted from attempts instead of executions.
8. A save that checks existence and then renames (two racers both pass
   the check; the hard link lets exactly one win).
9. A timed-out attempt refused, or scripted as `failed`.
10. A lossy journal read (an unknown event type or a torn last line
    silently dropped).
11. An aborted run, a cache hit or a fatal rejection captured.
12. MCP that saves.

## D2. The corrections ledger

A per-playbook store `.apb/corrections/<scope>/<id>.json`, collected at
every run end next to known problems, signed, scrubbed and locked
through the machinery that store already used. An entry that recurs in
`MIN_RUNS` (2) runs is `proposed` with a draft prompt line for a person
to edit and apply through the existing ways of changing a playbook; the
person then marks the version that carries the fix, the entry is
`fixed`, and it is `reopened` when its rate per run on that version does
not fall below the base rate snapshotted at marking. It is shown only to
people: never in a prompt, refused from a run step.

### One signed store for both ledgers

The read, MAC, lock and write half of the known-problems store moved
into `apb_core::signed_store`, made generic over a `SignedDoc` (a MAC
domain constant, a schema number, a directory name under `.apb/`, the
entries array key, and accessors for identity, MAC and schema). Known
problems keep their public API as thin wrappers over it; the bytes the
MAC covers do not change, so a file written before the move still
verifies. Semantics are exactly the old ones: the MAC covers the
canonical JSON without the MAC, an unsigned or misplaced file reads as a
fresh empty one and is replaced on the next write, a newer schema
refuses a write, the lock is a directory lock beside the file, the write
is atomic and private.

### Types and the key

An entry is one of four kinds (`review_note`, `node_note`,
`human_answer`, `rejection`) and moves through `open`, `proposed`,
`fixed`, `reopened` and `dismissed`. A sighting is one correction of one
run, built by the engine: the kind, the node a draft fix would change,
the review gate for a review note, the grouping key, the stored text
(absent only for a fingerprint key, below), the run id, journal seq,
time, version, and whether it came through the supervisor channel
(which does not tell an agent from a person). A correction folds the
sightings of one key: the entry id (`c-` plus the first 12 hex of the
key's digest), the leading wording and the distinct wordings kept the
way known problems keeps them, run counts in total and per version,
first and last seen, the latest ten evidence references (a run id and a
journal seq), the supervisor flag, the status, and the optional fix and
decision records.

The TypeScript export of the evidence reference is named
`CorrectionEvidenceRef`: the run outcome already exports an
`EvidenceRef` of a different shape, and two types of one name cannot
share the generated contract.

The grouping key is exact, never fuzzy:

1. `tokens` lowercases the text, splits on every character that is not
   alphanumeric (Unicode, so non-English text works), drops tokens that
   hold an ASCII digit (ids, counts, line numbers, hashes) or are a
   single character, keeps the first 24 distinct in text order, then
   sorts them.
2. `key_of` is `t:` plus the first 16 hex of the SHA-256 over
   `<kind>|<node or empty>|<tokens joined by one space>`. With fewer
   than two tokens the joined text is the whole scrubbed text trimmed,
   so a one-word correction still groups only with itself.
3. A rejection that carries a valid engine fingerprint (the
   node-contracts change's normalized hash, 8 to 128 hex characters)
   groups by `f:<fingerprint>` instead. The two key shapes are told
   apart by their prefix, so they never collide; a rejection seen in old
   and new runs may form two entries, which is accepted.

"Please add the parser tests" and "add the parser tests, please" get
one key, while "Please add tests for the parser" is another (it holds
`for`); "fix line 12" and "fix line 40" get one key. Two wordings
that do not share a key stay two entries, and a person dismisses the
duplicate: grouping stays deterministic and explainable.

### What a run contributes

One pass over the journal builds the sightings:

1. A `review_decided` by a person (no deciding agent), a decision other
   than `approve`, and a non-empty note: kind `review_note`, gate the
   node. The `node` is the node that delivered into this gate visit: the
   source of the traversed edges into the gate between the gate's
   previous decision and this visit's request. Exactly one distinct
   source that is an `agent_task` or a `finish` with a prompt names the
   node; none, several (a join of parallel branches) or another kind
   leaves it empty, and an empty node never gets a draft. A publish
   consent (a decision with a plan digest) is skipped.
2. A supervisor `context_append` for a node: kind `node_note`,
   `via_supervisor: true`. The supervisor channel records no person
   (`apb note` and an agent supervisor post the same action), so a
   `node_note` entry is shown to people but never offers a draft.
3. A `question_answered` by a human: kind `human_answer`, text
   `<question> -> <answer>` from the matching question.
4. An `attempt_finished` with a `rejected_for` or a rejection
   fingerprint: kind `rejection`, text the rejection.

The text is stored through the scrubber's stored-text rule (scrubbed, at
most 300 characters). A rejection with a valid fingerprint groups by the
fingerprint; everything else by the token key over the stored text.
When the stored text is withheld (the text held a secret or a
credential shape), the sighting is kept with no text only when a
fingerprint key exists, and dropped whole otherwise: nothing derived
from the raw text (no hash, no token, no length) is ever stored. Retry
advice is a decision model's answer, not a person's correction, and is
not a sighting.

### Folding into the file

`CorrectionFile::observe` folds one ended run in. A run already in
`observed`, or that started at or before `observed_floor_ms`, changes
nothing: the floor makes the dedup exact although `observed` is capped
at 1000 entries, because a dropped run is older than every kept one and
can never count again. The run is then added to `observed` and to
`runs_by_version`, even with no sighting: that count is the denominator
of the reopen rule. Sightings are grouped by key; each key counts once
per run (total runs, runs per version, last seen, evidence, wording). A
new key opens an entry; an `open` entry at `MIN_RUNS` runs becomes
`proposed`. Entries are ordered by runs, then last seen, then id.

The engine writes through `signed_store::update` at the one run-end
hook, `apb_engine::run_end::record`, which calls the known-problems
observation and then the corrections one; every former call site of the
known-problems hook alone calls it now. It is best effort: a fault is
reported on stderr and changes nothing about the run. The journal is
read strictly (a journal this apb reads only in part is not counted:
nothing is stored for that run), only production runs count, the
scrubber comes from the run redactor (no scrubber, nothing stored), and
a run with no sighting creates no file.

### The draft

A `proposed` or `reopened` entry offers a draft for a person to edit and
apply themselves: the node and one line, whitespace collapsed, hidden
characters replaced, at most 400 characters. There is no draft when the
node is unknown or missing from the playbook, the node has no prompt,
the entry is a `node_note` (the supervisor channel), or the text was
withheld. The line is fixed per kind:

- `review_note`: `A person corrected this step in earlier runs: <text>`
- `human_answer`: `When this step asks a question, people answered in
  earlier runs: <text>`
- `rejection`: `Earlier attempts of this step were rejected by apb:
  <text>. Make sure the result passes that check before you report
  success.`

apb never applies a draft. The view shows it with the instruction to add
it to the node's prompt through the existing ways of changing a playbook
(the dashboard editor, the version tools, or a supervisor's forward
patch that a person then promotes with `apb candidate`), and then to
mark the version that carries it. The ledger creating a candidate itself
would take two locks (the candidate lock and the ledger lock), and a
crash between them would leave a live candidate the ledger does not
know, which needs a recoverable pending record and reconciliation; the
person applies and marks instead. A success check is also not a draft
fix: a check needs a script and the ledger has no model to write one,
and the `marker` form would only restate an output; the entry names the
node so a person can add a check by hand.

### After the fix

A person marks the fix with an existing version of the playbook (refused
`not_proposed` on an entry that is not `proposed` or `reopened`, refused
`unknown_version` otherwise). The fix record snapshots the base at that
moment: the fix version, the version of the entry's latest sighting, the
total runs of that base version and the entry's runs on it, and the
decision. The snapshot is why the reopen rule never needs history the
file no longer holds.

The settle rule for a `fixed` entry: once the fix version itself has
`REOPEN_AFTER_RUNS` (3) runs, the entry is `reopened` when the entry's
hits on the fix version, times the base version's total runs, reach the
base hits times the fix version's total runs (the rate per run did not
fall; integer cross-multiplication, no float). Below that it stays
`fixed`. Only the fix version itself counts (a later version is a new
definition), the base always holds at least one run (it holds a
sighting), and the view shows both rates.

A person's decisions (`dismiss` on any status, `reopen` back to
`proposed` or `open` clearing the fix, `mark_fixed`) each keep who
decided, when, and an optional cleaned reason.

### Who may read and decide

The ledger holds people's words and engine fingerprints and is shown
only to people:

1. No prompt section, no supervisor line, no `run_status` block, no
   template variable reads it.
2. Every entry point, reads included, is refused from a run step, on
   every surface.
3. MCP lists, dismisses and reopens; `mark_fixed` is a person's
   statement about a version and is offered on the CLI and the dashboard
   only.

### Surfaces

One tool layer (`apb_mcp::tools::corrections`) serves MCP, CLI and HTTP:
a view (playbook, scope, whether the file is signed, an unsigned file's
entry count, the entries each with its draft when one exists and both
rates when fixed) and a decision entry point (`dismiss`, `reopen`,
`mark_fixed { version }`; the MCP server never constructs the last).

1. CLI: `apb corrections <playbook> [--scope project|global] [--json]`,
   `show <entry>` (the draft line printed as data, with the node),
   `fixed <entry> --version X.Y.Z [--reason]`, `dismiss <entry>
   [--reason]`, `reopen <entry> [--reason]`.
2. MCP: `playbook_corrections { id, scope?, entry? }` (a full read-only
   use) and `correction_decide { id, scope?, entry, decision:
   dismiss|reopen, reason? }`. The descriptions say: decide only on the
   person's explicit word; never put a draft into a playbook on your
   own.
3. HTTP: `GET /api/playbooks/{id}/corrections` and
   `POST /api/playbooks/{id}/corrections/{entry}/{action}` with `action`
   `fixed`, `dismiss` or `reopen`; a refusal or a run step is 409, an
   unknown action or field 422.
4. Dashboard: a corrections card on the playbook page next to the
   known-problems card: entries by status, the leading text, runs, `via
   the supervisor channel` when set, the draft line with a copy button
   and the node it is for, "Mark fixed in version" with a version picker
   and a confirm box, Dismiss, Reopen, and for `fixed` and `reopened`
   both rates.

### Wrong implementations to avoid

1. A second copy of the MAC and lock code (both stores are served by
   `signed_store`; a pre-move known-problems file still verifies).
2. Counting a run twice, or counting a run below the observed floor.
3. A denominator only from runs with corrections (base 2 of 2, then
   three runs on the fix version with the correction in one stays
   `fixed`; in three it is `reopened`).
4. Grouping on raw text (two notes differing in a secret value are one
   entry, and the stored text holds no secret).
5. A flagged text kept or hashed (a note that is only a
   credential-shaped token adds no entry, and the file holds neither the
   token nor a digest of it).
6. The draft shown to an agent, or applied by the ledger (a run step is
   refused; after `fixed` the playbook store holds no new version and no
   candidate pointer).
7. A wrong delivering node (a gate fed by a join of two agent tasks gets
   no node and no draft; a gate fed by one agent task names it even when
   another branch finished later).
8. A supervisor note worded as a person's (no draft).
9. Retry advice decisions or `approve` notes counted.
10. A lossy read counted (an unknown event after its last checkpoint
    adds nothing and writes no file).

## D3. The follow-up check

`follow_up: { after, grace?, check }` at playbook level: one
baseline-shaped check (a connector or script source, absolute rules),
read once `after` the run succeeded by a detached waker (the pause
waker's lease pattern), or by `apb resume --all` when the machine was
off, and journaled as a new event `follow_up_observed` after
`run_finished`. It shows in the run outcome, `apb stats` and an opt-in
notification transition `follow_up`. Validator codes V183 to V186; V187
to V190 stay reserved.

### Schema

```yaml
follow_up:
  after: 3d
  check:
    id: deal_won
    description: the deal moved to won
    connector: { name: crm, function: get_deal, args: { id: "{{params.deal_id}}" } }
    observe:
      - { id: stage, type: field, path: deal.stage, expect: { equals: won } }
```

One follow-up per playbook, so one per run. `after` is a duration from a
minute to 30 days; `grace` (default a day, at most a week) is how late a
read may still be taken. The check is today's `baseline_check` shape, so
its source rules, observation grammar and template checks all apply as
they are.

### Checking a rule without a before value

`apb_core::baseline::rule::check` checks one observation under a rule
with no before value: `equals`, `at_most`, `at_least`, `empty` and
`contains` only; a `change` rule is unknown, because a follow-up reads
once and has no earlier value to compare with (comparing with the
baseline's after snapshot is a later change). `regressed` is always
absent, and the detail is the observed value as people read it; an
observation that could not be read is unknown.

### Validator V183 to V186

The baseline rule helpers (`check_structure`, `check_source`,
`check_observations`, `check_templates`) take a label now ("baseline
check" or "follow-up check") and run on the follow-up check too, so its
V62 to V65 diagnostics carry the right name.

| Code | Severity | Rule |
| --- | --- | --- |
| V183 | error | `after` is not a duration, or is under a minute or over 30 days |
| V184 | error | `grace` is not a duration, or is under a minute or over a week |
| V185 | error | the check's source is `project_checks`: the project's own tests days later measure the commits made since, not this run |
| V186 | error | an observation's rule is a `change` rule |

The check that reads a connector reads it at the new engine site
`@follow_up`, added to the one connector-site walk, so the gate, the
manifest grants, preflight, effects and V23 to V26 see it with no other
change; V68 already refuses a node id starting with `@`.

### The follow_up_observed event and journal compatibility

`EventPayload::FollowUpObserved` sits next to `BaselineCompared`: the
check id, a status (`observed`, `unknown` with a reason, or `missed`),
the observations, one result per observation (rule, verdict, detail), a
verdict (`met` only when every result is met; `missed` and `unknown`
statuses give `unknown`), the seq of the `run_finished` it follows, the
due and observed times, and the input digest. Every field is
serde-defaulted; the variant is additive and a checkpoint.

1. This apb: the event is known and a checkpoint, so every reader works
   as before. An event type a later apb writes between `run_finished`
   and `follow_up_observed` is settled by the checkpoint after it; one
   written after `follow_up_observed` is not, and the strict read
   refuses it, as for every checkpoint.
2. An apb older than this change: the event is unknown and no checkpoint
   follows it. Its read-only surfaces skip and count it, its stop path
   allows it, and its strict read refuses with the "written by a newer
   apb" message, so a resume `--from-node`, a continue-from and a notify
   sweep of that run refuse there, and its `apb resume --all` skips the
   run. This is the accepted cost; the release notes say it.
3. An older apb that starts or drives a run of a playbook with
   `follow_up` drops the block on load and arms nothing. The run's
   snapshot still holds the block, so this apb's `apb resume --all`
   finds the follow-up pending and reads it (within `grace`) or journals
   it `missed`. No guarded feature is added: the follow-up acts only
   after the run ended, so refusing an older apb mid-run would protect
   nothing. The docs say that a follow-up is guaranteed only when this
   apb or a later one runs `apb resume --all` or drives the run's end.
4. A resume of a finished run (`--from-node`) in this apb appends after
   the event; the fold ignores it; a later `run_finished` does not
   re-arm a follow-up once one `follow_up_observed` exists. A second
   `run_finished` after the event would make every reader count a second
   end and send a second `finished` notice, which is why the event is
   appended only after a succeeded `run_finished` and never followed by
   another one.

### When it is due, and who reads it

The state fold (`apb_engine::follow_up::state`) is `None` when the run's
playbook snapshot has no `follow_up`, the run is not production, or its
latest end is not a success (the fold succeeded, the latest
`run_finished` succeeded and no `run_resumed` follows it, and no
`follow_up_observed` exists); otherwise `Pending`, with the finished
seq, the due time (the `run_finished` time plus `after`, wall clock: a
finished run has no paused interval) and the grace; otherwise
`Observed`. For tests, `APB_FOLLOW_UP_AFTER_MS` replaces `after` (a
release build ignores it), the way the notify retry gap is overridden.

One reader, `follow_up::observe_due(root, run_id, now_ms)`:

0. Trust first: the same resume gate check every engine resume makes. A
   run directory this machine did not create (one that came with a
   repository) is refused. A missing manifest reads as production, so
   the origin stamp is what keeps an imported run's script from running.
   A refusal journals nothing, reads nothing, and prints one stderr line
   in a sweep.
1. It takes the run's waker lease `follow-up-<finished_seq>`; a held
   lease returns: another process is reading it.
2. Under the lease it re-reads the journal and returns unless the state
   is still `Pending` for the same finished seq, no driver is alive, and
   the due time has passed.
3. Past the grace it journals `status: missed` (nothing was read,
   verdict `unknown`, the reason says how late and why: the machine was
   off or apb did not run). The source is never read late without
   limit.
4. Otherwise it reads the source once and checks each observation in
   memory, on the raw values.
5. Before the append, everything stored is scrubbed with the run
   redactor's scrubber: each observation through the value redaction,
   each result detail and the reason through the text scrub. When the
   scrubber cannot be built (the run's secrets are unresolved) the event
   is journaled `unknown` with no observations and a reason that says
   so: the verdict is computed from raw values, the journal never holds
   one.
6. Then the notify sweep of a finished run, exactly as the drive's end
   does it, so a follow-up notice also goes out on a machine that was
   off.

The source read reuses the baseline reader with a read context built
from the run. The context gained a `site` field (the drive and the list
check pass the baseline site as before, no behaviour change there) so
the connector call resolves as `@follow_up`. The workdir is the run's
resolved worktree when it is still a directory, else the execution
root; a script source whose run worktree is gone is `unknown`, reason
"the run's working tree is gone". Params and spawn environment come from
the run config, the scrub list from the referenced environment names,
the phase is `follow_up` (a script gets
`APB_BASELINE_PHASE=follow_up`), and the tree guard is on. A scenario
run never reaches here (it is not production), so the scripted branch of
the reader is never taken.

The connector call resolves the `@follow_up` grant the manifest recorded
at start and the live trust of the connector now, as every call does: a
connector revoked or changed since the run is refused, and the snapshot
is `unknown` with that reason.

### Arming the waker

1. The drive, on a succeeded run and after the notify waker, spawns
   `apb __pause-waker --root <root> --run-id <id> --follow-up
   <finished_seq>` when the state is `Pending`.
2. `WakerFor::FollowUp(seq)`: lease name `follow-up-<seq>`, arguments
   `--follow-up <seq>`, the "what" line "a follow-up check waits for its
   time" and the "instead" line pointing at `apb resume --all`.
3. The waker body takes the lease (a held one ends it at once, as for
   every waker), acknowledges the hold, polls the journal on the same
   interval the pause waker uses while the state is `Pending` for its
   seq and the due time has not passed, then drops its lease and calls
   `observe_due`, which takes the lease again and re-checks the state
   under it. Holding the lease into `observe_due` would make that call
   find it held and journal nothing. It ends at once when the state is
   no longer `Pending` for its seq (a resume ended the run's story).
4. The CLI's hidden pause-waker command takes `--follow-up <seq>` beside
   `--seq`.
5. `apb resume --all` sweeps finished runs too: a due follow-up is read
   now (or journaled `missed` past its grace); one still ahead gets a
   waker unless the lease is held. Nothing is listed for it, as for the
   notify sweep.

### Surfaces

1. Run outcome: a `RunFollowUp` view (the check, its description, the
   source kind, a status `pending`, `observed`, `unknown`, `missed` or
   `not_scheduled` when the run did not succeed, the due and observed
   times, the verdict and reason, one result per observation). The run
   readout and the outcome summary carry it; the text outcome and the
   HTML report print a `Follow-up` section after `Baseline` through the
   one `sections()` path, and MCP `run_status` adds `follow_up` next to
   `baseline`. Redaction follows the baseline block's path. A `not_met`
   follow-up adds a `FollowUp` needs-a-look reason, so the run reads
   "needs a look"; `unknown` and `missed` do not.
2. Dashboard: a follow-up block in the run's verdict panel (status, due
   or observed time, each result) and a journal line for
   `follow_up_observed`.
3. `apb stats`: per version, a follow-up block with observed, met, not
   met, unknown, missed and pending counts, the text line
   `follow-up deal_won: met 3/4 (75%), unknown 1, missed 0, pending 2`,
   and `--compare` gets the met rate of both sides (absent when either
   has no observed run). The met rate is over observed runs; unknown and
   missed are counted apart, never as met.
4. Notifications: the transition `follow_up` is opt-in (in the full
   list, not in the defaults, like `host_task`), opened at a
   `follow_up_observed` seq and closed on a `run_resumed` or
   `run_started` like `finished`. The notice names the run, the playbook
   and the verdict, with one line per result. A follow-up notice goes
   out for every verdict: the person opted in.

### Wrong implementations to avoid

1. `unknown` or `missed` counted as met anywhere (verdict, stats, needs
   a look).
2. Reading the source late without limit (past the grace, `missed` and
   no connector call).
3. Two observations of one run (a waker and `resume --all` at once): the
   lease and the re-read under it journal one event.
4. A follow-up after a failed run (the state is `None`, no waker).
5. The due time taken from `run_started` or from the waker's start (it
   is the `run_finished` time plus `after`).
6. Not a checkpoint, or a checkpoint read the wrong way round (an
   unknown event between `run_finished` and the follow-up reads; after
   it, the read refuses).
7. `change` rules accepted (V186).
8. The `@follow_up` grant left out of the site walk (the call would be
   refused at follow-up time).
9. A waker that keeps sleeping after a resume (a `run_resumed` ends it
   without journaling).
10. A follow-up for an imported run (the origin stamp check keeps its
    script from running).
11. Raw values journaled (the verdict is computed from raw values; the
    journal holds scrubbed text, and with unresolved secrets nothing).
12. A waker that keeps its lease into `observe_due` (the observation
    would find the lease held and journal nothing).

## Out of scope

1. Capturing list nodes, host-mode runs, supervisor directives that move
   a run (`node_retry`, `run_continue_from`, a patch, a rebind, a
   revival), an aborted run, an execution served from the node cache, a
   fatal check rejection, a journal with an unknown event type,
   interrupted attempts, a timed-out script node, a signalled wait whose
   payload the journal does not hold, and an enforced baseline. The
   scenario format has no answer that reproduces them exactly (an
   interruption is a control decision, not an answer; list items need
   per-item attempts the journal keys by item across batches; a
   directive has no boundary; the scripted timeout covers agent attempts
   only). A capture of such a run is refused with every gap named.
2. The ledger creating a candidate version from its draft (two locks and
   a crash window; the person applies the draft with the existing tools
   and marks the version).
3. A success check as a draft fix (a check needs a script; the `marker`
   form would only restate an output).
4. Retry advice in the ledger (a decision model's answer, already
   reported by `apb decisions`).
5. Fuzzy grouping (similarity thresholds, stemming): the exact key keeps
   grouping deterministic and explainable; two wordings that do not
   share a key stay two entries, and a person dismisses the duplicate.
6. Per-entry deltas in `apb stats --compare` (interventions there
   already count corrections per version; the per-entry count that
   decides `fixed` and `reopened` lives in the ledger, which counts
   every observed run per version itself).
7. A `change` rule in a follow-up (a follow-up reads once and has no
   before value; comparing with the baseline's after snapshot is a later
   change, V186 refuses it now).
8. A `project_checks` follow-up source (V185).
9. Making an older apb skip `follow_up_observed` silently (see the
   journal compatibility section: the accepted cost is a refusal on the
   resume paths, not a silent skip).

## Release notes

The change is not tagged here. User-visible changes to list when it
ships:

1. `apb scenario capture <run-id> --name <slug>` on the CLI, the HTTP
   API and the dashboard (MCP previews only): a finished production run
   as a regression scenario, scrubbed and redacted, saved only by a
   person on a second act bound to the previewed digest, never
   overwriting an existing scenario.
2. The scripted timeout answer `timed_out: true` in scenario files; the
   digest of a scenario without it is unchanged.
3. The corrections ledger, `.apb/corrections/` (git-ignored like
   `.apb/problems/`): every run end of a playbook whose runs carry
   corrections writes there; entries that recur become drafts a person
   applies through the existing ways of changing a playbook and marks
   per version, with a reopen check on the fix version's rate.
4. `follow_up:` at playbook level: a successful run leaves a sleeping
   `apb __pause-waker` process that reads the check once after `after`;
   `apb resume --all` reads a due follow-up (or journals it `missed`
   past its grace); the run outcome, `apb stats --compare` and the
   opt-in `follow_up` notification show the verdict.
5. An apb older than this release refuses to resume or continue a run
   that has a `follow_up_observed` in its journal.
