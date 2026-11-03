# Execution reliability: run outputs, read-only nodes, unattended runs

Date: 2026-11-03. Status: accepted. This is the second part of the
execution reliability work; the first part, deferred retries of one node,
is [2026-11-02-deferred-retry-design.md](2026-11-02-deferred-retry-design.md).
Author-facing usage: `docs/HOWTO-authoring.md` ("Run outputs", "Read-only
node", "A review in an unattended run", "Bounded loops"), the guarantees in
`docs/ISOLATION.md` ("Read-only nodes"), operations in `docs/CI.md`
("Unattended runs"), the controls in `docs/GUARDRAILS.md`, and the
connector side in `docs/CONNECTORS.md` ("`on_unknown`").

## 1. Goal

A playbook that runs well when a person watches it still fails three ways
when it runs as part of something larger:

1. **Whoever started the run cannot read its result.** The values a run
   produced sit in node outputs, and a pipeline that wants "the pull
   request URL" has to know which node wrote it, in which branch, on which
   loop pass. A run should hand back named values, decided by a rule the
   journal alone settles.
2. **A node that should only read can still write.** An audit, a review or
   a report runs an agent or a script on a tree it must not change. Today
   the only tool is `protect`, which lists globs and restores them; there is
   no way to say "this node changes nothing" and have it hold.
3. **A run nobody attends waits forever.** A run in CI that reaches a
   review, a question or a budget pause parks on a disk the job is about to
   throw away. The job either times out or reports the wait as a failure it
   cannot explain.

The same change names the run's dead end after a spent bounded edge,
words an unknown send that a resend followed truthfully, and carries the
fixes found when the deferred retry was audited.

## 2. Run outputs

### 2.1 Declaration

A playbook-level `outputs:` block, additive to schema 2, names the values a
run hands back:

```yaml
outputs:
  summary:  { from: { node: report } }
  pr_url:   { from: [ { node: publish_a, field: url },
                      { node: publish_b, field: url } ] }
  verdicts: { from: { node: review, field: verdict }, pick: all }
  checked:  { from: { node: files, item_field: status } }
```

A source is a structured object (a node, and at most one of `field` and
`item_field`), never a dotted string, because a node id may contain dots.
Names match `[a-z][a-z0-9_]*`, at most 64 characters and 64 outputs.
`pick` is `last` (the default), `first` or `all`. Validator rules V131 to
V135 check names, sources and declared fields.

### 2.2 Projection

The value of an output is a pure function of the journal and the
definition the run executed last. It is computed when a surface reads the
run and never journaled.

1. The candidates are the executions of the source nodes whose
   `node_finished` says `succeeded`, in journal order. A failed, timed-out
   or cancelled execution gives none. A finish node counts like any other.
2. `last` takes the candidate settled last across every source, `first` the
   one settled first, `all` every one in order. Alternative branches and
   loops need no rule of their own.
3. Without `field` the value is the output text. With `field` it is that
   key of the output read as a JSON object, kept as JSON; a `null` reads
   `null`; an absent key or an output that is no object reads `missing`.
   With `item_field` every item of a list execution reads on its own.
4. A state is `value`, `null` or `missing`, and `missing` carries one
   reason: `not_run`, `not_succeeded`, `field_absent`, `not_json_object`,
   `truncated`, `item_not_passed` or `definition_unknown`.
5. Every value names its source: the node execution, the journal seq that
   settled it and, for an agent, the attempt that produced it.
6. The projection is `final` once the run is finished (not necessarily
   succeeded); a revival or a `continue_from` sets it back to false.
7. When the last segment's definition is not established (an older apb
   resumed the run), the run's current `playbook.yaml` is read, and every
   output reads `missing` (`definition_unknown`) if that file declares
   outputs differently from the definition the run started with.
8. A failed run projects the same way. A missing output never changes the
   run's outcome.

