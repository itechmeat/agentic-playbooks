# Authoring playbooks (tier 2)

This is the on-demand detail an agent pulls via `playbook_howto` only when it is
actually creating or reworking a playbook. It is not needed for ordinary
matching or running.

## playbook.yaml structure

A playbook is a YAML document with these top-level fields:

- `schema` (int, default 1)
- `id` (string, machine id, English, kebab or snake)
- `name` (string, display name, any language)
- `description` (string, free text, any language; not used for matching)
- `version` (string, `X.Y.Z`)
- `params` (list): each `{ name, type, label?, options?, default? }`
- `defaults` (profile, retries, timeout, on_failure)
- `trigger`, `requires`, `effects`, `goal` (see below)
- `nodes` (list) and `edges` (list)

Every save through apb (`playbook_create` / `playbook_update`, the dashboard
editor, `apb import`) creates a new immutable version, except when the
definition equals the current version apart from its `version:` field and
formatting: then nothing is written, the current version and its trust stay as
they are, and the answer says `unchanged: true`.

## Visual editor and graph check

The dashboard renders a playbook as a top-to-bottom graph, and the canvas is
the fastest way to see whether a multi-output node actually reads the way the
edge declaration intended. After wiring or editing a node with several exits,
open the playbook in the visual editor and confirm the fan-out branches run
left-to-right in declaration order and do not cross. When `agent-browser` is
installed in the workspace, prefer driving that check through it: load the
graph page and read the rendered branches back, the way a human reviewer
would. This is advisory only, no code path depends on `agent-browser`; a
plain manual look is equally valid.

## Deleting and restoring a playbook

