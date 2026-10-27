# Authoring playbooks (tier 2)

This is the on-demand detail an agent pulls via `playbook_howto` only when it is
actually creating or reworking a playbook. It is not needed for ordinary
matching or running.

## playbook.yaml structure

A playbook is a YAML document with these top-level fields:

- `schema` (int, default 2; schema 1 playbooks are converted with `apb migrate`)
- `id` (string, machine id, English, kebab or snake)
- `name` (string, display name, any language)
- `description` (string, free text, any language; not used for matching)
- `version` (string, `X.Y.Z`)
- `params` (list): each `{ name, type, label?, options?, default?, secret? }`
- `defaults` (profile, retries, timeout, on_failure)
- `trigger`, `requires`, `effects`, `budget`, `goal` (see below)
- `nodes` (list) and `edges` (list)

Every save through apb (`playbook_create` / `playbook_update`, the dashboard
editor, `apb import`) creates a new immutable version, except when the
definition equals the current version apart from its `version:` field and
formatting (for an import of a format-2 bundle, the scripts must also match,
none when the bundle lists none):
then nothing is written, the current version and its trust stay as they are,
and the answer says `unchanged: true`.

`apb export` writes one version as a single bundle file: the YAML, the editor
layout and every regular file under the version's `scripts/` (bundle format
2; a version over the scripts limits, or one holding symlinks, refuses to
export rather than lose files). `apb import` checks those files again and
writes the YAML and the scripts as one new version, which holds exactly the
bundle's scripts: none when the bundle lists none, never the scripts of the
version that was current. An imported version is never approved by the
import, with or without scripts: it stays untrusted until it is approved
(`playbook_approve`, or a run confirmation). Older bundles (format 1) predate
scripts: they still import, carry no scripts themselves, and their version
keeps the current version's scripts as an ordinary save does. Sharing a
playbook, the preview before an import and the approval of imported scripts
are described in "Sharing a playbook" below.

### One playbook with params, or several playbooks

When two uses of a process differ only in data (which project, which channel,
which threshold), write one playbook and make that data a `params` entry. When
they differ in which steps run or in what order, write separate playbooks:
a param that switches whole branches on and off hides two processes in one
graph, and each of them gets harder to read, test and trust.

- Code: "fix the failing tests in service A" and "the same in service B" are
  one playbook with a `service` param. "Fix the failing tests" and "upgrade a
  dependency, then fix what broke" are two playbooks, since the second has an
  extra step before the fix.
- Routine: "post the weekly report to #sales" and "to #support" are one
  playbook with a `channel` param. "Post the weekly report" and "chase every
  overdue invoice, then post a summary" are two playbooks, since the steps
  differ.

```yaml
params:
  - { name: channel, type: text, label: "Channel to post the report to", default: "#sales" }
```

### Secret params

A param that carries a credential (an API token, a password) is marked
`secret: true`. apb never remembers its value for the next start (see
"Remembered run input" below), the dashboard asks for it in a password field
at every start and never prefills it, and a remembered value of a param that
became secret is never used. Two validator errors keep a secret out of the
definition itself:

| Code | Severity | Meaning |
| --- | --- | --- |
| V99 | error | a `secret: true` param has a `default`: the secret would sit in the YAML |
| V100 | error | a `secret: true` param is not `type: text` |

```yaml
params:
  - { name: deploy_token, type: text, label: "Deploy token", secret: true }
```

A secret param still reaches the run like any param (the run's
configuration records the values it started with), so prefer a connector
account or an environment variable for a credential the playbook uses on
every run.

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

A reference that resolves in no scope a run would look in (project, then
global, for a project playbook) is finding **V91** in `apb validate` and MCP
`playbook_validate`. It is an error when only the project can provide the
profile: a project profile that exists but cannot be loaded (a missing
`scope: project` profile is already error V14). When the global store was the
last place looked (`scope: global`, or `auto` falling through to it), it is a
warning: the global store is machine-local, and CI or another machine may
have the profile there. `apb doctor` shows the same as a `profile <name>` line
(`[fail]` or `[warn]`). A run on a machine without the profile fails at start.
A save does not check it, so a playbook can be saved first and its profile
created afterwards; the dashboard's playbook view shows it as a warning.

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

A function that declares `idempotency` (the official ones that send: github
issues, comments and releases, discord, telegram, smtp, whatsapp) is sent at most once
per intent: a repeat of a send that went through gets the earlier result
instead of a second copy. Tell the agent to pass `--intent <stable name>` on
such a call and the same name on every attempt (the connector block of the
prompt marks those functions `send-once`). A send whose outcome nobody can
tell is refused on a repeat until a person decides (`apb intent resolve`,
which refuses inside a run; an agent step reports such a send as a finding);
`on_unknown: resend` on the binding lets it go out again instead, where a
rare duplicate is better than a missing message:

```yaml
    connectors:
      - { name: telegram, functions: [send_message], on_unknown: resend }
```

The binding is covered by the playbook digest, but the connector folder
and each account are digest-pinned separately and must be approved before a run.
Installing connectors, configuring accounts, secrets, trust, sending once and
the `apb connector` CLI are covered in CONNECTORS.md.

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
- `defaults.host_decisions: off`: refuse `apb decide` and `decision_ask`
  calls that name this playbook's runs (the default `allow` lets a host task,
  a script or an agent step ask the configured decision providers a bounded
  question; see `docs/DECISIONS.md`, "Host-task decisions").

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

The engine reads the trailer back when it records a node's commits. A commit
without it, or one naming another run, is flagged in `apb runs <id>`
(`[no Apb-Run trailer]`, `[Apb-Run names <id>]`), on the dashboard's commit
list, and in the outcome's done line (`impl: 3 commits (1 without an Apb-Run
trailer)`). apb never adds or rewrites a trailer itself, and leaves the
repository's git hooks alone; with a git too old to read trailers nothing is
flagged.

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

### Findings and warnings (reported, never acted on)

An attempt may report problems it saw and did not fix, and things a person
should check, next to its result. Both go in the same status file:

```json
{
  "status": "success",
  "findings": [
    { "kind": "security", "location": "src/auth.rs:42",
      "evidence": "the token is compared with ==, not in constant time",
      "why_not_fixed": "out of scope for this dependency bump" }
  ],
  "warnings": [
    { "subject": "deal 9", "reason": "its owner left the company; reassign by hand" }
  ]
}
```

- A finding has a `kind` (a slug of `[a-z0-9_-]`, at most 40 characters;
  the recommended ones are `bug`, `regression`, `security`, `data_quality`,
  `config`, `docs`, `test_gap`, `performance`, `dependency`, `other`), a
  `location` (a path with an optional `:line`, or a record; at most 200
  characters), the `evidence` (at most 1000) and `why_not_fixed` (at most
  500). A warning has a `subject` (at most 200) and a `reason` (at most 500).
  Every field is required.
- At most 20 of each per attempt. An entry with an empty or over-long field,
  a control character or a wrong type is dropped whole and counted; nothing
  is truncated.
- A file with only `findings` and `warnings` and no `status` keeps the
  textual report as the verdict. Notes never change a step's status, its
  outputs, a route, the run context or any completion check.
- In host execution mode the host passes them to `run_task_submit` as
  `findings` and `warnings` (`apb tasks submit --notes '{"findings": [...],
  "warnings": [...]}'`); they are merged into the attempt's status file.
- Each attempt's notes are journaled as one `attempt_notes` event right
  before its `attempt_finished`. The run outcome lists them (each finding
  once per node, with the latest attempt that reported it), a run with any
  is marked as needing a look in `apb runs`, `runs_list` and the dashboard,
  and `apb stats` groups findings by kind and location across runs
  (`docs/STATS.md`).

A fresh agent_task attempt prompt carries a short paragraph with this contract.

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

### List processing (list:)

An `agent_task` or a `script` node with a `list:` block runs its step once
per item of a list instead of once for the whole list. The engine loads the
items, admits them in batches, runs each item's step on its own, checks each
item's result, and keeps every item's state in the run journal. A retry, a
pause, a crash or a later loop visit repeats only the items that did not
pass, and every run surface counts the items instead of trusting an agent's
summary of them.

```yaml
- id: file_tasks
  type: agent_task
  prompt: >-
    The item section holds one triaged message. Create one task for it with
    the tracker connector and write the output task_gid.
  connectors:
    - { name: asana, accounts: [default], functions: [create_task], max_calls: 2 }
  list:
    items:
      from: { node: triage, field: items }    # or `inline: [ ... ]`, exactly one
      key: id                                 # the item field that names it (default id)
      max_items: 100                          # default 100, at most 500
      where: { state: passed, field: actionable, equals: "true" }   # optional filter
    batch:
      size: 10        # items admitted per batch (default 10)
      parallel: 2     # items running at once (default 1)
      retries: 1      # re-runs of an item its check rejected (default 1, at most 5)
    item_outputs: [task_gid]   # the fields each item's step writes
    check:                     # optional; exactly one of output, script, connector, judge
      connector:
        name: asana
        account: default
        function: get_task
        args: { task: "{{item_output.task_gid}}" }
        accept_when: { path: data.gid, exists: true }
    on_needs_look: continue    # or fail
    rerun: all                 # or not_passed
```