The projection is `run_outputs` on MCP `run_status`, `run_report` and a
finished `run_wait`, on the dashboard's run detail and its Outputs card,
and in `apb runs <id>`; `apb runs <id> --outputs-json` prints exactly the
projection for a pipeline. The projected text goes through the run-tree
redactor, which withholds a value it cannot redact safely rather than show
it, and reads JSON text by its tokens so a secret split across escapes is
still found. The raw `outputs` map, `{{nodes.<id>.output}}` and the run
`answer` are unchanged.

## 3. Defects fixed beside it

### 3.1 The dead end after a spent bounded edge

A node that succeeded (most often a decided `human_review`) and finds no
edge to take used to end the run with a bare engine error. The drive now
ends the run `failed` through the unhandled-failure stop, with a run error
that names the node, its decision or status and the bounded edges whose cap
is spent, by their edge address: ``no edge can be taken from `review`:
decision `rejected`; exhausted: e:review>fix#1``. The validator warns
(V136) when a review's decision is routed only by bounded edges and the
review has no fallback edge and no unbounded edge for it; the fix is to put
the cap on the edge that leads back into the review.

### 3.2 An unknown send followed by a resend

`on_unknown: resend` lets a repeat of an unknown send go out again. The
resend is a new send with its own nonce and proves nothing about the first
one, which stays unknown and needs a person. The behaviour is kept; the run
outcome, the `resolve_intent` next action and a list item's row now say so
("a resend went out (si-...); whether this first send also went out is
not known", and "earlier send unresolved" on a passed item).

### 3.3 Deferred retry fixes

Found when the first part was audited, and described in its spec: a due
retry beside a parked node runs; the release is journaled
under the event lock only while nothing arrived since the drive last
looked; the waker lease is opened without following a link and without
blocking; a waker reports honestly whether it rearmed; a retry-now is
attributed to a person only from a terminal outside a run step or a
dashboard with keys; the test-only duration knobs work in debug builds
only.

## 4. Read-only node

### 4.1 The guarantee

`read_only: true` is a node-level field, valid on `agent_task` and `script`.
It sits on the node, not inside the node kind, so that an author who writes
it on another kind gets an error instead of having it dropped. What it
guarantees depends on where an attempt runs, and every attempt records its
tier:

| Tier | When | Guarantee | Not covered |
|---|---|---|---|
| `enforced` | the attempt runs in the sandbox (`isolation: full`, or `best_effort` where the sandbox works) | the tree and the repository's git directory (the common directory of a linked worktree included) are mounted read-only; a write fails at the operating system, ignored files included | the attempt's run paths and other writable sandbox paths, temporary files, network effects, connector calls |
| `detected` | no sandbox: every script, `isolation: none`, `best_effort` without a sandbox, a host step, a sandbox that could not be set up | a change to a tracked or untracked non-ignored file under the tree (content, executable bit, symlink target, creation, deletion), to `HEAD`, the refs or the staged index fails the node with `read_only_dirty`, without retry | ignored files, changes outside the tree, a change made and undone inside the attempt, writes by other processes during the attempt, effects outside the file system |

A node that needs prevention declares `isolation: full`: where the machine
cannot isolate, the node is refused with `isolation_unavailable`, so it
gets prevention or nothing.

### 4.2 Decisions

1. **The detected tier compares fingerprints, never copies.** The state of
   the tree is one entry per file (digest and executable bit), symlinks as
   entries with their target (never followed), `HEAD`, a digest of the refs
   and a digest of the staged index, taken over the same listing `protect`
   uses (tracked files and untracked ones git does not ignore; `.git` and
   `.apb` excluded). Outside git, and in a tree its enclosing repository
   ignores as a whole, every file is listed instead, with no repository
   state. Nothing is copied, so no content is stored and nothing can be
   restored. Every changed file, a secret-shaped one such as `.env` too, is
   compared by digest and named only.
2. **Limits.** At most 20000 entries and 512 MiB hashed per state. Above
   either the attempt records `detected_partial` with its counts: a file
   past the byte limit is compared by size and modification time, the
   entries past the entry limit are not compared. A limit never turns into
   "no change".
3. **The root is the node's working directory** (the run's working tree or
   the node's `workdir`), as for `protect`, not the attempt's working
   directory: without a sandbox an agent attempt runs in a scratch
   directory, and a state taken there would miss every write to the tree.
4. **Taken per attempt, right before the spawn.** A change a person makes
   while the run waits (a deferred retry, a release, a pause) is never
   blamed on the node.
5. **Fail once, at once.** A violation journals `read_only_violated`, then
   the attempt `failed`, and the node ends `failed`. It bypasses every
   recovery path: ordinary retries, retry advice, deferral, the next
   profile of the fallback chain, the host fallback, the continuation of a
   `require_verdict` node, the infrastructure and quota paths. The node's
   failure edge or `defaults.on_failure` applies, else the unhandled-failure
   rule, whose run error names the node. It is never a dead end.
6. **Evidence.** `runs/<id>/read_only/<node>/<n>.json`, private: the changed
   paths, scrubbed, at most 200 and a count of the rest, and `git diff
   --stat` for tracked files. No content. It is written for every check in
   the detected tier, clean or not, under the next free `<n>`.
7. **The enforced tier binds read-only.** The tree is bound read-only after
   the extra paths, and the git common directory, bound read-write for an
   isolated attempt otherwise, is bound read-only too. A `writable` extra
   path inside the tree would be covered by the read-only bind, so the
   attempt is refused, at every isolation level, with
   `isolation_unavailable` and a reason that names it. The attempt's run paths (its status file, skill copies, run-scoped
   home) stay writable as today.