Deleting a playbook (the dashboard's Delete, MCP `playbook_delete`) moves its
whole folder to `.apb/trash/<id>-<deleted_at_ms>`; its runs stay where they
are. The trash is listed and restored through one core path
(`apb_core::versioning::restore_from_trash`) from every surface:

- dashboard: Playbooks, then Trash, grouped by project, with the deletion time
  and a Restore button per entry;
- CLI: `apb trash list [--json]` and `apb trash restore <name|id>` (exit 1 on a
  conflict, 2 when nothing matches);
- MCP: `playbook_trash_list` and `playbook_trash_restore`.

A restore takes a trash entry name or a playbook id (its latest deletion) and
brings back every version, the `current` pointer, layouts and provenance. The
restored current version is trusted like any save through apb (its digest is
approved) when it has no scripts; a version with scripts keeps whatever
approval its digest already had. When a playbook with that id exists again, the restore is refused
and nothing moves: the listing flags such an entry (`conflict`), and the
dashboard says so on its card. Delete or rename the newer playbook first.

## Executor binding: profiles

An `agent_task` node binds its executor only through a profile. A profile
(`.apb/profiles/<name>/`, or global `<config>/profiles/<name>/`) carries the
agent, model, fallback chain, role prompt (SOUL.md) and skills. A node
references it by name (scope auto) or `{ name, scope }`:

```yaml
nodes:
  - { id: build, type: agent_task, prompt: "implement {{params.task}}", profile: architect }
  - { id: review, type: agent_task, prompt: "review the diff", profile: { name: reviewer, scope: global } }
```

`defaults.profile` supplies a fallback for nodes without their own. Create and
edit profiles with the `profile_*` MCP tools, `apb profile write` / `apb profile
edit`, or the web profile API (`/api/profiles`); see PROFILES.md. Legacy
`schema: 1` playbooks with `executors` are migrated with `apb migrate` (a
migrated reference to a global executor becomes a global-scope profile).

## Connectors (external services)

An `agent_task` node may also bind connectors: named, per-node grants to reach an
external service (a tracker, a messenger) over declarative HTTP, with secrets
resolved by `apb` and never handed to the agent. Use the same two-form pattern as
skills:

```yaml
nodes:
  - id: triage
    type: agent_task
    profile: dev
    connectors:
      - mock-tracker                 # everything allowed
      - { name: github, functions: read_only, max_calls: 20 }
```

`functions` is an explicit list or the string `read_only`; `accounts` allowlists
which configured accounts the node may use; `max_calls` is an optional call
budget counted per executor attempt, not per run, so the worst case for one
visit to the node is `attempts x max_calls`. Every spawn counts: `max_retries
+ 1` attempts per step of the fallback chain, plus the infrastructure-retry
backoff steps (two more by default, and they do not advance `max_retries`),
plus one per question and answer round under `reprompt` or `resume` interaction
(not under `live`, which spans every round in one attempt). A script node runs
no executor, so its count restarts per visit instead.

The binding is covered by the playbook digest, but the connector folder
and each account are digest-pinned separately and must be approved before a run.
Installing connectors, configuring accounts, secrets, trust, and the
`apb connector` CLI are covered in CONNECTORS.md.

## Success checks

An `agent_task` node may carry an optional `success_check` that gates the
agent's own success report. When the agent reports success, the engine runs
the check before the node advances; when the check fails, the attempt is
treated as a failure and flows through the normal retry and failure-edge
machinery. Absent, the self-report is trusted as is. Two forms:

```yaml
nodes:
  # Script form: an sh script under the version's scripts/ whose non-zero
  # exit fails the node even when the agent reported success.
  - { id: build, type: agent_task, prompt: "build", profile: dev, success_check: "scripts/verify.sh" }
  # Marker form: the literal string must appear in the node output, else the
  # reported success is rejected.
  - { id: wave, type: agent_task, prompt: "run the wave", profile: dev, success_check: { marker: "WAVE-COMPLETE" } }
```

The marker form requires the agent to emit an explicit completion marker in
its output, so an attempt that reports success while its output only contains
interim text is rejected with `success report rejected: completion marker
<marker> not found in output`. A `success_check` on any node other than
`agent_task`, or a marker that is empty, is a V33 validation error; a script
path outside `scripts/` is a V12 error.

A rejected success report consumes a retry like any other failure, so
`max_retries` is honored: the attempt is spent, and the node retries or takes
its failure edge as configured. The discarded report text is preserved on the
attempt and exposed to downstream templates as
`{{nodes.<id>.rejected_output}}`, so a fix or review node can read exactly what
the rejected attempt claimed.

### Protected paths (protect)

`protect` on an `agent_task` lists globs of files the attempt must not
change, relative to the node's working directory (the run's working tree, or
the node's own `workdir`):

```yaml
- id: fix
  type: agent_task
  prompt: "Make the failing test pass without touching the tests"
  protect: ["tests/**", "docs/spec/*.md"]
```

Before every attempt the engine copies the matching files into the run
directory; after the attempt's process has ended it lists the matches again.
A file that was modified, deleted or added under a protected glob fails the
attempt with `protected path modified: <path>`: a normal retry that keeps the
report as `rejected_output`, exactly like a failed success check. The engine
restores the protected files from its copy (an added one is removed) before
anything else runs, so the retry and every later node start from the files
as they were, without needing a separate worktree. One
`protected_paths_modified` event names each changed path and how it changed.

The check works for every agent because it runs after the attempt, not inside
the agent: it cannot stop a write, only reject and undo it. On a git tree,
paths git ignores are never protected, so build output under a protected
directory does not count; what git ignored when the attempt started stays
unprotected and untouched even if the attempt edits `.gitignore`, so a local
file is never removed as "added". `.git` and `.apb` are never covered, and
neither is a path below a symlinked directory.

The restore is careful about what it writes through. A symlink the attempt
put in place of a protected directory is removed (the link, never its target)
and the directory is recreated; nothing is written or removed through it. A
file is restored by writing the copy beside it and renaming it over the
path, so a hardlink the attempt made to a file elsewhere never changes that
file, and a crash leaves either the old file or the whole copy. Every copy is
checked against the digest taken with it: a copy that changed in the
meantime (the copies live under the run directory, which the agent can
reach) is never written back, and the node fails at once with
`protected path snapshot was tampered with: <path>` instead of retrying.
A path that could not be restored is listed in the event's `restore_failed`;
the copies are then kept, and the event's `kept_copies` and the failure
message name where.

A node with `protect` never runs in a concurrent batch: the check compares
the whole tree before and after the attempt, so a sibling's legitimate write
under the node's globs would otherwise be undone and blamed on it. Its
siblings run before or after it. A glob that is
absolute, climbs out with `..`, or does not parse is validator error **V75**;
a glob that matches no file in the project is warning **V76** (from
`apb validate` and `apb doctor`), because it protects nothing.

### Completion check (decision models)

When the machine's `decisions.yaml` turns the `completion_check` use on, each
successful `agent_task` attempt is also rated by a decision model (is the
reply a finished result, or a progress note, a plan or a question back?). The
check is recorded in the run journal; in advise a flagged attempt also raises
an anomaly wake, and only the `enforce` opt-in below can change the attempt's
status. It never runs on a node whose `success_check` is a script. A node whose
output is not a report (a collector returning a list, a node printing only
data) can switch it off:

```yaml
  - { id: collect, type: agent_task, prompt: "list the open issues", profile: dev, completion_check: off }
```

`completion_check` takes `auto` (the default), `off`, or `enforce`: the
opt-in that lets a confident "not a finished result" fail the attempt and
consume a retry, where the machine enables the use in enforce and has a
stored threshold (elsewhere `enforce` behaves as `auto`). See DECISIONS.md.

### Other decision-model opt-ins

All optional and additive to schema 2; none does anything on a machine
without a `decisions.yaml` that enables the use (validator V74 notes them).

- `route: auto` on an `agent_task` (V70): pick one of the profile's `tiers`
  for the first attempt. Needs a profile with `tiers` (a run start refuses
  otherwise); never applies to a node with `continue_session`.
- `option_descriptions: { <option>: "<meaning>" }` on a `human_review` (V71):
  what each option means, shown to the decision model as the recommendation's
  criteria. Keys must be the gate's options.
- `auto_decide: { allow: [needs_changes], min_confidence: 0.9 }` on a
  `human_review` (V72, V73): let a confident recommendation of
  `needs_changes` decide the gate. Never `approve`. Refused on a playbook that
  declares `irreversible` or `secrets` effects, or when a node after the gate
  declares them itself, is granted a connector function flagged
  `irreversible: true`, or is named like a merge, push, deploy or publish
  step, unless the gate sets `auto_decide_ok: true` (see "effects").
- `defaults.retry_advice: enforce`: let retry advice skip doomed
  same-executor retries.
- `supervisor: { pre_triage: enforce }`: let wake pre-triage post a retry
  itself (at most three per run by default).

### Run provenance

Every agent_task and script process runs with three environment variables:
`APB_RUN_ID` (the run's id), `APB_RUN_DIR` (its `.apb/runs/<id>` directory)
and `APB_NODE_ID` (the node's id). A prompt can read the id as `{{run.id}}`.

On a git working tree the engine reads `HEAD` before an agent_task or script
node runs and again when it finishes. When `HEAD` moved, the node's commits
(newest first, at most 50 listed) are journaled as one `artifacts_committed`
event and shown by `run_report`, `run_status`, `apb runs <id>` and the
dashboard run page. Outside git, or on a repository without a commit, nothing
is recorded.

Only commits added on top of where the node started are recorded: when
`HEAD` ends on another branch than it started on, or moved anywhere but
forward (a checkout, a reset, a rebase), the node gets no record, since the
difference would list someone else's history. An interactive node that
commits and then asks a question keeps the `HEAD` it started from, so its
record after the answer covers both rounds. Nodes that run at the same time
in a concurrent batch share one `HEAD`, so a node gets a record only when it
ran alone; commits made in a parallel fan-out are linked to the run by the
`Apb-Run:` trailer below, not by `artifacts_committed`.

That links a run to its commits. For the other direction, from a commit to
its run, write the run id as a commit trailer:

```
Fix the date parser

Apb-Run: <run id>
```

In a prompt: "commit with the trailer `Apb-Run: {{run.id}}`". From a script:
`git commit -m "..." -m "Apb-Run: $APB_RUN_ID"`. `git log --grep "Apb-Run: <id>"`
then finds every commit of a run.

### Status file (APB_STATUS_FILE)

Each agent_task attempt is handed an `APB_STATUS_FILE` environment variable
pointing at a per-attempt JSON file in the run directory. The agent MAY write its final verdict there as
`{"status": "success"|"failure", "outputs": { ... }}`, where `outputs` is an
object of values the step should expose to later steps. The engine reads that
file first to decide the attempt's status and outputs, and falls back to the
existing marker and text parsing when the file is absent, unreadable, or
invalid. The prompt builder appends a note describing this contract when the
node has a `success_check`, sets `require_verdict` or declares `outputs.fields`
(then naming the keys to write, see "Named outputs" below); other nodes keep
the report-only contract.

When the status file supplies a non-empty `outputs` object, that object
replaces the node output before the `success_check` runs, so a `marker` check
then looks for its marker inside the `outputs` JSON rather than in the agent's
textual report. Put the completion marker in `outputs` when you write one, or
omit `outputs` and keep the marker in the reply text, so the check can still
find it.

### require_verdict (mandatory completion signal)

`success_check` gates a report the agent already chose to write.
`require_verdict: true` on an `agent_task` node goes further: writing a valid
status file becomes the only way for the node to succeed, whatever the process
exit code or the agent's final text say.

```yaml
- id: migrate
  type: agent_task
  prompt: "run the long migration; write your status file when truly done"
  profile: dev
  require_verdict: true
  max_retries: 2
```

`defaults.require_verdict: true` turns the requirement on for every `agent_task`
in the playbook. The node field and the default are combined with a logical OR,
so the setting is switch-on only: a node cannot opt out of a playbook-wide
`defaults.require_verdict: true`, and `require_verdict: false` on a node is
indistinguishable from leaving it unset. Set it per node when only some steps
need it.

With `require_verdict` in force:

- the status-file contract note is appended to the prompt unconditionally
  (normally it appears only when the node has a `success_check`), together with a
  note saying plainly that an attempt whose process ends without a valid file is
  recorded as interrupted;
- an attempt whose process exits without leaving a valid
  `{"status": "success"|"failure", ...}` file behind is recorded as
  **interrupted** rather than succeeded. The text it had produced is preserved on
  the attempt as `partial_output`, a retry is consumed, and the next attempt's
  prompt carries a note that a previous attempt ended without recording a verdict
  and to check for work already done - commits, branches, worktrees, written
  files, running background jobs - before redoing any of it. That note also
  rides the first attempt of a node re-executed later (a supervisor retry, a
  resume, a continue-from) when the node's last journaled attempt was
  interrupted, so the recovery advice does not die with the process that earned
  it;
- a status file that already holds a verdict is read even past a non-zero exit,
  a timeout, or a kill: once the agent's own completion signal exists, the exit
  code is tail noise and is journaled as such. A `{"status": "failure"}` file on
  a failed process keeps the attempt failed but exposes the agent's own `outputs`
  as the node output instead of the raw CLI error text.

`require_verdict` defaults to `false`. Turning every "exit 0, no status file"
attempt into an interruption would change the contract under every playbook that
relies on the ordinary text report. Opt in for nodes that orchestrate
long-running or background work, where "the process ended" and "the work is
done" are different questions, and see "Long-running orchestrator nodes: commit
early and often" below for the authoring discipline that makes an interruption
cheap to recover from.

### Declared deliverables (outputs.files)

A node may declare `outputs.files`, glob patterns for the files it is expected to
produce:

```yaml
- id: report
  type: agent_task
  prompt: "write the audit report to report-<date>.md"
  profile: dev
  outputs:
    files: ["report-*.md"]
```

On every successful attempt of a declaring node the engine matches those globs
against the run's working tree and records what they matched as the node's
artifacts, available to later steps and to the run report. This no longer depends
on the node having a cache configured: a declaration is checked because it was
declared.

A declaration whose globs match nothing journals a `deliverable_missing` warning
event naming the node and the declared patterns. The same event carries a
`detail` when the capture itself failed instead (an unreadable match, a path
escaping the node's scope). It never fails the node: a glob is brittle enough
that hard-failing on drift would be worse than a visible warning. Watch the run's
events (the dashboard, or `run_events`) for it after a run whose deliverables
matter, particularly on orchestrator-style nodes where the agent's own success
report is the least trustworthy signal that a file actually landed. A node served
from the cache journals neither an artifacts capture nor this warning: its
artifacts are replayed from the cache record.

### outputs.extract (output hygiene)

`outputs` also carries `extract`, a marker name, independent of `files`:

```yaml
- id: review
  type: agent_task
  prompt: "review the diff and end with <VERDICT>your verdict</VERDICT>"
  profile: dev
  outputs:
    extract: VERDICT
```

Set on an `agent_task` node, the engine takes the content of the last
`<VERDICT>...</VERDICT>` block (whatever marker name is given) the agent emitted
anywhere in its turn as the node's output, instead of its last assistant message.
Unset, the node keeps the default: the last assistant message with any report
block stripped. This keeps the recorded output intact when a host `Stop` hook or
a guardrail appends a turn after the agent's real work finished, which would
otherwise become the node's output. The other half of that hygiene lives on the
profile: see PROFILES.md's "Agent environment" (the default `minimal` one
suppresses the appended turn at the source instead of filtering around it).

### Named outputs (outputs.fields)

A node can declare the named values it publishes:

```yaml
- id: assess
  type: agent_task
  prompt: "find the worktree of the PR branch and whether review comments are open"
  profile: dev
  outputs:
    fields: [working_tree, open_comments]
```

On an `agent_task`, the declaration puts the status-file contract into the
prompt (even without a `success_check`) and names the exact keys to write under
`outputs`: `{"status": "success", "outputs": {"working_tree": "...",
"open_comments": "none"}}`. That object becomes the node output as JSON, as for
any status file, so later nodes read one value by name:
`{{nodes.assess.output.working_tree}}` in a template, or an `output_field` edge
condition on `open_comments`. A `script` node publishes named outputs by
printing a JSON object on stdout, which is already its output.

After a successful execution, a declared field missing from the output (the
output is not a JSON object, or it has no such key) journals an
`output_fields_missing` warning event naming the missing fields. It never
fails the node, the same policy as `deliverable_missing`. Validation warns
(**V46**) when a template or an `output_field` condition reads a field that the
source node's `outputs.fields` does not declare. A node that declares nothing is
not checked.

### Node working directory (workdir)

An `agent_task` or a `script` node can run somewhere other than the execution
root:

```yaml
- id: gate
  type: script
  script: scripts/gate.sh
  runner: sh
  workdir: "{{nodes.assess.output.working_tree}}"
```

`workdir` is a template rendered like a prompt, but it may read only what can
name a path: `params.*`, `run.instruction`, `nodes.<id>.output` (or one field of
it) and `nodes.<id>.review_decision`. Anything else is a **V47** error, and so is
`workdir` together with `isolation` (an isolated node runs in its own
directory). The rendered path is resolved against the run's working tree (the
execution root when the run has none, see [Run working tree](#run-working-tree-worktree);
an absolute path is kept) and must be an existing directory when the node starts.
An empty render (the value it reads was never published) or a missing directory
fails the node, without spawning anything, with a message naming the rendered
value. It never falls back to the execution root: running a gate in the wrong
tree and reporting it green is the failure this field exists to prevent. A read
the graph does not order before the node is a **V38** warning, as for prompts.

Each attempt's directory is recorded on its `attempt_started` event
(`workdir`). A node with a `workdir` is not cache-eligible (the cache
fingerprints the run's tree), and its `outputs.files` globs still match against
the run's tree.

When every node of a run should work in one tree, declare the run working tree
instead of repeating `workdir` on each node: it also scopes the run's busy lock.

### Run working tree (worktree)

A run can have a working tree of its own, typically a git worktree of the
project checked out on the branch the run is about:

```yaml
worktree: "{{nodes.assess.output.working_tree}}"   # or "{{params.tree}}"
nodes:
  - id: assess
    type: agent_task
    prompt: "Create or find the worktree for the PR and publish it."
    outputs: { fields: [working_tree] }
```

Every `agent_task` and `script` without its own `workdir` runs in that tree, and
a node's own relative `workdir` resolves against it. The tree is resolved once
per run and journaled as `worktree_resolved` (`path`, `source`, `node`):

- **at start**, when the caller passes one (`apb run --worktree <dir>`, MCP
  `playbook_run` `worktree`, the dashboard API's `worktree` field), which wins
  over the playbook, or when the playbook's `worktree` reads only `params.*`;
- **when a node succeeds**, when `worktree` reads that node's output
  (`nodes.<id>.output` or one field of it). The nodes before it run in the
  execution root; every node after it runs in the tree. Sub-playbook runs work
  in their parent's tree.

The path is resolved against the execution root (an absolute path is kept) and
must be an existing directory that belongs to the project: a directory inside
it, or a git worktree of its repository. A tree passed at start that does not
qualify refuses the start; one published by a node fails the run before the
next node, never falls back to the execution root. `run_status` (MCP), the
dashboard's run page and `GET /api/runs/<id>` show the tree (`worktree`).

The template may read only `params.*` and the output of **one** `agent_task` or
`script` node; anything else is a **V48** error, and a field the source node
does not declare in `outputs.fields` is a **V46** warning.

**Busy lock.** A write-run holds a busy lock for the checkout it works in, so
two runs over the same tree still take turns (or queue, from the dashboard),
while runs over different git worktrees run side by side. A run in the
execution root, or in a plain directory of the root's own checkout (whose files
are the root's files), uses the project lock `.apb/workdir.lock` as before; a
run in a separate git worktree uses `.apb/locks/tree-<digest>.lock` in the
execution root, keyed by that worktree's top level (a worktree never grows an
`.apb` of its own). A tree resolved by a node moves the lock: the tree's lock is
taken first, then the project lock is released. `apb doctor --run <id>` reports
the lock of the run's tree.

### Declared cache key (cache.key)

`cache: auto` keys a node's result on its definition, rendered prompt, profile
bundle, executor, connectors and a fingerprint of the workspace (the whole git
work tree, or the `inputs.files` globs). A node whose result depends on
something else, such as a PR dossier that depends on the PR's head commit,
misses on every unrelated edit to the tree. Declare what it depends on instead:

```yaml
- id: collect
  type: agent_task
  prompt: "Collect the discussion of PR {{params.pr}} into the dossier."
  cache: { mode: auto, key: "{{nodes.probe.output.head_sha}}", ttl: 7d }
```

With a `key`, the rendered key takes the place of the workspace fingerprint in
the cache key; everything else stays in it. Admission is unchanged: the node
must leave the workspace as it found it (declared `outputs.files` aside) and
make only read-only connector calls, so a cached result can never hide a side
effect. A key that renders empty (the value it reads was never published) skips
the cache for that execution. The key may read what a `workdir` may read
(**V49** error otherwise); `key` without `mode: auto`, on a node with its own
`workdir`, or a key that reads nothing and has no `ttl` is a **V49** warning.

### Warning: premature success in long-running orchestrator nodes

A single-process agent node that spawns background workers and is expected to
wait for them is not reliable, no matter how firmly the prompt forbids ending
the reply. Observed repeatedly in real runs: a coordinator agent backgrounds
its workers, then exits minutes later at its first wait phase with interim
text plus a success report, the engine accepts the report at face value, the
run advances, and cleanup nodes destroy the still-running workers' state.
Prompt discipline alone does not hold.

Author around it, in order of strength:

- Give such a node a marker `success_check`. The coordinator is told to emit
  the marker only at true completion; an early exit with interim text is then
  rejected and flows into the normal retry and failure-edge machinery instead
  of advancing the run.
- Pair strict verification with empowered repair. Add a review or qa node
  that treats every named deliverable as mandatory and fails when one is
  absent, regardless of which subtask was supposed to produce it, and route
  its failure into a fix node whose prompt explicitly allows implementing the
  missing deliverable in full. This combination makes the graph self-healing
  when a coordinator dies early anyway.
- Prefer graph-level orchestration over prompt-level orchestration. When work
  splits into parallel pieces, model them as parallel branches with a join,
  or as sub-playbooks, rather than asking one agent node to babysit external
  processes for the whole duration.

### Long-running orchestrator nodes: commit early and often

An orchestrator or otherwise long-running `agent_task` node can be cut off
mid-work by anything from a host process restart to a supervisor interrupt aimed
at it. When its work happens inside a git worktree (an `isolation: full` or
`best_effort` node, or an agent managing its own worktree), the recovery cost of
that interruption is entirely a function of how much uncommitted work existed
when it happened.

Author the node's prompt to commit its own progress at natural checkpoints, after
each subtask, each file, each passing test, rather than saving one large commit
for the end. Paired with `require_verdict` and the interruption note a retried
attempt receives, that turns "the run was interrupted" into "the next attempt
resumes from the last commit and loses minutes" instead of losing the whole
phase. This is authoring discipline, not an engine mechanism: nothing forces an
agent to commit often, but the prompt can ask for it, and a node that checkpoints
little and often degrades gracefully under exactly the failure modes the
engine's resilience features exist to handle.

## Node types

`start`, `agent_task`, `script`, `prompt`, `condition`, `human_review`,
`wait`, `finish`, `playbook` (runs another playbook as a sub-run, see
"Sub-playbooks" below), `judge` (asks a decision model typed questions, see
"Judge nodes and judge edges" below). A playbook needs exactly one `start` and
at least one `finish`. Edges connect node ids; conditional edges gate on node
status, review status, an output substring match, one structured field of a
node's output, or a decision model's yes/no answer about the source node's
output (`judge`).

## Template variables

A node prompt (`agent_task`, `prompt`), a `playbook` node's `instruction`, and
a finish node's `prompt` are rendered as templates before use. This is the
exact accepted set; any other `{{...}}` reference is rejected at save time as
a V13 validation error:

- `params.*` - a declared playbook param's value, by name (`params.<name>`).
- `nodes.<id>.output` - the node's output text.
- `nodes.<id>.report` - the same value as `.output` (an alias; both names
  resolve identically).
- `nodes.<id>.output.<field>` (and `nodes.<id>.report.<field>`) - ONE top-level
  field of that output when it parses as a JSON object, the template twin of the
  `output_field` edge condition below and with exactly its semantics. Strings
  render verbatim; booleans and numbers render as their JSON text (`true`, `3`).
  Everything it cannot read renders as the empty string and never fails the node:
  output that is not JSON or not a JSON object, an absent field, and a value that
  is null, an array or an object. One field only, never a path, and never empty:
  `.output.a.b` and a trailing `.output.` are not valid references and fail
  validation like any other unknown namespace.
- `nodes.<id>.review_note` - the reviewer's note from a `human_review` node's
  decision.
- `nodes.<id>.review_decision` - the option a `human_review` node was decided
  with (for example `approve`), so a node after a multi-option gate can act on
  it without reading the whole `run.context`. Empty until the gate is decided.
- `nodes.<id>.rejected_output` - the agent report text a `success_check`
  discarded on the node's last rejected attempt (see Success checks). Empty when
  the node was never rejected; a later rejection overwrites an earlier one.
- `run.instruction` - the run's input prompt (see below).
- `run.id` - the run's id, the name of its `.apb/runs/<id>` directory. Every
  agent_task and script process also gets it as `APB_RUN_ID`, next to
  `APB_RUN_DIR` and `APB_NODE_ID` (see "Run provenance" below). A prompt that
  reads it renders differently on every run, so such a node never hits the
  node cache.
- `run.context` - the accumulated run context (params, instruction, node
  outputs, reviews, hooks), the same text a finish-with-prompt agent sees.
  Bounded by the node's context budget (see "Context budget" below).
- `run.hooks.*` - the relative signal URL for a `wait` node's hook key
  (`run.hooks.<key>` renders `/api/hooks/<run-id>/<secret>`). Posting to that
  URL only unblocks the wait; the request body is discarded, not stored or
  rendered anywhere. This is a different "hooks" from a connector's inbound
  event inbox, whose delivery bodies are kept and read with `inbox_read` (see
  CONNECTORS.md, "Receiving events (webhooks and the inbox)").

An unresolvable reference (an unknown param, a node id that is not in the
playbook, a namespace outside this list) fails validation before the
playbook can be saved or run, rather than silently rendering empty at run
time.

Whether a reference resolves and whether it has a value yet are separate
questions. A template that reads `nodes.<id>.output` or `nodes.<id>.report` (with
or without a field selector) where nothing in the graph orders `<id>` before the
reading node is validator warning **V38**: across un-joined parallel branches
that value may render empty. The remedy is to route the read behind `<id>`
itself, or behind a node that already joins both branches (see "Joining parallel
branches"). Adding `join: all` to the reader does nothing when the reader has a
single incoming edge, which is the common shape this warning catches. A
loop-carried read, where both nodes sit in one cycle, is not flagged: there the
previous pass supplies the value.

At run time the same hole is observable rather than silent. When a node executes
and one of its `nodes.<id>.output|report` references has no source text to fill
it, the run journals a missing-input anomaly naming every such reference and why
it is empty (`never ran`, the source's own status, or `<status> with empty
output`), and in a supervised run that anomaly wakes the supervisor. The
criterion is the source's recorded output, never its status: a reference is
reported only when the source has no recorded output at all or its output is the
empty string. So an `on_failure` handler reading the failure it handles stays
silent, because a failed node's own text is recorded and does render, while a
source that succeeded with nothing to say is caught. A field-selector read is
reported on that same source-side criterion; what it does NOT report is a
selector that could not project a field out of output the source really did
produce (not JSON, not an object, a missing field, a null/array/object value),
because there the source did give its text and the mismatch is in the agreed
shape of it, not in the graph. One anomaly per node execution lists all of that
node's holes. A finish node composing an answer is checked the same way; a node
served from the cache is not, because neither its execution nor its capture runs.

### Context budget (how much recorded output a prompt gets)

Recorded output reaches a prompt through `run.context` and `nodes.<id>.output`
(or `.report`, or a field of either), and both are bounded so a verbose node or
a long loop does not grow every later prompt. The budget is deterministic (no
model call) and loses nothing: the full text stays on disk, and every cut names
the file that holds it, so the agent can read more when it needs to.

- `run.context` keeps only the latest run of each node. An earlier run of the
  same node (a loop pass, a re-run) shrinks to its heading and a pointer to the
  run's `context.md`, which holds every run.
- Each remaining node section is clipped to `section_max_bytes`, with a note
  naming `<run dir>/node-outputs/<node>.md` (the node's latest full output).
- When the whole context still exceeds `max_bytes`, the oldest node outputs are
  replaced by a pointer to their file until it fits. The newest output is always
  kept, and supervisor notes are never cut.
- A `nodes.<id>.output` reference is clipped to `output_max_bytes` the same way.

The engine defaults are `max_bytes: 65536`, `section_max_bytes: 8192` and
`output_max_bytes: 32768` (about 16k, 2k and 8k tokens). Set them playbook-wide
under `defaults.context` or per node under the node's own `context`; each field
falls back on its own (node, then defaults, then the engine default), and `0`
lifts that limit:

```yaml
defaults:
  profile: developer
  context: { max_bytes: 32768 }
nodes:
  - id: review
    type: agent_task
    profile: reviewer
    prompt: "Review this diff:\n\n{{nodes.implement.output}}"
    context: { output_max_bytes: 0 }   # this node needs the whole diff
```

A finish-with-prompt composer is bounded the same way. The budget changes the
rendered prompt, so it also moves the node's cache key.

## Judge nodes and judge edges

A `judge` node asks a decision model (see DECISIONS.md) typed questions over a
small named state and publishes the answers as a compact JSON object, so the
usual `output_field` edges and `{{nodes.<id>.output.<field>}}` templates route
on them. It replaces a verdict-only `agent_task` or a brittle `output_match`
gate, at a fraction of a second and of a cent per decision. The playbook
names no provider, URL, model or key: the machine's `decisions.yaml` decides
whether and where the questions are asked.

```yaml
- id: triage
  type: judge
  title: Classify the review result
  state:                         # named fields; each value is a template
    request: "{{run.instruction}}"
    review_output: "{{nodes.review.output}}"
  questions:                     # a map, never a list
    verdict:
      type: choice
      instructions: "Which outcome does `review_output` report for the change described in `request`?"
      criteria:
        clean: "No blocking findings; only style notes or none."
        needs_fix: "At least one concrete defect that must be fixed before merge."
        unclear: "The review is cut off, empty, or does not address the change."
    risky: { type: noul, instructions: "Does `review_output` mention deleted tests, committed secrets or schema migrations?" }
    effort:
      type: score
      instructions: "How much work do the findings in `review_output` imply?"
      levels: ["nothing to do", "a one-line fix", "a local change in one file", "changes across several files", "a redesign"]
  thresholds:
    verdict: { min_confidence: 0.6, below: unclear }
    risky:   { yes_at: 0.7 }
    effort:  { bands: { small: [0, 1.5], medium: [1.5, 2.5], large: [2.5, 4] } }
  on_unavailable: { route: human_gate }  # or { default: { verdict: unclear, risky: true } } | fail | emulate
```

Question types: `choice` (2 to 255 named options, each described by its
criterion), `score` (2 to 10 levels, lowest first, each described in words)
and `noul` (the probability of yes; optional `criteria` under the keys `true`
and `false`). At most 32 questions per node.

The output, for the node above:
`{"verdict":"needs_fix","verdict_p":0.83,"verdict_confidence":0.71,"risky":false,"risky_p":0.12,"effort":"medium","effort_score":1.9,"decided_by":"typesafe/jev-1.13.0"}`.
A `choice` gives `<id>`, `<id>_p` and `<id>_confidence`; a `noul` gives
`<id>` (true or false) and `<id>_p`; a `score` gives `<id>_score` (the
expected level index, 0 = lowest) and, with bands, `<id>`. Thresholds are
applied in code, never by the model: a `choice` below `min_confidence` becomes
the declared `below` option; a `noul` is true at `p >= yes_at` (0.5 without a
threshold); a score falls into the band `[lo, hi)` (the last band closed).
`decided_by` is `<provider>/<model>`, `emulated:<provider>`, `default` or
`unavailable`.

Route with `output_field` edges: `{ type: output_field, node: triage, field: verdict, equals: needs_fix }`.

**When the answer is used.** Only when the machine's `judge_node` use is at
`enforce` and every answer is valid. Below enforce (shadow, advise), without a
`decisions.yaml`, with `APB_DECISIONS=off`, or when the provider is down,
times out, is out of budget or returns an invalid item, the node applies
`on_unavailable`, so a judge playbook runs on any machine:

- `{ route: <node> }` succeeds with `{"decided_by":"unavailable","reason":"..."}`.
  The route is an explicit edge, required by the validator:
  `{ from: triage, to: <node>, condition: { type: output_field, node: triage, field: decided_by, equals: unavailable } }`.
- `{ default: { <question>: <value> } }` succeeds with those values,
  `decided_by: default` and the reason. Each value lands in the field an
  answer writes: `<id>`, except for a `score` without bands, whose number
  lands in `<id>_score`.
- `fail` fails the node like any node (failure edges, `defaults.on_failure`,
  a supervisor park). Absent `on_unavailable` means `fail`, with a V55 warning.
- `emulate` asks an emulation backend: the `llm_emulation` providers of
  `decisions.yaml`, then the node's `profile` (falls back to
  `defaults.profile`) as one agent attempt that answers in JSON. Emulated
  answers are uncalibrated (DECISIONS.md) and cost an agent turn; the node
  fails when the emulation gives no answer either.

The `reason` names why: `not_configured`, `off`, `mode`, `budget`,
`unavailable`, `timeout`, `rate_limited`, `auth`, `invalid`.

Every answer is journaled as a `decision_made` before the output is written;
a resumed or re-run judge with the same state replays it without a request.
With `cache: auto` the answer is reused across runs, keyed by the node
definition (questions and thresholds), the rendered state and the pinned
provider model; only answers at enforce are stored, never a fallback.

**Writing good questions.**

- One judgment per question. Two things to decide are two questions.
- Describe situations, not degrees: "At least one concrete defect that must be
  fixed before merge", not "very bad". Score levels are words ("a one-line
  fix"), never numbers: the model never sees them (V57 warns on digits).
- Always give a `choice` an `unclear` (or `other`, `none`) option, and use it
  as `below` (V56 warns without one). The options come from you, the author.
- Facts stay in code. Whether a file exists, a test passed, a count or a
  date is a `script` node or a `success_check`, never a question.
- Keep the state small and named, and cite its fields in backticks in the
  instructions (`review_output`). Read one field of a node output where the
  source declares `outputs.fields`; a whole transcript is clipped (V58).
- Thresholds belong to one provider and model version. Pin versioned model
  ids in `decisions.yaml` (`jev-1.13.0`, not an alias): a threshold tuned on
  one version does not transfer to the next.
- Collect 20-30 labelled examples of the decision before trusting a
  threshold, and run the use in shadow first; switch `judge_node` to enforce
  only once the journaled answers agree with what you would have decided.
- References: https://docs.typesafe.ai/patterns and
  https://docs.typesafe.ai/model-jaggedness/jev-1.13.

**Judge edge.** A binary branch without an extra node:

```yaml
- from: review
  to: fix
  condition: { type: judge, question: "Does the review in `output` report at least one defect that must be fixed before merge?", min_p: 0.7, on_unavailable: false }
```

Always a yes/no question over the source node's output (`output`) and title
(`step`), nothing else. All judge edges of a node go in one request when the
node succeeds, journaled before routing; each loop execution is asked anew.
The edge matches at `p >= min_p` when the `judge_edge` use is at enforce, and
otherwise exactly when `on_unavailable` is true, which is mandatory (V59).
A failed source asks nothing. Prefer a judge node when you need more than a
yes/no, more than a few edges (V60 warns above eight), or the answer in a
template.

Validator codes: V50 questions shape (including two questions writing one output field), V51 question content, V52 thresholds,
V53 `on_unavailable`, V54 state, V59 judge edges (errors); V55, V56, V57, V58,
V60, V61 (warnings).

## Human review and conditional edges

A `human_review` node pauses the run for a human decision:

```yaml
- { id: review, type: human_review, options: [approve, reject] }
```

`options` is a list of strings: the choices a reviewer can pick. It is optional:
an empty or absent list offers the defaults `approve` and `reject`.
`review_decide` records one of them as the node's decision, plus a free-form
note (available downstream as `{{nodes.review.review_decision}}` and
`{{nodes.review.review_note}}`).

An optional `prompt` gives the reviewer guidance, shown above the options in
the owner-facing instruction and in the web review panel:

```yaml
- { id: review, type: human_review, options: [approve, reject], prompt: "Check the changelog before deciding." }
```

`prompt` is plain text: template placeholders inside it (`{{...}}`) are not
rendered and are surfaced literally, same as the option strings.

An edge's `condition` gates traversal on one of four types:

- `node_status { node, equals: success|failure }` - matches when the named
  node's status is `success` or `failure` (which also covers a timeout).
- `review_status { equals: <option string> }` - matches when the
  `human_review` node this edge starts from was decided with exactly that
  option string.
- `output_match { node, pattern }` - matches when the named node's output
  contains `pattern` as a substring (not a regex).
- `output_field { node, field, equals }` - matches when the named node's
  output parses as a JSON object whose top-level `field` equals `equals` as a
  string. This is the way to route on a verdict the agent wrote deliberately:
  an `agent_task` writes `{"status":"success","outputs":{"verdict":"failed"}}`
  to `$APB_STATUS_FILE`, the `outputs` object becomes the node output as
  compact JSON, and the edge reads one field of it. The comparison is exact
  (no substring, no case folding). Anything unreadable is simply a non-match:
  output that is not a JSON object, a missing field, or a value that is null,
  an array or an object. Booleans and numbers compare by their JSON text
  (`true`, `3`). The same projection is available inside a prompt as
  `{{nodes.<id>.output.<field>}}` (see "Template variables"), so a downstream
  node can quote one field of a verdict instead of the whole JSON blob.

```yaml
edges:
  - { from: verify, to: fix,  condition: { type: output_field, node: verify, field: verdict, equals: failed } }
  - { from: verify, to: done, condition: { type: output_field, node: verify, field: verdict, equals: ok } }
```

Two rules guard conditional edges. On a `condition` node they are hard errors:
**V09** if `node_status` branches cover only one of success and failure with no
`fallback` edge, and **V10** if a condition references a node that cannot
execute before the owner (an unknown node, or one that only runs after it). The
same two mistakes are possible on conditional edges hung off any other node
kind, for example an `output_field` route off an `agent_task`, and there they
are reported as warnings **V39** and **V40** with the same meaning. Warnings do
not block a save or a run, so an existing playbook keeps working, but they are
pointing at a route that can never be taken.

An edge with no `condition` always matches. Two edges from the same node with
structurally identical conditions (or two fallbacks) and different targets are
a V34 validation error: first-match routing would only ever take one of them,
so the other target is dead or contradictory. Several unconditional edges from
one node are parallel fan-out and are fine; an unconditional edge combined with
a conditional one from the same node is also V34, because the unconditional
edge makes the conditional unreachable. A worked example wiring a review
gate:

```yaml
nodes:
  - { id: draft,   type: agent_task, prompt: "draft the release notes", profile: writer }
  - { id: review,  type: human_review, options: [approve, reject] }
  - { id: publish, type: agent_task, prompt: "publish {{nodes.draft.output}}", profile: writer }
  - { id: notify,  type: agent_task, prompt: "tell the author: {{nodes.review.review_note}}", profile: writer }
edges:
  - { from: draft,   to: review }
  - { from: review,  to: publish, condition: { type: review_status, equals: approve } }
  - { from: review,  to: notify,  condition: { type: review_status, equals: reject } }
```

## Joining parallel branches

Several unconditional outgoing edges from one node are a fork: every target starts
as soon as the source finishes. A node with more than one INCOMING edge is where
those branches come back together, and how it waits depends on the edges into it:

- An incoming edge may carry `join: all` or `join: any`. `all` makes the node wait
  for every incoming branch to reach a terminal status before it runs; `any` lets
  the first arrival trigger it. This is the explicit form and behaves as it always
  has.
- A node with two or more incoming edges and NO `join:` on any of them is not
  first-arrival by default. When every incoming source lies outside the node's own
  cycle (an acyclic fan-in, the ordinary diamond of fork, two branches, merge) the
  node is an implicit `all` join: it waits for every branch, exactly as if
  `join: all` had been written, without anyone writing it.
- A node with no `join:` whose fan-in IS part of a cycle (`check -> tick ->
  check`, where `tick` has two inputs and one of them is the loop's own back edge)
  keeps first-arrival semantics. A wait-for-all barrier there would deadlock,
  because the back-edge source has not run yet in this pass. Loop bodies rely on
  that, and it is unchanged.

An implicit join only synchronizes: it waits, then runs. It never fails the node
because an incoming branch failed, the way an explicit `join` does. An
unconditional fan-in fed by a failure edge is very often meant as a shared error
sink, and the implicit form is deliberately permissive about that. Write an
explicit `join: all` (or `join: any`) when the node itself should fail on a failed
input.

A join, implicit or explicit `all`, does not deadlock on a branch that will never
run - a conditional fork where only one of two branches was selected, say. A
source no longer reachable from anything still active in the run counts as
satisfied instead of leaving the join waiting forever, and the run journals a
`join_input_dead` event naming the join and the sources written off, so the
decision is auditable afterwards. That is routine graph bookkeeping rather than an
anomaly - an either-or merge has one by construction - so no supervisor is woken
for it.

```yaml
nodes:
  - { id: start,   type: start }
  - { id: fetch_a, type: agent_task, prompt: "fetch A", profile: dev }
  - { id: fetch_b, type: agent_task, prompt: "fetch B", profile: dev }
  - { id: merge,   type: agent_task, prompt: "combine {{nodes.fetch_a.output}} and {{nodes.fetch_b.output}}", profile: dev }
  - { id: done,    type: finish, outcome: success }
edges:
  - { from: start,   to: fetch_a }
  - { from: start,   to: fetch_b }
  - { from: fetch_a, to: merge }     # no join: - implicit all-join, acyclic fan-in
  - { from: fetch_b, to: merge }
  - { from: merge,   to: done }
```

`merge` above waits for both `fetch_a` and `fetch_b` with no `join:` written
anywhere.

### Validating a join

`join` values are validated, not silently coerced. A value other than `all` or
`any` on an edge is validator error **V36**. Mixing `all` and `any` across the
incoming edges of one node is validator warning **V37**: the engine takes the
first `join` in file order and ignores the rest, which is easy to do by accident
when edges are edited independently. A template that reads across un-joined
branches is validator warning **V38** (see "Template variables").

### Concurrency limit (max_parallel)

A fork's ready branches run concurrently in every run mode, supervised as well as
autonomous, bounded by `max_parallel`: at most that many branch nodes run at once,
and the rest are admitted as slots free up.

```yaml
defaults:
  profile: dev
  max_parallel: 2   # at most two branch nodes run at the same time
```

`defaults.max_parallel` wins. Failing that, the value persisted on the run's own
config applies, so a detached run resumed later keeps the cap it started with.
Failing that, the engine default of 4. A declared `0` reads as `1` rather than
admitting nothing, and the cap is re-resolved on every scheduling pass, so a
supervisor patch that changes it takes effect from the next batch. There is no CLI
flag and no MCP argument for the cap today: `defaults.max_parallel` in the
playbook is the knob an author has.

`max_parallel: 1` does not form one-member batches; it takes the sequential path
outright, the same path a single ready node has always taken. Lower the cap for
playbooks whose branches are resource-heavy (large builds, rate-limited external
calls) or where many branches at once would just be noise to review; leave it
alone for cheap, independent branches.

One shape never joins a batch at all, whatever the cap says: a node with an
explicit `join:` edge. Only an explicit barrier can be recorded failed with the
barrier's own reason when one of its inputs failed, and raise a wake for a
supervisor, and that verdict belongs to the sequential path. An implicit fan-in
(two or more incoming edges and no `join:` field, outside any cycle) only
synchronizes, so it batches like anything else. Two `agent_task` nodes that each
read the same pair of producers therefore run alongside each other, in one
scheduling pass, when slots are free.

In a supervised run the execution is concurrent but the supervision stays serial:
the whole batch runs, then failures are presented one at a time, in batch order,
at the batch tail. A `join: any` satisfied by an earlier group cancels the
branches still waiting for a slot, and those are journaled cancelled like any
other cancelled branch.

One consequence for `cache: auto` nodes: a node executed as a batch member
usually fails cache admission, because batch siblings share one workspace and
any sibling write changes the post-execution fingerprint. Caching pays off on the
sequential path and on re-runs, not within a wave. The rejection is journaled
with its own reason, `workspace shared with concurrent batch siblings`, so it can
be told apart from a node that really did dirty the tree. Every member's cache
key is taken against the tree as it stood before the wave started, so the key
does not change when `max_parallel` does.

## Unhandled failures (defaults.on_failure)

A playbook that draws a `node_status: failure` edge from every node into one
negative finish node buries its own structure: those edges are most of the
graph and none of them says anything except "this went wrong".
`defaults.on_failure` declares once what an unhandled failure does, so they can
go:

```yaml
defaults:
  on_failure: aborted
```

The value is one of three things:

- `route` (the default, and what every playbook written before this did): a
  node that ends `failed` or `timed_out` with no edge to take that failure is
  an engine error. The run ends failed, and the reason says an edge is missing.
- `stop`: the same situation ends the run as failed on purpose, and the reason
  is the failing node's own output.
- a node id: the failure goes to that node, exactly as an edge into it would
  have. This is what keeps a negative `finish` node that composes a written
  failure answer working while every edge into it is deleted.

Anything that is not `route` or `stop` is read as a node id, so a misspelled
reserved word surfaces as validator V35 (`on_failure` names an unknown node)
instead of being silently ignored. The policy never applies to the target
itself: a failure of the handler has nowhere further to go and stays an engine
error rather than routing in a circle.

An explicit edge always wins over the policy, so the branches that actually
handle something (a review that routes into a fix, a check that routes into a
retry) stay exactly as they are. Only the edges that led nowhere but the end of
the run disappear.

An unconditional edge (no `condition`, not a `fallback`) is taken whatever the
node's status, so it moves the run past a failure without handling it. A run
that reaches a `finish outcome: success` node carrying such a failure ends
**failed**, with a run error naming the node: a delegated `type: playbook`
child that failed, followed by one arrow onward, must not read as success. A
failure counts as handled when, after it, the run takes a conditional or
`fallback` edge out of the failed node, the `on_failure` route, an edge into
an explicit `join` (which weighs the delivered failure), or any conditional
edge that reads the failed node (for example `build -> check`, then `check ->
fix` on `node_status: build equals failure`). Only the node's latest result
counts, so a retry or a loop that later succeeds leaves nothing behind.

The web canvas marks a node whose failure the policy handles with `stop on
failure` or `on failure: <node>`, so the branch that is no longer drawn is
still visible.

The policy governs AUTONOMOUS runs. A supervised run (`apb run --supervise`, or
`playbook_run` with a supervisor) never reaches it: a failed node raises a wake
and waits for the supervisor to decide (retry, continue from another node, patch
or abort), which is why deleting the failure edges does not change a supervised
run either way.

One thing to know before choosing `stop`: a `finish` node with a `prompt` is
what composes a written closing answer for a failed run. Where that answer
matters, point the policy at that node rather than stopping.

## Attempt failures: kinds, retries and interruptions

A failed attempt is classified before the engine decides what to do with it, and
the label is journaled on the attempt as `failure_kind`:

- `budget` - a money or quota problem (a spend limit, an exhausted plan). This is
  a property of the account, so neither a retry nor a different model on the same
  agent can fix it.
- `auth` - a credential problem. The same executor will fail identically until a
  human re-authenticates.
- `transient` - infrastructure noise: a dropped connection, a 5xx, a rate limit.
  The same executor, run again, has a real chance of succeeding.
- `agent` - everything else, the agent's own mistakes included. This is the
  historical behavior: consume a node retry, then walk the fallback chain.

The classifier is a curated table over the failure text, checked in that order,
because a spend limit and an expired token are both routinely delivered as a 429.
A plain "agent timed out" is deliberately NOT transient: that wording is the
engine's own deadline kill, and reading it as infrastructure would hand every
timed-out node extra same-executor attempts it never had. That holds for a
`require_verdict` node too, where a dropped transport does count as transient
(unless the text says something more specific) but a deadline kill does not:
re-running a whole job from scratch because it ran out of time spends the full
cost again on the same outcome. Instead, when the killed attempt's session can be
resumed (see "Retries continue the session" below), a `require_verdict` node gets
exactly ONE continuation of that session per chain step, with a short "continue
where you stopped" prompt, journaled as a `supervisor_action` with action
`timeout_continuation`; it spends neither the node's retries nor the
infrastructure budget. Without a resumable session the step is abandoned like
any other timeout (next fallback step, or the node ends `timed_out`). A node
without `require_verdict` keeps the plain rule: a deadline kill moves on to the
fallback chain.

A `transient` failure is retried on the SAME executor out of a separate
infrastructure budget that never touches `max_retries`: the node's own retry count
does not move and no retry is journaled against it, while each infrastructure
attempt is journaled in its own right and announced by a `supervisor_action` event
with action `infra_retry`. The budget IS the backoff schedule, by default two
waits of 5s and 30s, and it applies per fallback-chain step: every step of a
node's chain gets its own fresh allowance, so a node with three chain steps can
spend up to six infrastructure attempts before its own retries begin. Set
`APB_INFRA_BACKOFF_MS` to a comma-separated list of milliseconds to change the
waits and the budget together (`APB_INFRA_BACKOFF_MS=20,20` in tests); a
malformed or empty value falls back to the default rather than disabling
infrastructure retries.

An `auth` or `budget` failure fails the attempt at once and additionally
suppresses every remaining fallback step bound to the SAME agent: another agent
may still succeed where an exhausted quota cannot, but the same account certainly
will not. That suppression lives for one node execution and is not persisted. A
resume, a supervisor retry, or the next node walks the chain from the top again
and hits the same expired credential unless a human fixed it in between, which is
the point: between two drives, someone may have.

### Retries continue the session

A retry does not start over. When an attempt fails and the next attempt runs on
the same agent AND model (a node retry, an infrastructure retry, a deadline
continuation, or a fallback chain that comes back to that binding), it resumes
the failed attempt's own agent session and sends only what happened: the
failure text (clipped to 2 KiB), the interruption note for a verdict-less exit,
or the deadline note, plus any supervisor notes and, when the node has a status
file contract, a one-line reminder to write the verdict. The node prompt, the
SOUL and the skills are not re-sent: the session holds them. A fallback to a
different agent or model always starts fresh with the full prompt.

How the session is found, per agent:

- `claude`: apb assigns the id at launch (`--session-id`), so even an attempt
  killed at its deadline can be resumed (`--resume <id>`).
- `opencode`: apb titles the session at launch (`--title`) and finds it by that
  title with `opencode session list --format json`. An attempt killed before its
  first reply persisted no session; then there is nothing to resume.
- `codex`: the `session id:` line of its stderr header, `zcode`: the `sessionId`
  of its `--json` reply, and any agent that prints an id in its output: only an
  attempt that exited on its own can be resumed.
- Every other agent, and an agent without a resume form, starts fresh.

A continued session keeps the working directory it started in (agents key their
sessions by it), so an isolated node's continuation runs in the directory of the
attempt it continues instead of a fresh one. If the agent answers that the
session does not exist, the engine drops it and starts fresh once, without
spending a retry.

### Continuing another node's session (continue_session)

Consecutive nodes on the same profile often need the same understanding of the
repository: an `assess` node reads the code, an `implement` node acts on it. A
fresh agent for the second node rebuilds all of that. `continue_session` lets
it continue the first node's agent session instead:

```yaml
- { id: assess, type: agent_task, profile: dev, prompt: "assess the change" }
- { id: implement, type: agent_task, profile: dev, prompt: "implement it", continue_session: assess }
```

The node's first attempt resumes the session in which the named node's latest
successful attempt finished, and sends this node's whole prompt (task, skills
line, connector block, contracts) as the next message; the SOUL is not sent
again, the session carries it. The handoff is warm only when all of these hold:

- the source attempt recorded a session id (see "Retries continue the session"
  for how each agent's id is found);
- the source ran on the same agent and model as this node's primary executor;
- the agent can resume a session: `claude`, `codex`, `opencode` and `zcode`;
- both nodes run in the same directory: neither is isolated and their
  `workdir`s resolve to the same place (agents key sessions by directory).

Otherwise the node starts a fresh agent, exactly as without the field. Either
way the run journals `session_handoff` with `warm: true|false` and, when cold,
the reason. A handed-off session the agent no longer has is dropped and the node
starts fresh once, without spending a retry. Retries after a warm start continue
the session as any retry does, and a fallback to another executor starts fresh.

Validation: **V44** (error) when `continue_session` names an unknown node, the
node itself, or a node that is not an `agent_task`. **V45** (warning) when the
handoff can already be seen to start cold: nothing orders the source before the
node, the two bind different profiles, either is isolated, their `workdir`s
differ, or another node continues the same session and may run at the same
time. `apb validate` and `apb doctor` also warn `session_handoff_cold` when the
bound agent cannot resume a session at all.

The continuing node's prompt should say what to do next, not repeat the
context: the session already holds the source's work. Reading
`{{nodes.<source>.output}}` still works, but sends that output a second time.

### Attempt transcripts

Every agent attempt keeps its raw output in
`<run dir>/attempts/<node>-<attempt>/`, named on the attempt's
`attempt_started` event (`transcript`): `stdout.log` and `stderr.log` hold what
the agent printed, written as it arrives, so an attempt killed at its deadline
still leaves what it said and ran. For `claude`, whose text output does not show
its tool calls, the CLI's own session transcript (every tool call and result) is
copied there as `session.jsonl`. Other agents keep their sessions in their own
stores. `agent-stream/` still holds the streamed events of the `acp` transport.

A transcript holds whatever the agent printed or read, secrets included, so it
must never reach git. APB keeps `<project>/.apb/.gitignore` listing its
machine-local paths (`workspace.local`, `runs/`, `cache/`, `trash/`,
`backup-*/`, `workdir.lock`, `locks/`): `apb init` writes it and every run makes sure of
it before its first node starts, adding only the lines that are missing and
leaving your own lines as they are. Do not remove `runs/` from it.

### Interrupted attempts and reaping

An attempt recorded `interrupted` ended without a verdict rather than with one: it
is neither a success nor a decided failure, and the node is re-executed. Three
things produce it. One is a `require_verdict` node whose process ended without a
valid status file (see "require_verdict" above). A supervisor interrupt of such a
node is the same shape and gets the same label when no status file was written
(see "Supervisor interrupts" below). The third is reaping: when a run
is driven again after its previous driver died mid-attempt, drive entry closes out
any attempt the journal still shows open whose recorded process id is provably
gone, journaling it `interrupted` so the fact lives in the log rather than only in
a status reader's head. The node then re-enters scheduling the way an interrupted
node always has.

Reaping is deliberately narrow. An attempt with no recorded process id is never
reaped: unknown is not dead. A live process id is never reaped either, and neither
is anything the liveness probe could not answer, because a live id may mean
another driver still owns the run and ownership is settled by the working-directory
lock rather than by a guess. Reaping happens only at drive entry: nothing is reaped
while a run sits abandoned, which is why `apb doctor --run` and `run_status` keep
reporting such a run's attempt as lost until someone drives it again. In the
journal a reaped node can legitimately show two attempts numbered 1, the first
`interrupted`: the attempt counter is per execution, and the reap makes the stale
attempt explicit instead of letting the fresh one quietly overwrite it.

An attempt cut off with work in flight keeps whatever it had said so far in
`attempt_finished.partial_output`, so the recovery attempt and a human reading the
run both see how far it got.

### Supervisor interrupts

A supervising agent can interrupt an attempt with `supervisor_interrupt_attempt`
(see `docs/MCP.md`). Passing `node` interrupts only that node's running attempt,
which is what a wedged branch of a concurrent fan-out needs: its healthy siblings
keep running, and they neither acknowledge nor consume an interrupt addressed to
another node. With `node` omitted the interrupt is a broadcast and terminates every
attempt currently running in the run. Either way the interrupted branches recover
through their ordinary retry and fallback paths; unlike `supervisor_run_abort`, an
interrupt does not stop the run.

An interrupt only reaches an attempt that was already running when it was posted.
It is not queued for a later attempt of the same node, and a node sitting between
attempts in an infrastructure backoff does not observe one. Interrupt a running
attempt; when the run itself should stop, use `supervisor_run_abort`.

Where that abort is observed depends on how the node is running. A node on the
sequential path observes it within a poll tick even in the middle of a backoff, so
it does not have to wait the backoff out. A member of a concurrent batch observes
it at the batch's next admission boundary: the groups still queued behind the
running one are never started and are journaled cancelled, and the run ends
aborted at the boundary after the batch. A member already in flight, including one
waiting out its own backoff, runs to its own end first.

The label an interrupted attempt carries is decided by the node, not by the
interrupt. On an ordinary node the attempt is journaled `failed`, or `timed_out`
when its own deadline had already expired, and that holds whether or not the agent
had written a status file: a verdict does not survive the interrupt, it rides an
anomaly wake instead, so a supervisor can see the work existed and accept it
explicitly. On a `require_verdict` node the attempt is journaled `failed` or
`timed_out` when there was a status file to overrule, and `interrupted` when none
was written, because that node's contract is a recorded verdict and none was
recorded. Either label consumes the same retry, and neither carries a
`failure_kind`: an interrupt is a control decision, not an infrastructure failure.

## Interactive nodes

An `agent_task` node may be marked `interactive: true`, letting the agent ask
the user a question mid-attempt instead of only reporting a finished result.
Four fields carry this:

- `interactive` (bool, default false): only meaningful on `agent_task`.
- `answer_by` (`human` | `supervisor`, default `human`): who may answer.
  `human` requires a supervising agent to relay the question to the user
  verbatim and relay the answer back verbatim; a supervisor cannot answer
  such a node on its own judgment (see `docs/MCP.md`'s supervisor relay
  contract for the exact refusal and wording). `supervisor` lets the
  supervisor answer directly from its own judgment.
- `question_timeout_seconds` (optional): how long the node waits for an
  answer before falling back to `default_answer`. Omitted, the node waits
  forever, like `human_review`.
- `default_answer` (optional): the answer used when the timeout elapses
  (`answered_by: "timeout"`). Requires `question_timeout_seconds` (validator
  V32); the reverse - `interactive` companion fields set without
  `interactive: true` - is validator V31.

```yaml
schema: 2
id: deploy-with-confirmation
name: Deploy with Confirmation
version: 1.0.0

defaults:
  profile: architect

nodes:
  - { id: start, type: start }
  - id: confirm
    type: agent_task
    title: Confirm before deploy
    prompt: |
      Check the target environment, then ask the user to confirm before
      deploying.
    interactive: true
    answer_by: supervisor
    question_timeout_seconds: 900
    default_answer: "abort"
    expected_duration: 5m
  - id: deploy
    type: agent_task
    title: Deploy
    prompt: "Deploy using the confirmed target: {{nodes.confirm.output}}"
    expected_duration: 10m
  - { id: done, type: finish, outcome: success }

edges:
  - { from: start, to: confirm }
  - { from: confirm, to: deploy }
  - { from: deploy, to: done }
```

How the answer reaches the node depends on the transport the invocation
resolves to, best available first: **live** (today: claude only) injects a
one-tool MCP sidecar (`ask_user`) into the agent, so the tool call itself
blocks until an answer arrives; **resume** re-invokes the agent with the
answer once a session id is available; **reprompt** - the floor every agent
falls back to - re-invokes the agent from scratch carrying the full Q&A
transcript in the prompt. Whichever transport is live, a running agent can
also just print the marker `<<<apb:question>>>` followed by a line of JSON
(`{"question": "...", "options": [...]}`); this is how resume and reprompt
recognize a question, and it also works as a manual fallback for a live agent
that prints it instead of calling the tool. Answers land through
`run_answer` (MCP), `apb answer <run> [--node <id>] <text>` (CLI), or the web
UI's question panel; a pending question shows up in `apb runs`, `apb doctor
--run`, and `run_status.pending_question`.

## Bounded loops

A cycle in the graph is legal only when it carries one of two guards
(validator V11); a cycle with neither is refused:

- `max_loops` on a `condition` node caps how many times control passes
  through that node in one run, regardless of how many edges make up the
  loop. Once the cap is exceeded, the run takes that node's `fallback: true`
  edge if one is wired, or fails outright if none is. Use this when one
  `condition` node is naturally the loop's checkpoint.
- `max_traversals` on an edge (an integer >= 1; `max_traversals: 0` is
  refused separately, validator V30) caps that one specific edge. Once its
  count is reached, edge selection treats it as non-matching, so the run
  takes whatever alternative edge is wired instead (or hits the ordinary
  no-matching-edge behavior if none is). Use this when the loop has no
  `condition` node, or when only one edge in the cycle - not the whole loop -
  needs the cap.

A `condition`-node loop:

```yaml
nodes:
  - { id: lint,  type: script, script: "scripts/lint.sh", runner: sh }
  - { id: check, type: condition, max_loops: 3 }
  - { id: fix,   type: agent_task, prompt: "fix: {{nodes.lint.output}}", profile: architect }
  - { id: done,  type: finish, outcome: success }
edges:
  - { from: lint,  to: check }
  - { from: check, to: done, condition: { type: node_status, node: lint, equals: success } }
  - { from: check, to: fix,  condition: { type: node_status, node: lint, equals: failure } }
  - { from: fix,   to: lint }
```

The canonical `max_traversals` fix-loop (no `condition` node in the cycle):

```yaml
edges:
  - { from: review, to: fix,    condition: { type: node_status, node: review, equals: failure }, max_traversals: 3 }
  - { from: fix,    to: review }
  - { from: review, to: qa,     condition: { type: node_status, node: review, equals: success } }
```

After three review failures the bounded `review -> fix` edge stops matching
and the run takes whatever else is wired from `review` (here, `review -> qa`
if `review` last succeeded). If nothing matches at all, the run fails with an
explicit "node has no outgoing edge and is not finish" error rather than
looping forever - wire an edge for the fully-exhausted case (an escalation to
`human_review`, or a plain failure edge) if that outcome must be handled
gracefully.

A loop edge may also point straight back at its own source, for a node that
repeats itself without a separate fix step. It needs `max_traversals` like any
other loop edge, and a `fallback` edge for the way out:

```yaml
edges:
  - { from: start,  to: review }
  - { from: review, to: review, condition: { type: node_status, node: review, equals: success }, max_traversals: 2 }
  - { from: review, to: done,   fallback: true }
```

`review` runs three times: the first pass plus two traversals of the
self-edge, each journaled as a counted `edge_traversed review -> review`.
After that the capped self-edge stops matching and the run takes the
`fallback` edge to `done`. A self-edge whose condition stops matching earlier
ends the loop there. This behaves the same as routing the loop through a
`condition` node (`review -> again -> review` with `max_loops: 2` on `again`),
including on resume and inside parallel branches.

## expected_duration (progress estimates)

Every node may carry an optional `expected_duration`: the estimated wall time
of ONE execution. Give it as integer seconds (`90`), a single unit suffix
(`30s`, `5m`, `2h`), or a compound with units in descending order (`1h30m`,
`2h15m30s`). For a node inside a loop this is the per-iteration time. Use whole
numbers of the units above: an invalid value such as a bare decimal (`1.5`), a
negative number, a boolean, or a compound whose units are out of order or
repeated (`30m1h`, `1h1h`) still lets the playbook load but the validator flags
it as a V20 error.

When creating or editing a playbook, estimate `expected_duration` for every
`agent_task` and `script` node. A rough guess is fine; the trial and run
reports show expected vs measured durations, and you refine the numbers with
`playbook_update`. Nodes without it fall back to a 120s default, and the
validator emits a V19 warning. Waiting nodes (`human_review`, `wait`) count as
zero work, so leave their estimate at the default.

## Run input prompt (Start node)

Every run can carry a free-form "input prompt": the text available to node
prompts as `{{run.instruction}}`. Edit it on the Start node in the web editor.
Typing autosaves a draft that is NOT part of the playbook definition: it does
not create a version and does not change trust, and a frozen playbook still
accepts draft edits. At run start the value is resolved once: an explicitly
passed instruction wins, otherwise the current draft, otherwise none. The chosen
value is snapshotted immutably into the run.

`playbook_trial` accepts the same `instruction` argument as `playbook_run`, so
an instruction-driven draft can be trialed with a real instruction before it
is ever approved.

## Finish answer

A finish node may carry a `prompt` and an optional `profile`. With a prompt, an
agent composes the run's final answer from the accumulated run context (params,
instruction, node outputs, reviews, hooks, compacted context) and that text
becomes the run answer, shown on the dashboard and returned by run_status and
run_report. A finish without a prompt stays instant and free with no answer.
Do not set a profile without a prompt (validator V21). Estimate
expected_duration on a finish-with-prompt like any agent step.

## Host execution mode (running without agent CLIs)

A playbook needs nothing special to run in host execution mode (informally
"mono-agent" mode): it is a per-run choice, not a playbook or profile field.
Runs are `cli` by default: apb spawns the agent CLI each node's profile names.
Pass `execution: "host"` on `playbook_run` (CLI: `apb run --execution host`)
only when the person explicitly asks for it: mono, host or single-agent mode,
or for the run to use your own subagents. Do not choose it yourself. The only
other way a step runs as a host task is the automatic fallback below, when no
CLI of the step can start. Nothing detects a host by name, there is no
machine switch, and a project `.apb/config.yaml` can only turn the fallback
off.

In host mode apb spawns no agent CLI. Every agent step (and a finish answer)
becomes a host task that `run_wait` returns in `pending_tasks`, each with the
full `prompt` (the rendered node prompt, report contract included), the
profile's `role_prompt`, the `skills` paths, the `workdir`, the `env` to set
(`APB_RUN_DIR`, `APB_RUN_ID`, `APB_NODE_ID`, `APB_STATUS_FILE`), the `outputs` contract, a
`deadline` from the node's `timeout_seconds`, and a `model_hint` for fallback
entries and routed tiers. Run each with a subagent (independent tasks may run
concurrently) and submit its final reply verbatim with `run_task_submit`
(`succeeded`, `failed`, or `blocked` with the question for the user). The
engine treats the reply like a finished CLI attempt: report block, status
file, `success_check`, `require_verdict` (the submission counts as the verdict
unless the subagent wrote the status file), the completion check, retries and
fallbacks (each a new task), loops, gates and resume all work unchanged.
Sub-playbooks inherit the mode; their tasks show up on the parent run.

What host mode ignores in a profile: the executor's `agent`, `model` and the
`agent` of each fallback (a fallback's `model` becomes the hint), the
invocation `command`, `environment: minimal` (a claude-only mechanism) and
`continue_session` (a host subagent has no session apb can continue; the node
starts cold with `session_handoff.reason: host_mode`). The role prompt,
skills, `timeout_seconds`, `expected_duration`, `outputs`, `success_check`,
`require_verdict`, `completion_check`, `protect` and the goal criteria apply as
always. Context compaction
is skipped (it would spawn a CLI).

Host fallback: a `cli` run you start in the background or under `supervise:
self` can also hand you a single step as a host task when none of its CLIs can
start at all (the binary is missing, or it is not logged in); the journal
records `execution_fallback`. Ordinary agent failures never fall back. Turn it
off with `execution: { fallback_to_host: false }` in the global config or the
project `.apb/config.yaml`; `APB_EXECUTION=cli` turns off host mode and the
fallback for a process.

## Sub-playbooks (the playbook node)

A `playbook` node runs another playbook as a full child run:

    - id: translate_book
      type: playbook
      playbook: book-translation      # or { id: book-translation, scope: global }
      instruction: "Translate the plan from {{nodes.plan.output}} chapter by chapter."
      expected_duration: 2h

The node's rendered instruction becomes the child's run input; the child's
finish answer becomes the node's output. The child is an ordinary playbook (any
playbook can be a child). The parent's policy gate walks the whole reference
tree once and pins each child, so you consent to the whole tree at parent start;
an untrusted child blocks the parent, and a reference cycle is refused. Nesting
is limited to 5 levels. Set expected_duration explicitly on a playbook node
(validator V19 nudges you): the parent cannot sum the child's own estimates.

## trigger (matching contract)

`trigger` is the only thing used for matching. Keep fields machine-oriented and
in English so the FTS escalation stays language-agnostic:

- `when`: canonical phrasings of when to apply (max 5 items, each <= 120 chars)
- `avoid_when`: when not to apply
- `examples`: example user requests

The free-text `description` and display `name` never enter matching.

## requires (applicability)

`requires` declares what a project must have for the playbook to apply. The
server runs a preflight before a run and reports anything missing:

- `files`: paths that must exist
- `commands`: commands that must be on PATH

Scope (project vs global) is only about where the definition is stored, not
about applicability. A global playbook still declares `requires` to stay honest
about where it can run.

## effects

`effects` declares the playbook's side effects. Declarations can only widen what
the server infers from node types, never narrow it. Values: `fs_read`,
`fs_write`, `network`, `external`, `secrets`, `irreversible`. Declare
`irreversible` for anything that cannot be rolled back (a push, a merged or
opened pull request, deploys, publishes, external notifications): a run of a
playbook whose tree declares it starts only with an explicit consent (MCP
`confirm_irreversible` with the refusal's `consent_nonce` after asking the
person, a `y` to the question `apb run` asks at a terminal or
`--confirm-irreversible=<consent_nonce>`, the dashboard's Run dialog), which
the run manifest records as `consent: { irreversible: true, by: ... }`. A
connector function flagged `irreversible: true` that a node is granted counts
the same way. A node that looks like a merge, push, deploy or publish step by
its name but declares nothing is warning V90: declare it. A
trigger or a headless start without that consent is refused before the run is
created. A sub-playbook inherits its parent's consent, so declaring
`irreversible` on a child makes every parent that runs it ask once, at start.

A node can declare its own `effects` too, for example on the one step that
merges or deploys:

```yaml
- id: release
  type: agent_task
  prompt: "Publish the release"
  effects: [irreversible]
```

A node's declaration widens the playbook's effective effects the same way
(so it needs the same consent at start), and it is what V73 reads before the
name of the node: an automatic review
decision (`auto_decide`) is refused on a gate followed by a node that
declares `irreversible` or `secrets`, whatever the node is called.

## goal (target and criteria)

Optional. The goal this playbook exists to reach, in the owner's words, plus
verifiable criteria. When present, the validator (V41) requires a non-empty
statement, at least one criterion, and a description on every criterion.

Two more codes cover connectors that receive inbound events. A node that
grants `inbox` functions of a connector whose manifest carries no `webhook`
block is validator error **V42**: nothing can ever be delivered to that
inbox, so the node would poll an empty store forever. A node that grants
`inbox` functions on an account that does not define the account fields the
connector's webhook block references is validator error **V43**: a delivery
to that account could not be verified and would be rejected at the door.
Both checks are skipped when the tool running them cannot see the installed
connectors, so a machine that has not installed the connector yet still
validates its playbooks.

- `statement` (string): the goal in plain words, e.g. "the invoice is
  recorded in the tracking sheet and sent for approval".
- `criteria` (list): each `{ description, check? }`.
  - `check: { type: manual }` (default when omitted): a person confirms the
    criterion. The engine never checks it; every run surface lists it as an
    item to confirm.
  - `check: { type: marker, marker: <string> }`: the literal string must
    appear in the finish answer or in the latest output of any node.
  - `check: { type: script, path: scripts/<file> }`: a script under the
    version's `scripts/` (covered by the trust digest like every script) runs
    with `sh` in the run's working tree, with `APB_RUN_ID` and `APB_RUN_DIR`
    set; exit 0 passes. It may run for up to 10 minutes. It runs from the
    run directory's copy of `scripts/`, and only while that copy still
    matches the digest the run pinned at start: when an earlier node changed
    it, every `script` criterion is `error` (so `enforce` fails the run)
    instead of running an edited check.
- `enforce` (bool, default `false`): a failed `script` or `marker` criterion
  fails a run that would otherwise succeed, with a `run_error` naming the
  criterion. Without it the results are only reported. V41 warns when
  `enforce` is set and every criterion is manual, because nothing could fail.

When the checks run: when the run reaches a finish node, after every node
before it has run and after the finish answer is composed, just before the
finish node's `node_finished`. Every criterion journals one `goal_checked`
event (`index`, `description`, `check`, `status`: `passed`, `failed`,
`manual` or `error` when the check could not run, and a `detail`). A run that
ends before a finish node (a failure no route handles, a stop) checks
nothing. Because the checks run after cleanup-style nodes (a node that
deletes a scratch worktree or resets a branch), write criteria against
persistent outcomes: a pushed branch, a file in the repository, a passing
test suite, a published release, not scratch state a later node removes.

`run_status`, `run_report`, `apb runs <id>` and the dashboard run page show
the goal with each criterion's result (`pending` until the run is checked).

The goal is the contract of the run: agents and supervisors may adapt the
process, but must never weaken or rewrite the criteria; only a person may
change them.

<!-- 0.24.0 eval suites -->
## Eval cases (evals/)

A playbook can carry an eval suite under `.apb/playbooks/<id>/evals/`: one
YAML case per file, with a fixture (a directory under `evals/fixtures/` or a
ref of the repository), an instruction, and checks over the finished run
(outcome, the goal criteria above, the visited route, node outputs, files in
the tree, journal events, and case scripts under `evals/scripts/`).
`apb eval <id>` runs each case as a run in a disposable repository, repeats
it on request, stores the pass counts per configuration (playbook digest,
profile bundles, resolved models) and compares them with the previous
result. Write goal criteria against persistent outcomes and the suite gets
them for free: `checks.goal: required` is the default.

Evals never run a playbook with `irreversible` effects, a shipping step, a
connector or a sub-playbook; such a suite is V81 and `apb eval` refuses it.
Keep the cases small (the fixture of the repository's own
`branch-quality-review` suite is a twenty-line crate), tag a quick subset,
and set `limits` and `budget` in `suite.yaml`: every repetition is a paid
agent run. The format, the checks and the storage are in `docs/EVALS.md`.
<!-- end 0.24.0 eval suites -->

## Secrets

Never put secret values in a playbook or in a capture synopsis. Reference them
by env or config key name, or a placeholder param. Concrete secret-looking
values are rejected at capture and should never be committed to a definition.

## Recurring review findings go into project memory

A review node (an agent review, a judge, a `human_review` gate) that keeps
flagging the same kind of problem is telling you the implementing agent lacks
a rule. After a finding shows up for the second time across runs, write it
down where every later agent reads it: the project's memory file
(`CLAUDE.md`, `AGENTS.md`) or a skill the implementing profile loads. Keep
the rule short and concrete (what to do, where, why), not the whole review.

Make it a step of the playbook rather than a chore someone remembers: a
`docs` agent_task after the review that reads `{{nodes.review.output}}`,
compares the findings with the memory file and adds a rule only for a
finding it has seen before (for example because the same rule was already
proposed in an earlier run's review, or the file carries a "seen once" note
the node maintains). Put the memory file under `protect` on the
implementing node if that node must not edit its own rules, and gate the
docs node with `human_review` when rules need an owner's approval. A rule
in memory advises; if it must always hold, add a deterministic check behind
it (a `success_check` script, a `goal` criterion, `protect`; see
GUARDRAILS.md).

## Linking runs, commits and tracker records

When a playbook works on a tracked item (an issue, a ticket, a task), link
both directions so either end leads to the other:

- **The record id goes into the artifact.** Pass the tracker id as a param
  (`{{params.issue}}`) and have the nodes put it where the work lands: the
  branch name, the commit message, the pull request title or body, the
  report file.
- **The run id and the commits go into the record.** Every agent and script
  node gets `APB_RUN_ID` in its environment (next to `APB_RUN_DIR` and
  `APB_NODE_ID`), and a prompt can place the id itself with `{{run.id}}`.
  End every commit a run makes with the trailer

  ```text
  Apb-Run: <run id>
  ```

  so `git log --grep 'Apb-Run: <run id>'` finds the run's commits and a
  commit leads back to `.apb/runs/<run id>/`. On a git working tree apb
  also records the commits each node made (HEAD before and after the node)
  as an `artifacts_committed` event, and the run report lists them. A final
  node that comments on the tracker record with the run id and those
  commits closes the loop from the record side. "Run provenance" above
  has the details.

Keep the tracker write in one node near the end (after the gates), so a
record is not updated for work that a later gate rejects.

## Language

Machine fields (`id`, canonical `trigger.when` / `avoid_when`) are English.
Display `name`, human `description`, and node prompts may be in any language.
Anything you say to the user about a playbook should be in the language of
their recent chat.