**Items.** `items.from` names an earlier node and one of its declared
`outputs.fields` holding a JSON array of objects, or a list node and its
field `items` (below). `items.inline` writes the objects in the playbook.
Every item is a JSON object; `key` names the field that identifies it, a
non-blank string or an integer of at most 128 characters. An id reaches
prompts, the environment and journal lines, so it may not hold a backtick,
a line break, a tab or any separator other than a plain space, a control
character or an invisible formatting mark (bidi embeddings, overrides and
isolates included). Duplicate ids, a
missing or malformed key, a source that is not a JSON array, or more items
than `max_items` fail the node before any item runs, with a message that
names the problem (duplicate ids are listed). `where` keeps the items whose
`field` equals `equals` (compared as an `output_field` condition compares:
a string as is, a number or a boolean in its JSON form). `equals: null`
keeps an item whose field is present and null; a missing field is not equal
to it, and no other `equals` matches a missing field. A field that holds an
object or an array never matches. The dropped items are counted as
`filtered_out`. An item larger than 8 KiB of JSON is never
handed to a step: it needs a look at once (`item_too_large`).

From a list node the items are that node's `items` entries, keyed by their
`id` (the `key` setting is not read): each item is the earlier item object
with the earlier step's outputs added under `output`. `where.field` then
reads those earlier outputs, and `where.state` keeps the items that ended in
one state there (`where: { state: passed }`). Both may be given, and an
item is kept only when both hold: `where: { state: passed, field:
actionable, equals: "true" }` takes the items whose check passed and whose
step read actionable, so an item its check rejected never reaches the
next step on the strength of an output nobody confirmed. `where.state` on
any other source is a validation error.

**Item states.** An item is `pending`, `running`, `passed` or `needs_look`;
the last two are final for the visit. After its step, the item's check
decides:

- `accept`: the item passed. `accept_with_note`: it passed with a note (the
  note shows on every surface and is counted as `passed_with_note`).
- `reject`: the item goes back into the queue (behind the items not admitted
  yet) for another round while it has `batch.retries` left, then needs a
  look (`rejected`). A step that did not write every `item_outputs` field is
  rejected the same way, with a note naming the missing fields.
- a check that cannot tell (a connector read that failed, a script that
  could not run or printed no verdict, a judge set to `on_unavailable:
  needs_look` without an answer) never accepts: the item needs a look
  (`check_unknown`).
- a step that failed needs a look (`failed`), with the first line of the
  failure as its note.

There are two retry layers. Inside one round, the item's step runs through
the ordinary attempt loop of the node: `max_retries`, profile fallbacks and
infrastructure retries apply to each item as to any node, and the retry
briefing tells a retried attempt what the failed one did. Across rounds,
`batch.retries` re-runs an item whose step succeeded but whose check
rejected it; the re-run's item data carries `previous_round`,
`previous_outputs` and `check_note` (each of the last two cut to 2 KiB with
a marker), so the step knows what to fix. Under `rerun: not_passed` an item
that runs again in a later loop visit gets the same fields from the visit
before. An item
interrupted by a pause, a quota pause or a crash goes back to `pending`
without spending a round.