8. **Batching.** Whether a node may join a concurrent batch is decided from
   the playbook, before an attempt knows whether it gets a sandbox. So a
   `read_only` node may batch only with `isolation: full`, which always
   means the enforced tier or a refusal; every other level counts as the
   detected tier and runs alone on the sequential arm, as a node with
   `protect` does. An enforced attempt has no after-check, so a sibling's
   write is never blamed on it.
9. **The tier is its own event.** `read_only_checked { node, attempt,
   check }` carries the tier with the entry and byte counts and the limit
   that made the state partial, right after the attempt's
   `attempt_started`, and before a script's run (a script journals no
   attempt start). It is decoded leniently and is no checkpoint. It is not
   part of `isolation`, because an attempt without isolation records no
   isolation at all, nor a field of `attempt_started`, which every
   attempt of every node writes.
10. **List nodes are refused** (V138). Items share one tree and run at once,
    so a per-item state could not tell which item changed the tree, and a
    list retry would queue a dirty item again. Support with one item at a
    time or with `isolation: full` is left for later.

### 4.3 Validator

| Code | Severity | Rule |
|---|---|---|
| V137 | error | `read_only` on a kind other than `agent_task` or `script` |
| V138 | error | `read_only` on a list node |
| V139 | warning | `read_only` with `protect` (redundant) |
| V140 | warning | a `publish` node publishes from a `read_only` node |

A `read_only` node with a `workdir` cannot be isolated (V47) and is always
in the detected tier.

### 4.4 Not covered

- A `read_only` node's `success_check` runs after the check, and in the
  detected tier outside any sandbox.
- A driver crash loses the state taken before the attempt, so a reaped
  attempt is not judged.
- A `read_only` node still counts as acting for the baseline check and the
  working-tree lock.
- Preventing writes without the sandbox, restoring the tree, ignored files
  in the detected tier, effects outside the file system.

### 4.5 Surfaces

The tier is shown per attempt by `apb runs <id>`, MCP `run_status`, the HTTP
run detail and the run page's Isolation card; a partial check says so.

## 5. Unattended runs

### 5.1 Decisions

