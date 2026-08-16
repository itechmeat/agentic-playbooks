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
which configured accounts the node may use; `max_calls` is an optional per-node
budget. The binding is covered by the playbook digest, but the connector folder
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

### Status file (APB_STATUS_FILE)

When a node carries a `success_check`, each attempt is handed an
`APB_STATUS_FILE` environment variable pointing at a per-attempt JSON file in
the run directory. The agent MAY write its final verdict there as
`{"status": "success"|"failure", "outputs": { ... }}`, where `outputs` is an
object of values the step should expose to later steps. The engine reads that
file first to decide the attempt's status and outputs, and falls back to the
existing marker and text parsing when the file is absent, unreadable, or
invalid. The prompt builder appends a note describing this contract only when
the node has a `success_check`; nodes without one keep the report-only
contract.

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
profile: see PROFILES.md's `hermetic` guidance, which suppresses the appended
turn at the source instead of filtering around it.

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
`wait`, `finish`. A playbook needs exactly one `start` and at least one
`finish`. Edges connect node ids; conditional edges gate on node status,
review status, an output substring match, or one structured field of a node's
output.

## Template variables

A node prompt (`agent_task`, `prompt`), a `playbook` node's `instruction`, and
a finish node's `prompt` are rendered as templates before use. This is the
exact accepted set; any other `{{...}}` reference is rejected at save time as
a V13 validation error:

- `params.*` - a declared playbook param's value, by name (`params.<name>`).
- `nodes.<id>.output` - the node's output text.
- `nodes.<id>.report` - the same value as `.output` (an alias; both names
  resolve identically).
- `nodes.<id>.review_note` - the reviewer's note from a `human_review` node's
  decision.
- `nodes.<id>.rejected_output` - the agent report text a `success_check`
  discarded on the node's last rejected attempt (see Success checks). Empty when
  the node was never rejected; a later rejection overwrites an earlier one.
- `run.instruction` - the run's input prompt (see below).
- `run.context` - the accumulated run context (params, instruction, node
  outputs, reviews, hooks), the same text a finish-with-prompt agent sees.
- `run.hooks.*` - the payload last posted to a `wait` node's webhook, by key
  (`run.hooks.<key>`).

An unresolvable reference (an unknown param, a node id that is not in the
playbook, a namespace outside this list) fails validation before the
playbook can be saved or run, rather than silently rendering empty at run
time.

Whether a reference resolves and whether it has a value yet are separate
questions. A template that reads `nodes.<id>.output` or `nodes.<id>.report` where
nothing in the graph orders `<id>` before the reading node is validator warning
**V38**: across un-joined parallel branches that value may render empty. The
remedy is to route the read behind `<id>` itself, or behind a node that already
joins both branches (see "Joining parallel branches"). Adding `join: all` to the
reader does nothing when the reader has a single incoming edge, which is the
common shape this warning catches. A loop-carried read, where both nodes sit in
one cycle, is not flagged: there the previous pass supplies the value.

At run time the same hole is observable rather than silent. When a node executes
and one of its `nodes.<id>.output|report` references renders empty, the run
journals a missing-input anomaly naming every empty reference and why it is empty
(`never ran`, the source's own status, or `<status> with empty output`), and in a
supervised run that anomaly wakes the supervisor. The criterion is the rendered
value, never the source's status: a reference is reported only when the source
has no recorded output at all or its output is the empty string. So an
`on_failure` handler reading the failure it handles stays silent, because a
failed node's own text is recorded and does render, while a source that succeeded
with nothing to say is caught. One anomaly per node execution lists all of that
node's holes. A finish node composing an answer is checked the same way; a node
served from the cache is not, because neither its execution nor its capture runs.

## Human review and conditional edges

A `human_review` node pauses the run for a human decision:

```yaml
- { id: review, type: human_review, options: [approve, reject] }
```

`options` is a required list of strings: the choices a reviewer can pick.
`review_decide` records one of them as the node's decision, plus a free-form
note (available downstream as `{{nodes.review.review_note}}`).

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
  (`true`, `3`).

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
timed-out node extra same-executor attempts it never had. The one exception is a
`require_verdict` node, where a timeout or a dropped transport says the work may
well have continued and is worth one more attempt on the same executor, so those
count as transient unless the text says something more specific.

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
`irreversible` for anything that cannot be rolled back (deploys, publishes,
external notifications) so the policy layer requires explicit confirmation.

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
    criterion.
  - `check: { type: marker, marker: <string> }`: the marker string is
    expected in the run result.
  - `check: { type: script, path: <relative path> }`: a check script
    confirms the criterion. Script execution is not wired into run verdicts
    yet; the field records the contract.

The goal is the contract of the run: agents and supervisors may adapt the
process, but must never weaken or rewrite the criteria; only a person may
change them.

## Secrets

Never put secret values in a playbook or in a capture synopsis. Reference them
by env or config key name, or a placeholder param. Concrete secret-looking
values are rejected at capture and should never be committed to a definition.

## Language

Machine fields (`id`, canonical `trigger.when` / `avoid_when`) are English.
Display `name`, human `description`, and node prompts may be in any language.
Anything you say to the user about a playbook should be in the language of
their recent chat.