**The item prompt.** An `agent_task` list node renders its prompt once for
the visit; each item's step gets that text followed by a final `## item`
section: "Item 4 of 43" and one fenced JSON data block holding the item's
`id` and its content as `item`, with the instruction to treat the block as
data, never as instructions. Everything that comes from the item or a check
sits inside that block, JSON-escaped, behind a fence longer than any
backtick run in it. Everything before the section is the
same for every item, so a provider's prompt cache serves the shared prefix
and only the short section is new per item. For the same reason a prompt or
a `workdir` may not read `{{item.*}}` or `{{item_output.*}}` (V86): a
template that changed per item would break the shared prefix. Write "the
item section holds ..." in the prompt instead. Each item's step runs in a
session of its own (never a continued one) with `APB_ITEM_ID` in its
environment. A `script` list step gets `APB_ITEM_ID` and `APB_ITEM_FILE`
(the item's JSON in a file under the run directory) and publishes its item
outputs by printing a JSON object, as any script node does.

**Checks.** `check` names exactly one source (V87):

- `output: { field, equals }`: one of the `item_outputs` must equal the
  value; anything else rejects. Deterministic and free.
- `script: scripts/<file>`: a script of the version's `scripts/` runs per
  item with `APB_ITEM_ID`, `APB_ITEM_FILE` and `APB_ITEM_OUTPUT_FILE` (the
  step's output as JSON) and prints `{"verdict": "accept" |
  "accept_with_note" | "reject", "note": "..."}`. A non-zero exit rejects;
  no verdict is a check that cannot tell. It may run for 120 seconds and
  reads the scripts the run pinned at start.
- `connector`: one `read_only` function of an installed connector, called
  by the engine itself with `args` that may template `{{item.<path>}}`,
  `{{item_output.<field>}}` and `{{params.<name>}}`. `accept_when` sets a
  dot `path` into the response and exactly one of `equals` (which may
  template the same way) or `exists: true`. The check is its own binding
  site, `<node>#check`, granted only that one function and account, and it
  goes through trust and the run gate like a node's binding. A write
  function here is a validation error.
- `judge`: a decision-model question per item, named `verdict`, over the
  rendered `state` fields (same templates as above). `criteria` keys are
  `accept`, `accept_with_note` and `reject`, and `accept` must be described
  (V89). An answer below `min_confidence` counts as no answer;
  `on_unavailable` (`accept_with_note` by default, `reject` or
  `needs_look`) decides what no answer means. See `docs/DECISIONS.md` for
  the providers.

A list without `check` accepts every item whose step succeeded and wrote
its `item_outputs`; the validator warns (V93) and the run outcome lists the
items as passed on the agent's own report. When a check is set, the item
does not also get the decision-model completion check, and tier routing is
not asked per item.

**The node's output.** When every item is final the node publishes one JSON
object, whatever its `outputs.fields` say: `total` (items after the filter),
`processed` (`passed` plus `needs_look`), `passed` (including those with a
note), `passed_with_note`, `needs_look`, `filtered_out`, and `items`, one
entry per item with `id`, `state`, `note`, `item` and `output` (the item's
declared outputs). Later nodes read the fields by name
(`{{nodes.file_tasks.output.needs_look}}`, an `output_field` condition on
`passed`) or take the node as the source of another list. With
`on_needs_look: continue` (the default) the node succeeds and reports the
items; with `on_needs_look: fail` the node fails with the same output, so a
failure route or a supervisor sees it.

**Sending once.** A list step is repeated by design, so every write
function a list node is granted must declare `idempotency` (V88): an
undeclared write could act twice for one item. A send is keyed by the node
and the item, so one item's send never answers another's. Without
`--intent` an item's send carries the fixed label `item:send`: one item
sends a function once, and a rerun that words the payload differently is
still the same send (it gets `already_sent` once the first went out). An
item that sends one function more than once names each send with its own
`--intent`. A call that names an item (`APB_ITEM_ID`) with no running
attempt of its own is refused: a made-up item id can neither send nor
reset a spent `max_calls`. When an item's step fails
and a send it made has an unknown outcome:

- a `key` or `marker` send gets one free reconcile run in the same round:
  the repeat replays the key or looks the marker up through the send
  ledger, so the service answers from what it already has.
- a `none` send settles the item as needing a look (`unknown_send`). The
  item is never sent again on its own: a supervisor retry skips it until a
  person resolves the send with `apb intent resolve` (`--sent` or
  `--not-sent`); either way the next retry runs the item again, a repeat
  of a send resolved as sent is answered `already_sent`, and the item's
  check decides. A
  binding's `on_unknown: resend` does not change this once the item has
  settled: no later round, visit or supervisor retry runs it again on its
  own while that send's outcome is unknown.

**What a list node may not be.** V85 refuses `list:` on any node type other
than `agent_task` and `script`, on an interactive node, on a node that
continues another node's session or whose session another node continues,
on a node with `cache` or `success_check` (use `list.check`), on a node
with `protect` while `batch.parallel` is above 1, and refuses a zero
`batch.size` or `batch.parallel`, `batch.retries` above 5, `max_items`
outside 1 to 500 and a `key` that is not an identifier. A run in host
execution mode with a list node is refused at start: list nodes need
`execution: cli` in this release.

**Validator codes.**

| Code | Level | What it covers |
|---|---|---|
| V85 | error | where `list:` may sit and its numbers (above) |
| V86 | error | the item source: exactly one of `from` and `inline`, a source that finishes before the node, a declared field (or `items` of a list node), `where` shapes (`field` with `equals`, `state` only from a list node, both together when both must hold), usable inline items; and `{{item.*}}` in a prompt or `workdir` |
| V87 | error | the check: one source, an `output` field among `item_outputs`, a script under `scripts/`, a read-only connector function, a valid `accept_when`, check templates limited to `item.*`, `item_output.<item_outputs>` and `params.*` |
| V88 | error | a granted write function without an `idempotency` declaration |
| V89 | error | a judge check's criteria keys, `min_confidence` in (0, 1], `on_unavailable` |
| V93 | warning | a list without a check; `rerun: not_passed` on a node in no cycle |
| V94 | warning | `batch.parallel` above the playbook's `max_parallel` |
| V95 | error | a `type: list` goal criterion that names a node without `list:` |

**Pause, resume, quota and stop.** Before an item starts, the runner reads
the run's control channel, so no item starts after a pause request returned.
A drain pause lets the running items finish and admits no new one; a
`--now` pause kills the running items, which journal `interrupted` and go
back to pending; a quota pause of one item stops admission, lets the other
running items finish and pauses the run until the quota window passes. The
visit stays open in each case: the node gets no `node_finished`, and the
resume continues the same visit with only the items that are not final
(when the source still yields the same items; a changed list starts a new
visit). A crashed item attempt is reaped under its item and runs again
alone. A stop ends the visit cancelled with its open items left open.

A failed list visit (for example under `on_needs_look: fail`) is reopened
by a supervisor retry (`supervisor_node_retry`): the items that passed are
kept, the items that need a look run again, except an item too large to
run and an item whose unknown send nobody resolved yet. Any other later
visit of the node, such as a loop back to it, is a new visit governed by
`rerun`.

**Surfaces.** `run_status` and `run_report` carry a `lists` block (counts
per list node; `run_status` names at most 50 items that need a look per
list, `run_report` every item; see `docs/MCP.md`). The run outcome counts
each list, names every item that needs a look as a gap (at most 30, then
one "and N more" line), lists a list without a check as self-reported, and
reads the run as needing a look while an item does. After every batch a
`run_progress` event carries a line such as "12 of 43 processed, 11
passed, 1 needs a look"; the dashboard run page shows a card per list
with its counts and a filterable item table, and the graph shows `12/43`
on the list node. `apb runs <id> --items [--node N] [--state
pending|running|passed|needs_look]` prints one line per item. The journal
records `list_started`, `list_batch_started` and `list_item_settled`, and
the attempt, connector call, send intent and decision events of an item
carry its id in `item`.

**Compatibility.** The `list:` block is additive to schema 2. An older apb
reads the playbook but ignores the block and would run the node once for
the whole list, so start and resume list runs with this apb or a newer
one.

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

A `script` node's `script` is a path relative to the version directory, and
it must live under `scripts/`: a run copies only the version's `scripts/`
next to its snapshot and runs the script from there. A script file that is
missing, that sits outside `scripts/`, or that is a symlink which is absolute
or leaves `scripts/` (a run copies the link, not its target), is error **V92** in `apb validate`,
MCP `playbook_validate` and `apb doctor` (the same check covers a
`success_check` script and a goal criterion's script). As with V91, a save
does not check it, so the YAML can be saved before the scripts are added.

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
- `run.retro` - the run's retrospective as of the moment the node starts:
  per node its time against `expected_duration`, executions, attempts,
  retries, fallbacks, re-entries, tokens and cost, the model each attempt
  actually ran on, the host wait and the status-file verdicts; the goal
  results; and the comparison with the medians of the last 10 finished runs
  of the same version. Compact text, at most 8 KiB (a longer report is cut
  with a note). The same report as data is MCP `run_retro_context`. See
  "Retrospective node" below. It renders differently on every run, so such a
  node never hits the node cache.
- `run.active_elapsed_seconds` - whole seconds the run has been active since
  it started, as of the moment the node starts. Time the run spent paused
  does not count, so a playbook can compute its own time budget from it. It
  renders differently on every run, so such a node never hits the node cache.
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

**A join inside a loop** weighs each round on its own. An input counts as
delivered only when its source finished after the join last started; a
source that delivered into the round before has yet to run in this one, so
the join waits for it while something still leads there, and writes it off
when nothing does (its gate kept it out this round). A node's routing is
read from its latest execution: an edge it took in an earlier round does
not keep that branch live, and a bounded edge its own traversal spent is
still the edge it took, never a fallback it did not take. So a finish node
fed both by a collector and by the fallback of a spent fix loop ends the
run when the collector routes into it.

A write-off holds only for the pass that made it. When an operator or a
supervisor asks for a node to run again (`resume --from-node`, or a
supervisor's retry, continue-from, patch, or move off a review gate), that
node and every node only downstream of it count as not yet run for routing
until each one starts again; the run journals that as a `nodes_revived` event
(`from_node`, `nodes`), and the run's history keeps their earlier results. An
automatic restart of interrupted work (a crash, a `--now` pause) revives
nothing: the nodes after it may already have run in this pass. A requested
re-run that a pause or a crash stopped before it began is still owed: the
next resume without `--from-node` re-runs it. A new explicit re-run (a
`--from-node`, or a supervisor directive) supersedes it instead: the latest
choice wins, the owed re-run is dropped, and the nodes only it revived read
their earlier results again. Siblings a fork's failure policy cancelled when the
re-run node failed are revived with it. A join fed by the re-run branch waits for that branch
again instead of starting together with the re-run node, and an old route of a
downstream node (into a finish node, say) is not taken before the join runs on
the new input. The run journals a `join_input_revived` event naming the join,
the sources it waits for again, and the re-run node (`from_node`).

If the re-run branch does not deliver (the node fails again, or routes
elsewhere), a join that never ran is weighed again and journals a fresh
`join_input_dead`, as in a first pass. A node that already ran in the earlier
pass (a join, a `join: any`, a merge inside a cycle) is not run again on its
old inputs: it runs again only when the re-run branch delivers into it.
"Only downstream" stops at the nodes that lead back into the re-run node:
inside a loop they keep their old routing, so a sibling that already
completed is not re-run before the loop comes back to it, and a first-arrival
merge inside the cycle still runs on an arrival from the earlier pass.

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

### Dead arrivals: what a join does with a failed branch

Every incoming branch of a join ends up in one of three states when the join is
weighed:

- **delivered**: its source finished and routed into the join, with the
  source's status (succeeded or failed);
- **pending**: its source can still run, so the join waits;
- **dead**: its source can never deliver into the join. Either the source is
  no longer reachable from anything still active, or it finished and routed
  somewhere else.

A dead branch is ignored, not counted as a failure. That is what keeps an
either-or merge from deadlocking, and it has a consequence that surprises
authors: a branch node that fails and takes its failure edge (`design -> rejected
on failure`) routed away from the join, so at the join it is a dead arrival.
The join runs as soon as the other branches arrive, as if the failed branch had
never existed. An explicit `join: all` fails only on a branch that DELIVERED a
failure (an unconditional edge carried the failed node into the join); a failure
that left through a failure edge is invisible to it. Two options make a failed
branch stop the fork instead: `require: all_succeeded` on the join, and
`fork.on_branch_failure` on the forking node.

### A join that requires every branch (require: all_succeeded)

`require: all_succeeded` on any incoming edge of a join makes the join refuse
to run unless every incoming branch delivered a success. A branch that delivered
a failure, or arrived dead, refuses it:

```yaml
edges:
  - { from: design,  to: assemble, condition: { type: node_status, node: design, equals: success } }
  - { from: design,  to: rejected, condition: { type: node_status, node: design, equals: failure } }
  - { from: content, to: assemble, require: all_succeeded }
  - { from: assemble, to: done,     condition: { type: node_status, node: assemble, equals: success } }
  - { from: assemble, to: fix_up,   condition: { type: node_status, node: assemble, equals: failure } }
```

A refused join does not execute. The run journals `join_refused` with the
inputs it refused on and records the join `failed`. The join then takes only a
failure route: a conditional edge that matches its failure, a `fallback` edge,
or a `defaults.on_failure` handler. It never takes an unconditional edge, so a
refusal cannot slip past to the next step. Without a failure route the run ends
failed with the refusal as its reason. The option works on an implicit join and
on an explicit `join: all`; a join with `require` never runs in a concurrent
batch, because only the sequential path records a refusal.

`require` refuses only once nothing can still arrive, so it does not cut a
running sibling short: in the example `content` still runs to the end before
`assemble` refuses. For that, use a fork policy.

### Failing a fork early (fork.on_branch_failure)

A forking node (two or more unconditional outgoing edges) may declare what a
failure inside one of its branches does to the others:

```yaml
nodes:
  - id: split
    type: agent_task
    prompt: "Plan the site"
    fork: { on_branch_failure: fail_fast, on_failure: rejected }
```

- `wait` (the default, and the behavior before this option): the other branches
  keep running and the failed node follows its own routing.
- `fail_fast`: the first branch node that fails (after its retries and
  fallbacks) interrupts every other running node of the fork's branches the way
  `supervisor_interrupt_attempt` does, cancels the branch nodes that have not
  started, and sends the run to `on_failure`. The failed node's own edges are
  not taken, and the fork's join never runs. `on_failure` runs exactly once,
  even when two branches fail at the same moment.
- `cancel_siblings`: the same cancellation, but the failed node follows its own
  routing (its failure edge, or an unconditional edge that moves past the
  failure). A failure sink that both branches feed (`design -> rejected` and
  `content -> rejected` on failure, which makes `rejected` an implicit join) no
  longer waits for the cancelled sibling: it runs as soon as the failed node
  routes into it.

A node belongs to the branch of a head when every path from the fork to it
passes through that head (the head dominates it); edges back into the fork are
ignored. So a rework loop from below the merge (`assemble -> review -> design`
on a rejection) keeps `design` in its own branch and `review` outside both,
and a node only one branch reaches, such as `content -> notify -> done` or a
failure sink only that branch feeds, is part of that branch and is cancelled
with it. The fork's joins are the nodes outside the branches that a branch
node reaches along its normal path. A shared failure sink that only failure
edges (`node_status: failure` on the source) and `fallback` edges lead into is
not a join: it is the natural `on_failure` target.

When forks nest, the innermost fork whose branches contain the failed node and
whose policy is not `wait` decides first. If the routing that leaves the
failure with (its `on_failure`, or the failed node's own targets under
`cancel_siblings`) has no target inside an enclosing fork's branches, the
failure was not handled inside that fork either and escalates: the enclosing
fork's branches are cancelled too, and an enclosing `fail_fast` routes to its
own `on_failure`. A failure handled inside (a failure edge into a fix-up node
of the same branch) leaves the enclosing fork's other branches running.

Every cancelled node is journaled `cancelled` with a `branch_cancelled` event
naming the fork and the node whose failure triggered it, and the decision itself
is a `branch_failed` event. A cancelled node routes nowhere, so a resumed run
does not run what lies behind it. A running host task of a cancelled branch is
closed as cancelled with a note naming the fork and the failed node. A resume
also finishes a policy a previous drive left half done: a cancellation that a
pause or a crash cut short is completed, and a branch failure whose driver died
before journaling `branch_failed` gets its policy applied, as long as nothing
ran after it. Resume such a run with the same apb or a newer one: an older apb
does not know `fork`, `require` or these events and would route the run as if
they were not there. The policies apply to autonomous runs; a supervised run
parks a failed branch node for its supervisor like any other failure, and the
supervisor decides what happens to the other branches.

Because branch nodes are scheduled continuously (see "Concurrency limit"
below), a branch's next step starts as soon as its previous step succeeds, even
while a long node in a sibling branch is still running, so a failure in that
later step interrupts the long node within seconds.

### Validating a join

`join` values are validated, not silently coerced. A value other than `all` or
`any` on an edge is validator error **V36**. Mixing `all` and `any` across the
incoming edges of one node is validator warning **V37**: the engine takes the
first `join` in file order and ignores the rest, which is easy to do by accident
when edges are edited independently. A template that reads across un-joined
branches is validator warning **V38** (see "Template variables").

`fork` on a node that does not fork is error **V77**. `fork.on_failure` is
error **V78** when it is missing under `fail_fast`, present under `wait` or
`cancel_siblings`, names an unknown node, the fork itself or the start node,
names a node inside the fork's own branches or one of its joins, and when a
`fail_fast` fork has a `join: any` join (the first success and the first
failure would both claim the fork; use `cancel_siblings` or `wait` there). A
node reached only through `on_failure` is not unreachable. `require` on an edge into a node that is not a join, or together
with `join: any`, is error **V79**; a `require` value other than `all_succeeded`
does not parse.

### Concurrency limit (max_parallel)

A fork's ready branches run concurrently in every run mode, supervised as well as
autonomous, bounded by `max_parallel`: at most that many branch nodes run at once,
and the rest are admitted as slots free up.

Scheduling is continuous: the moment a branch node succeeds, the engine records it,
routes it, and starts the successors that became ready (up to `max_parallel`)
while other branch nodes are still running. A fast branch therefore moves on while
a slow one is still working, and a join still waits for every branch it
synchronizes. A successor starts beside the running nodes only while at least one
other branch node is still at work; the node that finishes last hands its
successors to the ordinary one-at-a-time path, so the chain after a join runs
node by node with the control scan, the supervisor watch, context compaction and
commit attribution between nodes, as before. These successors also wait until the
running branch nodes have ended:

- nodes that cannot run alongside others: a `human_review`, `wait`,
  `condition`, `prompt` or `finish` node, a sub-playbook node, an explicit
  `join:` barrier, a join with `require`, an interactive node, a node with
  `protect`;
- a successor that leads back to a branch node still running (a loop through
  the fork, `fork -> {a, b} -> c -> fork`), so a new pass of a loop never starts
  beside the old one, and a fan-in inside its own cycle still runs once per pass;
- everything after the node that publishes the run's working tree (a
  `worktree` template over a node's output), so the next node starts in that
  tree.

Once a branch node ends failed, timed out, unknown or interrupted, no new node is
started alongside the running ones: the nodes already queued still run, and the
failure is handled when they end (a supervised run parks there, and the failure
policies apply there), except that a fork's `on_branch_failure` policy reacts at
once. A branch node gets its commits recorded only when it ran alone.

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

A list node (`list:`) never joins a batch: it runs alone on the sequential
path, and its items run on the list's own worker pool, at most
`min(batch.parallel, max_parallel)` at a time. `batch.parallel` above the
playbook's `max_parallel` (or the default 4 when the playbook sets none) is
a validator warning (V94): the cap still wins. As with a branch node, the
commits an item's step makes are recorded only when the pool runs one item
at a time.

One shape never joins a batch at all, whatever the cap says: a node with an
explicit `join:` edge (and a join with `require: all_succeeded`). Only an explicit barrier can be recorded failed with the
barrier's own reason when one of its inputs failed, and raise a wake for a
supervisor, and that verdict belongs to the sequential path. An implicit fan-in
(two or more incoming edges and no `join:` field, outside any cycle) only
synchronizes, so it batches like anything else. Two `agent_task` nodes that each
read the same pair of producers therefore run alongside each other, in one
scheduling pass, when slots are free.

In a supervised run the execution is concurrent but the supervision stays serial:
the running branch nodes end, then failures are presented one at a time, in batch
order. A `join: any` satisfied by an earlier group cancels the
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
- `quota` - a provider quota or rate limit: HTTP 429, `too many requests`,
  `rate limit`, a free tier's `FreeUsageLimitError`, a `credit usage limit`.
  The executor works again once the provider's window passes, which no short
  backoff waits out (see "Quota pause" below).
- `transient` - infrastructure noise: a dropped connection, a 5xx, an
  overloaded server. The same executor, run again, has a real chance of
  succeeding.
- `agent` - everything else, the agent's own mistakes included. This is the
  historical behavior: consume a node retry, then walk the fallback chain.

The classifier is a curated table over the failure text, checked budget, auth,
quota, transient, because a spend limit and an expired token are both routinely
delivered as a 429. Only `FreeUsageLimitError` and `credit usage limit` are
checked first, ahead of the budget words they resemble.
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

A deadline kill is graceful: the agent's process group gets SIGTERM, then up
to `APB_TIMEOUT_GRACE_MS` milliseconds (default 5000) to exit, then SIGKILL,
the same stop path a `--now` pause uses. What the agent printed on its way
out is drained within the usual drain budget, and when it holds a complete
final record (claude prints one, with its usage, when it is stopped) the
attempt keeps that token usage and cost. The attempt stays `timed_out`
whatever the record says; a partial record is ignored, and an agent that
prints nothing leaves its usage unknown, never zero.

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

### Quota pause

A `quota` failure never fails the node while pauses remain. A profile fallback
comes first: another chain step runs at once. On the last step, when the
failure text carries a `Retry-After` (seconds or an HTTP date) longer than the
next infrastructure backoff step, the run pauses right away; otherwise the
ordinary infrastructure retries run first and the run pauses once they are
spent. The attempt that ends in the pause is journaled `interrupted` with
`failure_kind: quota` and spends no retry; the node gets no `node_finished`.
The run journals `run_paused` with `reason: "quota (<agent>/<model>)"`, `by:
engine` and an `until` taken from `Retry-After`, or from the schedule 15 min,
30 min, then 1 h for every later pause (`APB_QUOTA_BACKOFF_MS`, a
comma-separated list of milliseconds whose last step repeats, changes it for
tests). The driver exits and leaves the waker behind, like any pause with an
`until`.

When `until` passes, the waker first sends a probe: one minimal call of the
same agent and model with a fixed tiny prompt, no tools and no autonomy grant.
Its outcome is journaled as a `supervisor_action` with action `quota_probe`
(detail `passed: ...`, `quota: ...` or `error: ...`). A probe that hits the
quota again re-pauses the run at the next schedule step without touching the
node; any other outcome resumes the run, which runs the node again. `apb
resume --all` probes a due quota pause the same way; an operator's `apb
resume` resumes at once, without a probe. After 6 consecutive quota pauses of
the same node (attempts and probes alike) without a successful attempt, the
next quota failure fails the node with a `quota exhausted` output. A host task
is no provider call: in a host-mode run the rate-limit wording keeps the
`transient` handling and no probe is sent. Script nodes are never classified,
so they never pause on quota.

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

### The retry briefing

Every attempt that repeats a step gets a briefing on what the earlier attempts
did, as apb recorded it: a section `What earlier attempts of this step did
(apb's record)` appended to its prompt, after the continuation note of a
continued session, after the node prompt of a fresh one, and in the prompt of
a host task. It covers every attempt since the node last succeeded: retries,
infrastructure retries, fallback steps, deadline continuations, resumes after
a pause or a crash, a supervisor retry and a failure route back into the
node. A first attempt, an answer round, a session handoff and a loop visit
after a success get none. It lists:

- each earlier attempt with its agent, model and how it ended;
- the sends of `send-once` functions, grouped as confirmed (do not send
  again; a repeat with the same `--intent` returns the earlier result),
  unknown (what apb will do on a repeat; never send it under another name,
  report it as a finding) and not sent (may be sent again);
- writes to connector functions without a send-once declaration, by the same
  three outcomes;
- the attempts with no connector writes recorded.

At most 30 such lines are listed and the rest is counted. It ends by naming
what apb does not record, so a missing line is never read as "it did not
happen": anything done outside `apb connector call` (shell commands, other
API calls, messages sent another way, git pushes) and the file changes and
commits in the working tree, which the agent checks with `git status` and
`git log`. The briefing sits outside the node's cache key, and the default
retry policy is unchanged.

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

### Bounded agent output

apb never keeps an unbounded amount of what an agent prints. In memory each
stream keeps its beginning (the head, where session ids are) and its end (the
tail, where every supported CLI prints its final record: claude's `result`,
codex's `turn.completed`, opencode's last `step_finish`, or the error
record that ends a failed codex or opencode run); the middle of a
longer stream is dropped. On disk `stdout.log`, `stderr.log`, the `acp`
transport's stream log and the `session.jsonl` copy each stop at a size cap:
the file then ends with one line saying how many bytes were written and that
the rest was not kept, while apb keeps reading the agent, so a cap never
blocks or kills it. On the `acp` transport a single line as long as the tail
or longer is never kept whole either.

The limits are in the `output_limits` section of the global `config.yaml`:

```yaml
output_limits:
  head_bytes: 2097152          # in memory, per stream (2 MiB)
  tail_bytes: 6291456          # in memory, per stream (6 MiB); also bounds an acp line and the acp queue
  log_file_bytes: 67108864     # stdout.log and stderr.log, each (64 MiB)
  stream_log_bytes: 67108864   # the acp stream log (64 MiB)
  session_copy_bytes: 67108864 # the session.jsonl copy (64 MiB)
  stream_line_queue: 1024      # acp lines buffered between the reader and the engine (at most tail_bytes in all)
```

Every key is optional and must be positive. A project `.apb/config.yaml` may
carry the same section to lower a limit for its own runs; a larger project
value is ignored. A run resolves its limits once, when it starts.

An attempt that lost part of its output says so on its `attempt_finished`:
`output_truncated` lists each cut stream (`stdout`, `stderr`, `stream_log`,
`session_copy`) with the bytes kept and the bytes dropped at least, and is
absent when nothing was cut; a stream counts as cut only when neither the
memory copy nor its file holds all of it. When the agent's final record
itself was cut (it fell into the dropped middle, its line was longer than the
tail, or the stream ended inside it) the attempt is never a success: it fails
with "the agent's final result could not be read whole: output exceeded the
limit", classified `agent` (not a quota or a transport failure), and
`transcript_incomplete` is set. A status file verdict still decides the
attempt, as it does for any other abnormal end, since the status file is not
part of the cut output. Token usage then counts only the records that were
kept. `run_status`, `run_report`, `apb runs <id>` and the dashboard journal
show both fields.

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

A list node inside a loop runs a new visit each time the loop reaches it.
With `rerun: all` (the default) every item runs again. With `rerun:
not_passed` the items that passed in the previous visit are carried over
as passed (as long as their content is unchanged) and only the others run,
each told in its item section what it reported and what the check said the
visit before. A reviewer panel is the typical shape: the list node fails
under `on_needs_look: fail`, a bounded edge routes to a fix, the fix routes
back to the list, and only the reviewers still open review again
(examples/playbooks/review-panel-list.yaml). `rerun: not_passed` on a node
that sits in no cycle has no effect, and the validator warns (V93).

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
prompts as `{{run.instruction}}`. Edit it on the Start node in the web editor,
or in the Run dialog. What you type there is remembered for this project
(see "Remembered run input" below): it is NOT part of the playbook
definition, does not create a version and does not change trust, and a frozen
playbook still accepts it. At run start the value is resolved once and
snapshotted immutably into the run.

### Remembered run input

A person who runs a playbook with the same values every time does not have to
type them again: the param values and the instruction can be remembered per
project. The Run dialog's "Remember for this project", `apb run --remember`
and `playbook_run` with `remember: true` keep what the start passed;
`playbook_memory` and `apb memory show|clear` read and forget it.

- Where: `.apb/memory/<project|global>/<id>.json` in the project, git
  ignored. A global playbook remembers different values in every project.
  Its memory file carries a MAC under this installation's key, so a file
  that came with a checkout (not saved by apb on this machine) is ignored
  with a diagnostic, as is a signed file put under another playbook's
  name: a repository cannot seed a trusted global playbook. Without a
  usable config directory for the key, saving a global playbook's input
  fails instead of writing a file the next read would ignore. Making a
  playbook global re-checks the moved values like a save.
- Precedence at start: an explicit value wins, then a compatible remembered
  value, then the param's `default`. For the instruction: an explicit one
  (an explicit empty one too), then the remembered one, then the legacy
  draft. A sub-playbook child takes nothing from memory or the old draft:
  the parent's consent covers its defaults and the instruction the parent
  passes. The
  run's `run_provenance` event lists what came from memory (`remembered`),
  and every start surface says it: `recalled` in the `playbook_run` and
  dashboard answers, a `used remembered input:` line from `apb run`.
- A start remembers its input only once it succeeded (for `apb run`, once
  the run gate and the budget check passed), checked against the
  playbook's current version; a refused start changes nothing.
- Values are bound to the params as they were saved. After the params
  change, each value is checked again: one whose param is gone, whose value
  no longer parses as the param's type, that is not one of the options now,
  or whose param became secret is reported as incompatible with the
  reason, never used. A value that still fits a changed type is used
  (`schema_changed` says the params changed). The next save drops every
  value that no longer fits, so the file never keeps one.
- Never stored: a `secret: true` param, a param whose name says it holds a
  credential (`password`, `token`, `secret`, `api_key` and the like), and
  any value or instruction that looks like a credential (a token prefix
  such as `ghp_`, a JWT, a bearer header, a value after `password=` or
  `api_key:`, a URL with a password, a Slack webhook, a long random
  string). They are refused by name with a reason; the value is not
  echoed. Making a playbook global does not carry an old instruction draft
  that looks like a credential either.
- Reading memory changes nothing: a file that does not parse reads as
  empty with a diagnostic, and the next save moves it aside
  (`<id>.json.corrupt-<time>`). A file of a newer format is left alone.

Behaviour change for an existing project: the old instruction draft lived in
the definition (`meta/instruction-draft.md`), so a project that committed it
shared one draft through git, and a global playbook shared one draft across
every project. That file is now read only, as a seed: it is used when
nothing is remembered, and an edit goes to the project's local memory
instead. Delete the committed file once nobody needs the seed.

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

The answer is written for a person by default: the composer's prompt carries
apb's writing brief (see "Writing for people" below). When the answer is
read by another agent instead (a sub-playbook whose parent parses it, a run
whose caller is a script), set `audience: agents` on the finish node and the
brief is left out. `audience` on a finish without a prompt has no effect
(validator V101, a warning).

## Writing for people

Some of what a run writes is read by a person, not by the next step: the
finish answer, a pull request description, a reminder mail, a chat post, a
note in a CRM. For that text apb adds one fixed brief to the agent's prompt:
lead with the conclusion, say who acts next and what they do, state facts
(numbers, names, links) and mark what was not checked as not verified, use
the reader's words instead of apb's internal terms, keep it short.

The brief goes only where a person reads the output:

- the finish composer, unless the finish node says `audience: agents`;
- an `agent_task` marked `audience: people`;
- a node granted a connector function that declares a `people:` block (the
  official `telegram`, `slack`, `smtp` and `zulip` send functions and
  `github.create_pull`; see "Text people read" in `docs/CONNECTORS.md`).
  Its connector block also names the arguments people read.

```yaml
  - id: draft_reminder
    type: agent_task
    audience: people        # the mail draft is read by the customer
    prompt: >-
      Draft the payment reminder for {{item.name}} ...
```

`audience` is allowed on `agent_task` and `finish` (`people` or `agents`).
Every other prompt stays byte for byte as it was, so cache keys do not
change. The brief shapes how an agent writes; it is not a check. The run's
own report says what was verified and what was not, whatever the answer
claims (see "Run outcome" in `docs/MCP.md`).

To hear about a run while it waits for you (a review, a question, a pause, the
end) through a chat or mail you already use, see `docs/NOTIFY.md`.

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
off. A playbook with a list node (`list:`) is refused in host mode at run
start: list nodes run with `execution: cli` only in this release.

In host mode apb spawns no agent CLI. Every agent step (and a finish answer)
becomes a host task that `run_wait` returns in `pending_tasks`, each with the
full `prompt` (the rendered node prompt, report contract included), the
profile's `role_prompt`, the `skills` paths, the `workdir`, the `env` to set
(`APB_RUN_DIR`, `APB_RUN_ID`, `APB_NODE_ID`, `APB_STATUS_FILE`), the `outputs` contract, a
`deadline` from the node's `timeout_seconds`, and a `model_hint` for fallback
entries and routed tiers. The hint is only the model the profile declares for
that chain step, labelled by `hint_source` (`primary`, `fallback` with its
index, or `tier`), `fallback_of` (the attempt that closed the previous step
and how: `failed`, `expired`, ...) and the one-line `hint_note`; the host
picks the model itself (`hint_note` opens with `advisory:`). Run each with your
own subagent tool in the current session, never by launching another agent
CLI or process such as `claude -p`, and never with permission-bypass flags:
that is the execution contract each task carries verbatim as `execution_note`
(see "The execution contract" in `docs/HOST-INTEGRATION.md`). Independent tasks may run
concurrently. Submit each final reply verbatim with `run_task_submit`
(`succeeded`, `failed`, or `blocked` with the question for the user). The
engine treats the reply like a finished CLI attempt: report block, status
file, `success_check`, `require_verdict` (the submission counts as the verdict
unless the subagent wrote the status file), the completion check, retries and
fallbacks (each a new task, so a fallback's or a tier's task does appear in
host mode: an expired deadline or an unanswered question advances the
profile's chain at once, while a `failed` submission first spends the node's
retries on the same step), loops, gates and resume all work unchanged.
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

<!-- issue #192: forward patches and candidate trials -->
## Supervisor policy (supervisor.policy)

`supervisor.policy` tunes what a run's supervisor may do and how its playbook
patches become the norm:

```yaml
supervisor:
  policy:
    capabilities: [observe, retry, rebind, patch_playbook]  # default: all
    promote_supervisor_patches: on_success  # | always | manual | { after_n_successes: 3 } | { when: quality }
    trial_candidates: next_run              # | off | { share: 0.3 }
```

- `capabilities`: the supervisor tools the run grants (see MCP.md).
- `promote_supervisor_patches`: when a supervisor patch moves `current`.
  For an in-run patch (`scope: current_run`, the run migrates onto it) it is
  decided by the run it was made in; a `workaround` is never promoted. For a
  forward patch (`scope: next_runs`) it is decided by the candidate's trial
  runs: `on_success` and `always` promote on the first successful trial,
  `{ after_n_successes: N }` on the N-th, `manual` never (a person promotes it
  with the dashboard's "Use"; the trials still count and still reject).
  `{ when: quality, compare, min_runs, max_trials, window, margin }`
  compares the candidate's graded trial runs with the graded runs of
  `current`, criterion by criterion, and promotes (only when shown not
  worse, within `margin`), keeps `current`, waits for more trials or hands
  the decision to a person; one failed trial alone rejects nothing. It
  needs a `quality:` block with criteria in force, `outcome` among them
  (V107); a gate that does not read falls back to `manual` (V106); see
  "quality (run quality)" below and docs/QUALITY.md.
- `trial_candidates`: whether a run started without an explicit version runs
  the playbook's candidate version. `next_run` (default): every such start
  does; `off`: none (the candidate runs only when asked for by version);
  `{ share: p }`: a random share `p` (0 to 1) of starts, drawn per start.
  The policy of the `current` version decides.

A forward patch is how a supervisor improves the playbook for later runs
from what it saw in this one, including nodes that already ran: the run
keeps its version, and the new patch version becomes the playbook's
**candidate** (a `candidate` pointer next to `current`, one at a time; a newer
forward patch replaces it, and its provenance names the version it was based
on). A trial run that succeeds with every goal criterion holding counts
towards promotion; a failed trial, or one where a goal criterion did not
hold, drops the candidate (the journal's `candidate_rejected`), and the next
runs use `current` again. A promotion journals `candidate_promoted`.
`run_status`, `apb runs`, the dashboard run page and `apb stats` mark the
trial runs; the version history marks the candidate and each version's
outcome (`promoted`, `rejected`, `superseded`). A start that asks for the
candidate by version counts as a trial too; eval runs and sub-playbook
children never do.

A forward patch may not change the goal, the declared or effective effects,
the irreversible steps, `requires`, the `supervisor` block, the decision
opt-ins in `defaults` (`host_decisions`, `retry_advice`), `worktree`, the
`quality:` block, a node's connector grants or the sub-playbook a node runs; it must pass the
validator and be classified `improvement`. It may bind another profile: the
run gate checks the candidate's profiles on their own. Its base must be
`current` or the candidate on trial (`stale_base` otherwise).

The candidate inherits the trust of `current`, like an in-run supervisor
patch inherits the trust of the version its run started on. When `current`
is approved, a start without a version runs the candidate with no extra
question; when it is not (or a profile, connector or sub-playbook of the
candidate is not trusted), the start runs `current` and reports
`candidate_skipped: untrusted` instead of refusing. A person's
acknowledgement never covers an untrusted candidate; start it by its version
to try it anyway. A version a person saves or promotes by hand ends the
trial (`superseded`).

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

## Will it fit here?

Before a playbook runs in a project, one read-only report tells whether it
can, in four separate sections, so a missing trust approval never hides a
missing file and the other way round:

- **Compatibility**: what the project cannot change by configuration: the
  lifecycle, `requires`, the sub-playbook tree, scripts the version lacks
  (V92) and connectors that do not resolve. Cold session handoffs and
  `protect` globs that match nothing are notes.
- **Setup**: what a person can provide: profiles, skills and the models of
  their executor chains, connector accounts and secret env vars, and param
  values a start must pass (`param_needs_value`) or that memory holds in a
  form that no longer fits (`remembered_incompatible`).
- **Trust**: each digest of the tree (the playbook, imported scripts,
  profile bundles, connectors and accounts, sub-playbooks) with whether it is
  approved and whether an acknowledge at start stands in for it.
- **Consent**: the irreversible sources a start must be agreed to, and the
  effects of the whole tree.

`ready` (no blocking compatibility or setup item) and `trusted` are reported
apart: an untrusted playbook can be ready. `ready` does not mean a start goes
through unasked: `trusted` and `needs_consent` say what a start must still
be approved or agreed to. When the sub-playbook tree cannot be resolved, a
`tree_not_assessed` note says the effects and trust items cover the
playbook alone; memory that cannot be read is a `remembered_unreadable`
note, and memory that is ignored (corrupt, or not saved by this
installation) a `remembered_ignored` note, never a failed report. The same report backs
`playbook_adopt_report` (with `scope` and `workspace`), `playbook_catalog`
with `fit: true` (a summary per entry), `apb adopt [--scope]` and the
dashboard's Fit panel. For a global playbook the report answers for the
project it is asked about: the same playbook may fit one project and not
another while its trust stays the same.

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

## budget (run spend limit)

`budget` caps what a run spends, in tokens, in US dollars or in both. It is a
predictive stop: before each agent node and each `playbook` node, the engine
checks whether what is left still covers the node's median spend in
comparable past runs, and pauses the run before the node when it does not.

```yaml
budget:
  tokens: 400000          # input plus output tokens of the run's agent attempts (cache excluded)
  usd: 4.00               # cost the agent CLIs reported, in US dollars
  on_no_estimate: ask     # ask (default) | pause | continue
```

At least one of `tokens` and `usd` is required. `on_no_estimate` says what
happens before a node that has no estimate (no comparable run reported its
spend) or when what the run spent is unknown: `continue` runs it, `pause`
pauses the run before it, and `ask` makes the person decide at start (see
below); an `ask` nobody answered acts as `pause`. One exception: a run that
already spent its limit (what its attempts reported alone reaches it)
pauses before the next gated node under `continue` too.

**Precedence.** A run argument overrides the block per field: MCP
`playbook_run` and `playbook_execute_plan` take `budget: { tokens, usd,
on_no_estimate }`, `apb run` takes `--budget-tokens 300k`, `--budget-usd 3.50`
and `--on-no-estimate continue`, and the dashboard's Run dialog has a token
and a dollar input. A run may set a higher or a lower limit than the block.
With neither a block nor a run argument the run has no budget and no gate.
An `on_no_estimate` without any limit (in the block or the run) is refused:
`apb run` exits with an error, MCP and HTTP answer `invalid_budget`, and a
block with only `on_no_estimate` is a V96 error. A token amount takes `_`
separators (`300_000`), never a `,`: `1,5m` is refused, not read as `15m`.

**The estimate.** A top-level run with a budget, or one a launch surface
showed an estimate for, writes the estimate it started with
(`runs/<id>/estimate.json`; best effort, a failed write never fails the
start; with run overrides it is computed for the overridden bindings, not
taken from the one shown), from the newest 50 runs of the same playbook
version and execution mode (`cli` or `host`) among the project's newest 200
run directories this apb created (a planted run directory takes no place
among them). A node's spend comes
only from runs where it ran with the same profile digest (its `profile.yaml`
and SOUL, so its model too). `apb run --estimate`, MCP
`playbook_get(estimate: true)` and the Run dialog show it before a start:
time, tokens and cost with a range, how many comparable runs there are, how
many attempts reported no usage, and the nodes with no estimate. Below 3
comparable runs it is indicative only. See `docs/STATS.md` for the method.

**The gate.** Before a gated node starts a visit, per limited dimension:

- the node's need is the median spend of one visit of it;
- what is left is the limit minus what the run spent, minus the medians of
  the members of a parallel batch that were admitted and are still running
  (so parallel branches do not all pass against the same amount);
- an attempt that reported no usage (or no cost, for dollars) is never
  counted as zero: it is estimated with its node's median per attempt, and
  when no median exists what was spent is unknown. What the attempts did
  report is still a lower bound: when it alone leaves nothing (basis
  `exhausted`) or less than the need (basis `median`), the run pauses
  whatever the policy; only while it still fits does `on_no_estimate`
  decide;
- a list node is gated before each batch, with the median per item times
  the items of the batch; a node in a loop is gated at every traversal;
- a node that fell back to another executor in this run (a profile fallback
  in its latest visit, or a host fallback at any time) has no estimate,
  because its history is another executor's.

When less is left than the need, the run pauses before the node (the journal
has `budget_gate` with `decision: paused` and `run_paused` with `by: engine`
and a reason that starts with `budget (`). A plain admission journals nothing.

**At start.** With `on_no_estimate: ask` and a gated node that has no
estimate in a dimension the budget limits (a node with token history but no
cost history counts under a dollar limit), the person decides before the run
starts: MCP refuses the start
with `budget_needs_decision` (the estimate and the nodes) until the call sets
`budget.on_no_estimate`, the dashboard's Run dialog shows a pause or continue
choice, and `apb run` asks at a terminal (without one, `ask` stays and the
run pauses before such a node).

**Resume.** A run its budget paused does not continue on a plain resume: the
refusal (`budget_paused`, `apb resume` exit 2) names the node, what it needs
and what is left. Resume it with a change:

- a raise, added to the limit in force: `--raise-budget-tokens 100k`,
  `--raise-budget-usd 2` (MCP `run_resume` `budget: { raise_tokens,
  raise_usd }`, the dashboard's Resume form);
- a new limit: `--budget-tokens`, `--budget-usd` (`tokens`, `usd`);
- another policy: `--on-no-estimate continue` (`on_no_estimate`);
- or `--over-budget-once` (`once`): the paused node (or the next batch of a
  list node) runs once over the budget, then the gate applies again.

A set and a raise of the same dimension in one resume are refused. A raise,
a new limit or a policy is journaled as `budget_set` with `source: resume`,
and `once` as `budget_gate` with `decision: waived`; what was spent is never
reset. Both are journaled only when the resume is admitted, under the run's
event lock right before its `run_resumed`: a refused resume (environment
drift, a finished run, a busy workdir, another resume that won) journals
none, and a change is applied once per pause, so a second submit of it for
the same pause is refused. A waiver covers the pause it resolved: a later
budget pause, or a resume from another node, drops it. A running run's
budget does not change: pause it first. A budget may also be set at resume
on a run that had none.

**Who changes a budget.** The engine refuses a budget change, and a plain
resume of a budget pause, from a supervisor or from its own waker. That a
person stands behind an operator's change is each surface's rule: `apb
resume` and the dashboard are a person at a terminal or a browser; MCP
`run_resume` answers a `budget` first with
`budget_change_requires_confirmation` and a `budget_nonce`, and applies it
only when called again with `confirm_budget` set to that nonce, after the
agent showed the person the change. Like the irreversible consent nonce it
is not a secret: it makes the agent stop and ask, it cannot prove a person
answered. A run a supervisor agent watches refuses budget changes over MCP
outright (`budget_change_refused`).

**Limits.**

- It is not a ceiling: a node the gate admitted may spend past the limit,
  and the estimate's range is wide on purpose (the medians of the nodes add
  up as if they moved together).
- A sub-playbook's spend counts toward its parent's budget and its
  `playbook` node is gated as one node, but nothing inside the child is
  gated, and the child's own `budget:` block is ignored under a parent.
- What decision models spend is not counted; they keep their own budget
  (`docs/DECISIONS.md`).
- A subscription plan that reports no cost leaves the dollar dimension
  without an estimate: with a dollar limit, `on_no_estimate` decides before
  every gated node, and the start answer warns about it.
- An older apb that reads a journal with `budget_set` or `budget_gate`
  refuses to continue the run only until the next checkpoint, then continues
  it without enforcing the budget.

| Code | Level | What it covers |
|---|---|---|
| V96 | error | `budget` declares neither `tokens` nor `usd` |
| V97 | error | `budget.tokens` is 0, or `budget.usd` is not a finite amount greater than 0 |
| V98 | warning | a `budget` on a playbook with no agent node and no `playbook` node, so it never applies |

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
  - `check: { type: list, node: <id> }`: the latest visit of a list node
    (`list:`) processed every item and none needs a look. It counts the
    engine's own item states, so "every row was handled" no longer rests on
    an agent's tally. Before the finish, run surfaces show where the list
    stands ("list `send`: 12 of 43 processed, 11 passed, 1 needs a look").
    Naming a node without `list:` is validator error **V95**.
- `enforce` (bool, default `false`): a failed `script`, `marker` or `list`
  criterion fails a run that would otherwise succeed, with a `run_error`
  naming the criterion. Without it the results are only reported. V41 warns when
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

## quality (run quality)

A run that succeeded can still have gone badly. The `quality:` block names
what a good run looks like, so every run is graded and versions compare
on more than success (docs/QUALITY.md has the full reference):

```yaml
quality:
  task_type: "{{params.kind}}"   # optional, params only: runs compare within one type
  grade_on_finish: rules         # the default; off leaves grading to apb grade
  builtins:
    scope: { off: "the task is to reorganize the whole repository" }
  criteria:                      # at most 3 of the playbook's own
    - id: first-error
      subject: { node: triage }  # or run
      evidence: presence         # presence | absence | both
      reads: [outputs, transcripts]
      levels:
        good: "likely_cause cites the first error in the log"
        partial: "likely_cause names the step but cites a follow-up error"
        poor: "likely_cause names an error the log does not show"
```

- The built-ins (`outcome`, `loops`, `scope`, `consent`, `skill_use`) are
  graded by rules as every run ends, from the journal and the transcripts,
  without a model. They grade a mail or CRM run as well as a code run:
  irreversible connector calls without consent and repeated failing calls
  come from the journal.
- The playbook's own criteria are graded by a host agent when a person asks
  (MCP `run_grade_start`, `run_grade_read`, `run_grade_submit`), citing the
  records it read in full. A grade keeps references, counts and one short
  redacted `why`, never the source text it read. A run takes one host
  grade until a person reopens it (`apb grade <run> --reopen`).
- Write each level as something checkable against a record. Editing a
  criterion starts a new comparison history (its digest changes).
- `apb grade <run>` grades a run by rules, `apb grade --backfill <id>`
  grades earlier runs, `apb candidate <id>` shows and decides a candidate.

## baseline_check (before and after)

Optional, report-only by default. A goal says what the run is for; a
baseline says what the world looked like before the run touched it and what
it looks like at the end, read by the engine itself rather than by an agent.
Each check reads one source twice: once before the first node that can act,
and once at the finish node, then compares every observation under its
rule.

```yaml
baseline_check:
  enforce: false            # default: report only
  checks:
    - id: unowned           # [a-z0-9_], at most 40
      description: Open deals without an owner
      timeout_seconds: 300  # default 300, at most 1800
      connector:            # exactly one source: connector | script | project_checks
        name: twenty
        account: default
        function: list_opportunities   # must be read_only, not an inbox function
        args: { filter: "ownerId[is]:NULL,stage[in]:[{{params.open_stages}}]", limit: 1 }
      observe:
        - { id: count, type: number, path: totalCount, expect: { equals: 0 } }
```

**When the snapshots are taken.** "A node that can act" is one that takes
the working-tree lock (`agent_task`, `script`, a sub-playbook, a finish
with a prompt), binds a connector, or declares any effect other than
`fs_read`: a human gate or a judge before it does not count. The before
snapshot is journaled once (`baseline_observed`, `phase: before`) and never
taken again: a pause, a quota pause, `apb resume` and a resume from a node
reuse it. A run continued from another (`continued_from`) inherits its
predecessor's before snapshot. A check with no before snapshot when a node
that can act has already started (a playbook patched mid-run, a journal from
an older apb) is journaled `missed`, and its comparisons are unknown. The
after snapshot and one `baseline_compared` per observation are journaled at
the finish, after the finish answer and before the goal criteria; a run that
ends before a finish node has no after snapshot, and the outcome says so. A
run whose nodes never act takes its before snapshot at the finish, right
before the after one. A resume that re-runs nodes that can act takes a new
after snapshot, and a resume after a crash between two `baseline_compared`
events journals the missing ones before judging the whole set. An abort or
a `--now` pause interrupts either snapshot like any node work: nothing of
it is journaled, and the resume takes it again. A finish answer composed
before an interrupted after snapshot is kept: the resume continues at the
snapshot and the answer agent does not run again, unless the run moved on
meanwhile (a node re-ran, a supervisor directive or patch, a resume that
names its start node). A supervisor patch may not
change `baseline_check` once a node that can act has started.

**Sources.**

- `connector`: one call of one function, through the run's own connector
  grant for the reserved site `@baseline` (no node needs to bind it). The
  function must be `read_only` and not an inbox function, and `args` may
  template only `{{params.*}}`, so both snapshots ask the same question. The
  observations are read from the response body. The read is unknown when
  the call fails, its response was not read whole, or `complete_when` (a
  path and the value it must hold; connector sources only) does not hold.
  Numbers compare as numbers there, and a templated value (rendered as
  text) holds a number or a bool written the same way. Use `complete_when`
  only with a condition the page you ask for can meet: with `limit: 1`, a
  `pageInfo.hasNextPage` is true whenever more than one record matches,
  while a `totalCount` does not depend on the page size. The call is
  bounded by the smaller of the check's `timeout_seconds` and the
  function's own `timeout_sec`.
- `script: scripts/<file>`: a script under the version's `scripts/`, pinned
  by the run's script digest like a goal script, run with `sh` in the run's
  working tree with `APB_RUN_ID`, `APB_RUN_DIR` and `APB_BASELINE_PHASE`
  (`before` or `after`). Its last non-empty stdout line is one JSON object
  the observations read from; `"complete": false` in it, a non-zero exit, a
  timeout, more than 1 MiB of output, or any change to the git working tree
  makes the read unknown. The working tree must be a git work tree with at
  least one commit: one that cannot be checked (not a git repository, or
  git failed) reads unknown with "the working tree could not be checked".
- Scripts and project commands never see a connector secret: every env var
  an installed connector references is removed from their environment, as
  from every agent and script node.
- `project_checks: { only: [test, lint, typecheck, check] }`: the project's
  own commands, found without running anything: `package.json` scripts with
  the package manager its lockfile names (`bun`, `pnpm`, `yarn`, otherwise
  `npm`), `cargo check` and `cargo test --no-fail-fast` for a
  `Cargo.toml`, and recipes named `test` or `check` in a make file
  (`GNUmakefile`, `makefile`, `Makefile`, the first that exists, as make
  reads them) or a `justfile`. `only` narrows the kinds; omit it for all.
  The commands are discovered once, at before, and the after snapshot runs
  exactly those. Its observations are fixed (no `observe`): per command its
  status and, for `cargo test`, the failing tests, each named by its test
  binary (a doctest without its line number); the list is read only when
  every test binary printed its result, and more than 500 failing tests
  read unknown. The rule is "no new failures": a command that passed must
  still pass; one that failed may fail only on tests that already failed.
  For a runner other than `cargo test`, a command that fails on both sides
  reads unknown. The commands run under the same tree rule as a script: a
  command that writes into the tree (an untracked lockfile, a build output
  that is not ignored) or a tree that cannot be checked reads unknown.
  What a command is compared against is its definition only: the
  `scripts.<kind>` entry of `package.json` with its `pre<kind>` and
  `post<kind>` scripts, every make or just rule that names the recipe with
  its body, and the `Cargo.toml` without its dependency tables (targets,
  auto-discovery flags, features and workspace members decide which tests
  run). A definition that changed between the snapshots, or a manifest
  that is gone, reads unknown; a dependency bump in the same manifest does
  not change it. A definition is not followed into what it calls: another
  script it runs (`npm run lint && jest`), a make prerequisite's recipe, a
  make variable set elsewhere in the file and a workspace member's own
  `Cargo.toml` are not compared, so keep the checked command
  self-contained when that matters.

**Observations.** Each has an `id`, a `type`, a dotted `path` into the read
document (`data.items[].id` maps over an array) and exactly one rule in
`expect`:

- `number`: `equals`, `at_most` or `at_least: <n>`, or `change: decrease |
  increase | no_increase | no_decrease | unchanged`. `count: true` reads the
  length of the array at the path.
- `list`: `change: no_new | shrink | unchanged`, or `empty: true`.
- `field` (a string, bool or number): `equals: <value>` or `change:
  unchanged`.

A path with `[]` reads a list, so it fits only a `list` or a `count: true`
number. A missing key, a type mismatch, a number that is not finite, a list
of more than 500 items, an item or a field text longer than 256
characters, or text with a control or invisible format character reads
unknown, never zero, empty or truncated. A list is a set: an item that
appears twice counts once, and an integer item keeps its exact digits.

**Verdicts.** Each comparison is `met`, `not_met` or `unknown` (either side
unknown, no before snapshot, or the check's input changed between the two
snapshots: other rendered args, an edited script, an edited test
definition). `regressed` marks an after value strictly worse than the
before one in the rule's direction. With `enforce: true` anything but `met`
fails a run that would otherwise succeed, with a `run_error` naming the
check and the observation; without it the results are reported only.

**What it is not.** A baseline shows the change; it never says whether the
goal was reached. The run outcome reports "goal reached" from the goal
criteria only, so give a playbook with a baseline a goal with checked
criteria (V66 warns otherwise).

Two worked examples ship in `examples/playbooks/`:

- `crm-cleanup.yaml`: the connector check above. A run that assigns 17 of
  37 owners before a network error reports `unowned/count: 37 -> 20, equals
  0: not met`, with the CRM's own counts, while the marker criteria still
  say what the agents reported.
- `dependency-security-fix.yaml`: `project_checks: { only: [test] }` under
  `enforce: true`. Two tests that failed before the fix may still fail; a
  third failing test fails the run and is named.

Validator codes: **V62** (structure: no checks, a duplicate or malformed id,
not exactly one source, `observe` missing for a connector or script source
or present for `project_checks`, `complete_when` on a source other than a
connector, an unknown `only` kind, `timeout_seconds` out of range), **V63**
(source safety: a script outside `scripts/`, and with the installed
connectors known a function that writes, is an inbox function or is not
declared; a malformed connector or function name is V23, as for a node),
**V64** (a rule that does not fit the observation type, not exactly one
rule, a non-finite number, a malformed path, a `[]` path on anything but a
list or a count, `count` on a non-number), **V65** (connector args or
`complete_when` templating anything but a declared `params.<name>`), **V66** (warning: a baseline without a goal, or with only
manual criteria), **V67** (warning: a script or `project_checks` source while
`worktree` reads a node output, so the two snapshots would run in different
trees), **V68** (a node id that starts with `@`: those ids, `@baseline` and
`@notify`, are reserved for the engine).

`run_status`, `run_report`, `apb runs <id>` and the dashboard run page show
each check and observation with both values and the verdict; `apb stats`
counts each observation's results across runs.

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

List nodes (`list:`) run in evals like any other node, except that a
`connector` item check is a connector binding site and is refused like
every other one (V81). To assert the counts, give the playbook a `type:
list` goal criterion: `checks.goal: required` then needs every item
processed with none left for a look, and an `events` check may name
`list_started`, `list_batch_started` and `list_item_settled`.
<!-- end 0.24.0 eval suites -->

## Secrets

Never put secret values in a playbook or in a capture synopsis. Reference them
by env or config key name, or a `secret: true` param (see "Secret params").
Concrete secret-looking values are rejected at capture and should never be
committed to a definition.

## Sharing a playbook

`apb export <id> --out file.json` writes one version as a bundle: the YAML
(in the form a save writes it, so a hand-edited version loses its comments
and formatting here, as on any save), the editor layout, the scripts, and an
`origin` block (the exporter's digest of the version, its scope, the apb
version and the export time). The digest in `origin` is the one an import
of the bundle under the same id and version computes. Nothing in `origin`
is trusted by the importer; it only tells where the file says it came
from.

`apb import file.json` always prints a preview first: whether the playbook is
new or the bundle equals the current version, the definition diff against the
current version (nodes added, removed and changed, edges added and removed),
the scripts it adds, removes and changes, the validation errors that would
refuse it, and the bundle digest. `--preview` stops there. A bundle equal to
the current version (naming no other version) writes nothing: apb prints
"nothing to import" and exits 0, with or without a terminal. A bundle the
import would refuse (validation errors, or a version that already exists
with other content) stops after the preview with exit 2, before anyone is
asked to accept it. Otherwise, to import, accept that exact file: on a
terminal apb asks y/N; without a terminal pass `--accept <bundle digest>`
from the preview.

Behaviour change: without a terminal and without `--accept`, `apb import`
now exits 3 and writes nothing (it used to import at once), except for the
unchanged and refused cases above. A wrong `--accept` is refused with
exit 2.

The version an import writes records where it came from (`created_by:
import`, the bundle digest, the file's base name, the declared id and
version, the scripts by sha256 and the final trust digest); the dashboard's
version history shows "imported from <file>". A version imported with
scripts carries code nobody on this machine wrote, so it runs only after an
approval that names its exact digest, for the playbook and every
sub-playbook:

```sh
apb trust approve <id> --version <version> --digest <digest>
```

(MCP: `playbook_approve` with `digest`). Without a digest the approval is
refused and prints the digest and the scripts as they are now; an
acknowledge at start does not stand in for it (refusal
`imported_scripts_require_approval`), and a script changed after the approval
needs a new one. The rule holds for every way a version runs: a trial
(`playbook_trial`) and a sub-playbook child are refused the same way. It
follows the scripts too: a version saved or patched on top of an imported
version nobody approved yet keeps the import record, so it needs the same
approval by digest. Once an imported version is approved, a save that
carries its scripts unchanged is trusted like any save. The dashboard marks
a waiting version "awaiting approval".

Over MCP this approval records consent; it does not stop an agent. The
refusal names the digest, so an agent can approve by sending it back: the
tool asks the agent to show the scripts to the person first, and the
approval is recorded as the agent's (`agent_generated`). A person approves
on the CLI or the dashboard by naming the digest themselves.

## Making a playbook global

A project playbook that every project could use is made global in one
command:

```sh
apb make-global <id> --dry-run
apb make-global <id> --variant trash|retire|keep [--move-profiles]
```

(MCP: `playbook_make_global`). It copies the playbook with every version to
the global store; digests depend on content only, so the copy keeps its
trust. The project's remembered input moves to the global scope (with
`keep` it is copied, since the project copy keeps running here). The
`variant` decides what happens to the project copy: `trash` moves it to the
project trash (restorable), `retire` keeps it retired so the global one wins
here, `keep` leaves both: while both are active and trusted the catalog
marks the pair ambiguous until one is retired, otherwise the global one
shadows the project copy. A step that fails undoes the earlier ones (the
global copy, the copied profiles, the memory), so a retry starts from the
same state.

The dry run lists what stops it (a global playbook with the same id, a
candidate version on trial, a version that fails validation as a global
playbook, an `auto` profile only this project has: pass `--move-profiles` to
copy it, unless it uses project skills, an `auto` sub-playbook only this
project has) and what to know (an explicit `scope: project` sub-playbook
resolves per project, connector accounts this project configures, a global
profile of the same name that differs, other projects on this machine with
their own playbook of this id, which are reported and never changed; a
global profile counts as different when its own files or the content of a
skill it uses differ). Once applied, the answer ends with the global
playbook's fit report for this project (none, with a warning, when it
cannot be built); a dry run and a blocked run answer with the plan only.

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

## Retrospective node (learning across runs)

A run that succeeds slowly or wastefully raises nothing, so nothing improves
by itself. A final `retro` node hands the next runs what this one learned:

```yaml
nodes:
  # ... the playbook's own steps ...
  - id: retro
    type: agent_task
    profile: reviewer
    prompt: |
      Review this run of the playbook and write retro.md.

      {{run.retro}}

      Read the run's artefacts as well (the files the nodes wrote, the
      attempt transcripts under .apb/runs/{{run.id}}/attempts/). Write each
      finding as: what happened, the evidence (node ids, durations, attempt
      numbers, file paths), and the target to change (a node prompt, a
      profile, a script, an expected_duration). Only findings the next run
      can act on; no general advice.
  - { id: done, type: finish, outcome: success }
edges:
  # ... -> retro -> done
  - { from: retro, to: done }
```

- `{{run.retro}}` gives the node the numbers, so it does not re-derive them
  from the journal: which node was slow against its `expected_duration` and
  against the median of the last runs of the version, which ones retried or
  fell back, what each attempt cost and which model actually ran it (in host
  mode, the model the host reported on submission).
- Keep `retro` the last node before `finish`, so every other node has
  finished when its prompt renders. Its `expected_duration` should be short;
  it reads, it does not redo the work.
- `retro.md` changes nothing on its own. A supervisor (`supervise: "self"`)
  reads it, or calls `run_retro_context` for the same numbers as data, and
  turns the findings it accepts into a forward patch for the next runs with
  `supervisor_patch_playbook` and `scope: next_runs` (see MCP.md). Findings
  about the project's own files (skills, scripts, libraries) belong to the
  project: a later step or a person changes them.
- Gate the node with `human_review` after it when an owner should see the
  findings before anything is patched.

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
  as an `artifacts_committed` event, and the run report lists them, flagging
  a commit whose trailer is missing or names another run. A final
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