1. **A run-level mode that fails fast.** `apb run --unattended`, MCP
   `playbook_run { unattended: true }` and the HTTP start body
   `{ unattended: true }` start a run nobody attends. Every wait without a
   declared outcome ends the run `failed` at once with a run error that
   names the node and starts with `hitl_required:`. It goes through the
   unhandled-failure stop directly: no failure route is taken, because a
   route could lead into another wait.
2. **Recorded once, in the manifest.** The run manifest, written once at
   start, holds `unattended`, and every drive reads it from there. The run's
   `run.yaml` carries a copy for the drive and for child runs, but an older
   apb rewrites that file, so it is only a cache: such a rewrite cannot make
   the run attended for an apb that knows the field. No resume request can
   change the mode.
3. **No consent is implied.** The mode is not a person: an irreversible
   start still needs its nonce, the launch policy is unchanged, and an
   `agent_launch: ask` start without a terminal is refused as before.
4. **A review may declare its outcome.** `human_review` gains a node-level
   `unattended: fail | { decide: <option>, note?: <text> }`, `fail` by
   default. It is a node field beside `auto_decide`, never inside it,
   because older binaries parse `auto_decide` strictly and would refuse the
   playbook.
5. **The decision is journaled as a decision.** An unattended decision
   journals `review_requested`, posts the decision on the review channel,
   then journals `review_decided` with `decided_by: unattended`. The channel
   entry keeps later visits of the gate aligned, since decisions are
   consumed by position. `decided_by` is a typed field with one value; a
   value this apb does not know reads as absent. Each visit of the gate
   decides again.
6. **It is never consent.** The rules grader does not count an unattended
   approval as a person's consent before an irreversible step.
7. **What it may not pass.** The validator refuses a `decide` whose option
   reaches, before the next `human_review` or `publish`, a node with the
   effects V73 refuses for `auto_decide` (declared or implied `irreversible`
   or `secrets`, a granted connector function flagged `irreversible`, a
   step that looks like a merge, push, deploy or publish), or whose
   playbook declares those effects (V142). The walk starts only from the
   edges that can take the chosen option. The run checks it again at the
   gate with the effects of its pinned sub-playbooks.
8. **No dead loop.** An option routed only by bounded edges is an error for
   `decide` (V143), not the warning V136 gives an attended review: an
   unattended loop would have no way out once the cap is spent. If a run
   still gets there (the validator was skipped, the definition patched),
   the dead-end rule of 3.1 ends it `failed`, naming the gate and the edge.
9. **Refused at start.** An unattended start that would execute its agent
   steps in the host, run under a supervisor (`apb run --supervise`, MCP
   `supervise: self`) or in another supervised mode is refused before any
   run file exists: a start action `fix_requirement` with a manual remedy,
   policy `unattended_unsupported`. An unattended MCP start never falls back
   from an agent CLI to the host.
10. **The budget fails the run.** Where an attended run would pause for its
    budget, an unattended run fails with a run error that starts with
    `budget_exhausted:` and journals no pause, on all three admission sites:
    a node on its own, a member of a concurrent batch and the list pool.
    The gate records the decision as exhausted; an older reader reads it as
    unknown. Spending past the budget without a person would need its own
    explicit flag.
11. **It never releases.** An unattended run that waits for a deferred
    retry sleeps in its own driver, like a child or an eval run, instead of
    releasing the run to a detached waker. A CI job's foreground `apb run`
    therefore ends with the run's real outcome instead of exiting 4 and
    leaving a waker its container would kill.
12. **Children.** A child run inherits the mode. A child that fails
    `hitl_required` or `budget_exhausted` stops its parent with the same
    code and ``via child <run>`` in the reason, through the same stop,
    instead of letting the parent take its own failure route.

### 5.2 Every wait kind

The action named here is the one an attended run shows at that wait; its
`unattended` field says what an unattended run does there. A run that
already failed `hitl_required` or `budget_exhausted` shows `read_report`,
like any terminal run.

| Wait | Unattended run | Next action; `unattended` |
|---|---|---|
| `human_review` without `unattended` | fails `hitl_required` | `decide_review`; `fails_run` |
| `human_review` with `decide` | decided at once, `decided_by: unattended` | `decide_review`; `decides { option }` |
| `publish` | fails `hitl_required`; no outcome can be declared | `decide_publish`; `fails_run` |
| question with `default_answer` | answered at once, `answered_by: unattended` | `answer_question`; `answers_default` |
| question without `default_answer` | fails `hitl_required` | `answer_question`; `fails_run` |
| host task | refused at start | `submit_host_task`; `refused_at_start` |
| supervisor park | refused at start | supervisor actions; `refused_at_start` |
| timer `wait` | elapses and proceeds | `wait_again`; `proceeds { timer_elapses }` |
| webhook `wait` | waits for its signal; the timeout takes the failure route | `wait_again`; `proceeds { wait_times_out }` |
| deferred retry | retries at its time, never releases | `retry_now`; `proceeds { retry_attempt }` |
| budget pause | fails `budget_exhausted` instead of pausing | `resume_with_budget`; `fails_run` |
| quota pause | pauses and resumes at `until`, still unattended | `resume_run`; `proceeds { resume_attempt }` |
| operator pause | unchanged, a person's act | `resume_run`; not a wait |
| start that needs consent | refused before the run, as attended | start action `confirm_irreversible_start` or the launch question |

`NextAction` gains `unattended`, computed for every run, attended or not:
`proceeds { auto }`, `decides { option }`, `answers_default`,
`fails_run { code }` or `refused_at_start { code }`. A CI author runs the
playbook once with a person and reads which gates still need a
declaration. `retry_now` on an unattended run keeps today's attribution.
The computation lives beside the next-action verdict, not inside it, so
the verdict and its exit codes are unchanged. A list item never asks a
question (`interactive` is refused on list nodes), so items have no case of
their own; a `hitl_required` failure notifies as any `finished:failed`.

### 5.3 Validator

| Code | Severity | Rule |
|---|---|---|
| V141 | error | `unattended` on a kind other than `human_review`, or `decide` names an option the review does not offer |
| V142 | error | the `decide` option reaches a node with `irreversible` or `secrets` effects (or the other V73 evidence) before the next gate, or the playbook declares them |
| V143 | error | the `decide` option is routed only by bounded edges |

### 5.4 Not covered

Required or optional gates beyond the `unattended` field; permission
prompts of an agent CLI (only the node timeout ends them); an unattended
start from the dashboard (the run page shows the mode); spending past the
budget.

## 6. Journal and older binaries

The writing apb is checked against two older releases, 1.0.0-beta.1 and
0.25.0, by the journal corpus. Neither denies unknown fields on a playbook
or a node, so every new definition field is dropped there, not refused.

| Change | Both older binaries | Rule |
|---|---|---|
| `read_only_checked`, `read_only_violated` | read: skipped and counted; a `node_finished` checkpoint (or the pause that interrupts the attempt) follows, so resume and stop work. A driver that died during a read-only attempt leaves them after the last checkpoint, and resume refuses as for any unknown event there | a new event type is safe to skip up to the next checkpoint |
| `review_decided.decided_by` | dropped; identical routing | a new field is optional and lenient |
| `read_only/` evidence files | never opened | none |
| manifest `unattended`, `run.yaml` `unattended` | dropped: a resume by an older binary is attended and waits for a person; it never decides a gate | the conservative outcome |
| definition fields `outputs`, a playbook node's `output`, `read_only`, `human_review.unattended` | dropped: no projection, the answer text to the parent, no read-only check, no unattended decision | stated per feature |

An older binary that resumes an unattended run rewrites `run.yaml` without
the field. The manifest keeps the mode, so the next drive by this apb is
unattended again.

Corpus rows added by this part (rows D1 to D9 belong to the first part):

| Case | Writer, then reader | Expected |
|---|---|---|
| D10 | beta.1 and 0.25.0 journals | the new binary projects `run_outputs` without error |
| D11 | a playbook with `outputs`, `read_only`, `retry_delay` and `unattended` | both older binaries validate and run it with the fields dropped: the `read_only` script writes and succeeds, the gate waits for a person; the new binary validates it and fails the script `read_only_dirty`, its output missing `not_succeeded` |
| D12 | new: an unattended run decided at gate 1 (`decided_by`), with a read-only attempt (`read_only_checked`), its driver lost before gate 2 | both older binaries read it (skipping `read_only_checked`) and stop it; beta.1 resumes it and waits at gate 2 (`apb wait` exits 3); the new binary resumed afterwards is unattended again and fails `hitl_required` there |
| D13 | new: `read_only_checked` and `read_only_violated` followed by `node_finished` | both older binaries skip them with the unknown count shown and resume with the routing of the new binary |

## 7. Tests

Every check drives a shipped entry point: the `apb` binary, an HTTP route
or an MCP tool.

Read-only:

1. `apb validate` reports V137 to V140 on fixtures for every kind, a list
   node, `protect` and a publish source.
2. In the detected tier, a script that modifies, creates, deletes, changes
   the mode of a file, retargets a symlink, runs `git commit` or runs `git
   add` fails the node `read_only_dirty` after one attempt, with
   `max_retries: 3` unused; each mutation is its own case.
3. The evidence file names a changed `.env` and holds no byte of what was
   written, and no file under the run directory does.
4. A change to an ignored file passes.
5. Lowered limits give `detected_partial` with counts, shown by `apb runs`.
6. In the enforced tier, a write to the tree, to `.git`, to an ignored file
   and to the common directory of a linked worktree fails inside the
   sandbox, a write to the attempt's run paths succeeds and the status file
   publishes; a machine without `bwrap` skips with one printed line.
7. A detected-tier node never overlaps a sibling's attempt; a `full` one
   may run in a batch beside a writing sibling and passes.
8. A violation routes through `on_failure`, or stops the run with the node
   and the reason; a run output from it reads `not_succeeded`.
9. With `retry_delay`, a tree change made while the run was released is not
   blamed on the retry, and a retry that writes journals the violation with
   no deferral after it.

Unattended:

1. `apb run --unattended` at a plain review ends `failed` with the node and
   `hitl_required`, exits 1, and the `on_failure` target never starts.
2. `decide: approve` passes the gate with `decided_by: unattended`, twice
   for two visits.
3. `apb validate` refuses `decide` before an irreversible or secret node,
   accepts it behind a second gate, and refuses an option only bounded
   edges route.
4. A publish gate fails `hitl_required`; a question with a default is
   answered `unattended`, one without fails `hitl_required`.
5. A budget that runs out fails the run `budget_exhausted` with no
   `run_paused`, on the sequential, batch and list sites.
6. Host execution, a supervised mode and MCP `supervise: self` are refused
   at start by the CLI, MCP and HTTP, and no run directory exists after.
7. A child inherits the mode and fails its parent without the failure
   route.
8. The mode survives a waker, `apb resume --all` and a `run.yaml` with the
   field removed.
9. A deferred retry longer than the release threshold journals no
   `drive_released`, and a foreground `apb run --unattended` exits 0 after
   the retry.
10. `next_actions[].unattended` matches the table of 5.2 on MCP
    `run_status`, and the run page shows it.
11. `apb grade` does not count an unattended approval as consent.
12. Corpus rows D11 to D13 run in the `journal-corpus` CI job.

## 8. Not covered

Typed outputs and failing a run on a missing output; file results; read-only
list nodes; restoring a tree; unattended gates beyond `human_review`;
auto-approving a publish; an unattended dashboard start.
