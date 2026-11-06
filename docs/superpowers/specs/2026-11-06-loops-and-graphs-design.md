# Loops and graphs that check themselves

Design record for four changes that make agent work more trustworthy and
its graph less serial: templates render arrays and objects, read-only
reviewers share concurrent batches, a loop can stop when a round finds
nothing new, and a retry after a rejection is told what to repair.

## Why

Agent work is only as trustworthy as the checks around it, and only as
fast as the waits the graph forces. Reading apb against that idea found
four concrete gaps:

1. A template that reads an array field renders an empty string.
   `{{nodes.<id>.output.<field>}}` goes through
   `apb_core::schema::output_field_value`, which returns `None` for an
   array or an object. The bundled `apb-feature-delivery` asks reviewers
   and `collect` for `findings` as a JSON array and hands
   `{{nodes.collect.output.findings}}` to the reviewers and to `fix`. An
   agent that writes a real array hands `fix` no findings at all. The
   bundled scenarios pass only because they script the array as a string.
2. Read-only review panels run one node at a time. `is_batchable`
   (`crates/apb-engine/src/scheduler/node.rs`) keeps `protect` nodes and
   `read_only` nodes outside `isolation: full` out of every concurrent
   batch. The bundled reviewers use `protect: ["**"]`, so three reviewers
   run one after another everywhere, and `isolation: full` is refused on
   macOS.
3. An open-ended loop (bug hunting, research, review rounds) can stop only
   at its `max_traversals` cap or on the agent's own word. Nothing in apb
   counts whether a round found anything new.
4. A fresh-session retry after a rejected success is never told why it was
   rejected: the reason lives only in memory (`last_msg`), and the retry
   briefing quotes the agent's own summary. Nothing tells the retry to
   repair only what was rejected.

## Scope

1. Crates: `apb-core` (schema, new `loop_items.rs`, validator, templates),
   `apb-engine` (context, state, parallel, scheduler batch admission,
   pipeline admission, read-only check, attempt memory, event fields,
   diagnostics, the HTML report's edge label, run view), `apb-server` (ts
   contract), `web` (graph label and round-trip of the new condition).
2. Bundled content: `apb-feature-delivery`, `apb-code-review`, the
   `apb-delivery` skill and their scenarios.
3. Journal: two additive fields (`read_only_violated.concurrent_with`,
   `attempt_finished.rejected_for`). No new event type.
4. Behaviour change for release notes: a bundled reviewer that writes to a
   tracked file now fails at once, with no retry, and its change stays in
   the tree, where `protect` used to undo it and retry. In
   `apb-code-review` the report still runs and names the missing review;
   in `apb-feature-delivery` the review panel fails and a repair round
   starts.

Out of scope:

1. Lessons written by agents and shown to later runs as rules. The safe
   form already exists: a finding with `location: node:<id>` or
   `skill:<name>` and a `workaround` is a known problem, shown only with a
   `known_problems:` block, after recurrence or a person's confirmation,
   as facts to check. An agent-written rule shown as an instruction would
   be a persistent prompt-injection path.
2. A validator warning for an edge that carries nothing. Too many read
   channels (`nodes.*`, `run.context`, `run.retro`, `continue_session`,
   list sources, `evidence`, approvals, success checks) make it noisy, and
   an unconditional edge is often a deliberate cost gate.
3. One tree comparison around a whole batch. Batches are pipelined: a
   member is journaled and routed as soon as it ends, so a later
   batch-wide verdict would have to undo a routed success.
4. Batching `protect` nodes. Their restore undoes a sibling's write before
   the sibling's own check runs, so the writer escapes and an innocent
   node is blamed.

## D0. Array and object fields in templates

Add to `crates/apb-core/src/schema.rs`, next to `output_field_value`,
a sibling reader:

```rust
/// The text a `{{nodes.<id>.output.<field>}}` template renders: what
/// `output_field_value` returns, plus an array or an object as compact
/// JSON. Edge conditions keep `output_field_value`.
pub fn output_field_text(output: &str, field: &str) -> Option<String>
```

1. String, bool, number: same as `output_field_value`.
2. Array or object: `serde_json::to_string(value)` (compact, never pretty).
3. Null, absent field, output that is not a JSON object: `None` (renders
   empty, as today).

Use it only in `crates/apb-engine/src/context.rs` `resolve` for
`["nodes", id, "output" | "report", field]`. The existing clipping bound of
that path still applies. Do not change `output_field_value`: the
`output_field` edge condition keeps its documented non-match for arrays.

Cache keys of prompts that read such a field move once (empty became
text). That is a cache miss, not a wrong hit. State it in the release
notes. Docs: `docs/HOWTO-authoring.md` "Template variables", one paragraph
on what an array or object field renders.

Wrong implementations to avoid:

1. Changing `output_field_value` itself (silently changes routing of every
   `output_field` edge whose field is an array).
2. Pretty-printed JSON (bigger prompts, unstable whitespace).
3. Accepting nested paths `.output.a.b` (they stay V13 errors).

## D1. Read-only nodes in concurrent batches

### Write class

New file `crates/apb-engine/src/scheduler/batch_class.rs`, declared from
`scheduler.rs` next to the other `mod` lines:

```rust
pub(crate) enum WriteClass { Writer, Enforced, Detected }
pub(crate) fn write_class(playbook: &Playbook, node: &str) -> WriteClass
pub(crate) fn batch_admits(playbook: &Playbook, batch: &[String], candidate: &str) -> bool
```

1. `Writer`: a batchable node that is not `read_only`.
2. `Enforced`: `read_only: true` with `isolation: full` (the sandbox makes
   a write fail at the OS level).
3. `Detected`: `read_only: true` in any other tier (today's
   `detected_read_only` in `scheduler/node.rs`, moved into the new file).

`batch_admits` is false when the candidate is `Detected` and any member of
`batch` is `Writer`, or the candidate is `Writer` and any member is
`Detected`. `Enforced` mixes with both. `batch` is the whole logical batch,
finished members included.

`is_batchable` (`scheduler/node.rs`) drops `!detected_read_only(n)`;
`protect` nodes and list nodes stay non-batchable. Its doc comment says a
detected read-only member may now batch, but only with members that cannot
write.

### The two admission sites

1. Batch formation in `crates/apb-engine/src/scheduler.rs` ("Concurrent
   fast path"): `current` first, then each frontier node only if
   `is_batchable && batch_admits(&batch, n)`. The loop pushes as it goes,
   so each check sees the nodes admitted before it. A refused node stays
   in the frontier for a later iteration.
2. Mid-batch admission in `crates/apb-engine/src/scheduler/pipeline.rs`:
   the ready set is admitted one candidate at a time against `slots.batch`,
   pushing each admitted node before the next check. A single filter pass
   would admit a `Detected` and a `Writer` candidate together.

### The per-attempt check stays

Each detected read-only member keeps its own before and after tree state
(`scheduler/node/read_only.rs`, `Before::take` and `Before::finish`),
exactly as today. Since no member of its batch may write, a change it sees
was made by a co-member that ran at the same time (or by something outside
apb, which the check already cannot tell apart). Every member that saw the
change fails.

### Who ran beside whom

1. Type: `pub(crate) struct BatchIntervals(Mutex<...>)` in
   `scheduler/node/read_only.rs`: a monotonic tick counter and, per
   attempt, the node, its start tick and an optional end tick.
2. Carrier: `ExecOptions.batch_peers: Option<Arc<BatchIntervals>>`. Set by
   the batch member spawn in `scheduler.rs` and by mid-batch admission in
   `pipeline.rs`, one `Arc` per logical batch, `None` everywhere else. No
   global state.

3. Ticks: the start tick is taken immediately before `Before::take`; the
   end tick is set after `Before::finish` has taken its after state.
4. Overlap: an interval whose end is not set yet is open (end is
   infinity). Two intervals overlap when `a.start < b.end && b.start <
   a.end`. Self is excluded. Every overlapping member is listed, `Enforced`
   ones included.
5. `Before::finish` gets the overlapping node ids (sorted, deduplicated).
   With `batch_peers: None` the list is empty.

### Journal

No new event. One additive field on `EventPayload::ReadOnlyViolated` in
`crates/apb-engine/src/event.rs`:

```rust
/// Other members of a concurrent batch whose attempts overlapped this
/// attempt; the change cannot be attributed to one of them. Empty on the
/// sequential arm. Additive.
#[serde(default, skip_serializing_if = "Vec::is_empty")]
concurrent_with: Vec<String>,
```

The failure message keeps its `read_only_dirty` prefix:

```text
read_only_dirty: the tree changed while node `X` ran beside `Y`; the change cannot be attributed to one of them: <path> modified (and N more)
read_only_dirty: the tree changed while node `X` ran beside `Y`, `Z`; the change cannot be attributed to one of them: <path> modified (and N more)
```

With an empty list the message is exactly today's. The scenario boundary
(`scheduler/node/read_only.rs` `scripted`) keeps today's message and an
empty `concurrent_with`.

Retry: none, exactly as today (no retry, no fallback, no deferral).
Resume: nothing new; a crash mid-batch reaps attempts as now, and a re-run
takes a fresh before state.

### Surfaces

1. `crates/apb-engine/src/run_view.rs`: `ReadOnlyViolation` gets
   `#[serde(default, skip_serializing_if = "Vec::is_empty")]` with the ts
   optional attribute `concurrent_with: Vec<String>`, filled from the
   event. `ReadOnlyRow::line()` reads, with the list set,
   `; the tree changed while it ran beside Y, Z (read_only_dirty), N
   paths: ...` and, with it empty, exactly today's line. The CLI, MCP and
   `apb doctor --run` print that line.
2. TypeScript: `ReadOnlyViolation` and `ReadOnlyRow` are exported to
   `web/src/lib/api.gen.ts`; regenerate.
3. `web/src/lib/AttemptIsolationPanel.svelte`: when
   `v.concurrent_with?.length`, the heading reads `The tree changed while
   this node ran beside Y, Z; the change cannot be attributed to one of
   them` followed by the same code span; test in
   `AttemptIsolationPanel.test.ts`. `web/src/lib/journal.ts` appends `
   (ran beside Y, Z)` on the `read_only_violated` line. `readOnlyText` and
   `readOnlyBadge` describe only the tier and stay as they are.

The HTML report renders no read-only data; do not add a section there.

### Bundled playbooks

1. `bundled/playbooks/apb-feature-delivery/playbook.yaml`:
   `review_correctness` and `review_security` get `read_only: true` and
   lose `protect`. `review_tests` keeps `protect`: its prompt runs the
   project's checks, which write tracked files in many projects, and
   restore plus retry is right there. `intake`, `design`, `decompose`,
   `collect` and `report` keep `protect`.
2. `bundled/playbooks/apb-code-review/playbook.yaml`: `review_correctness`,
   `review_security` and `review_tests` get `read_only: true` and lose
   `protect`. `scope` and `native_checks` keep `protect`.
3. Each changed bundled playbook bumps its `version` once (patch) for the
   whole change. Follow the bundled-content tests for any digest sidecar.
4. One new scenario per playbook: a reviewer scripted as `read_only_dirty`
   (the scenario boundary exists) fails at once, with no retry. The run
   outcome follows each playbook's own routing: in `apb-feature-delivery`
   the failed reviewer lands in the `collect` join, the barrier fails, its
   fallback starts a repair round, and the run ends failed at `reconsider`
   only when that round cannot converge; in `apb-code-review` the
   reviewers deliver into a `join: all` on the `report` finish node, a
   failed delivery there is handled, the report composes and names the
   gap, so the scenario pins the node-level guarantee (one attempt, no
   retry, the node's `failed` status through the scenario expectation
   `status` (the status a node's last execution recorded, added for this),
   `read_only_dirty` in its output). Update `clean-panel` and `single-change-clean-panel` only if
   they assert order.

Docs: `docs/GUARDRAILS.md` (rows for `protect` and `read_only`),
`docs/ISOLATION.md` (read-only nodes), `docs/HOWTO-authoring.md` "Read-only
node (read_only)" and the `protect` sentence "A node with protect never
runs in a concurrent batch", which stays as it is.

Wrong implementations to avoid:

1. Only `is_batchable` changed: writer plus detected read-only batches
   form.
2. Admission rule at batch formation but not in `pipeline.rs`, or there as
   one filter pass.
3. A batch-wide comparison, or a second `node_finished` for a routed
   member.
4. Failing only the first member that noticed.
5. Admitting `protect` members as read-only.
6. Overlap computed over closed intervals only (the member that finishes
   first then lists nobody).
7. A failure message without the `read_only_dirty` prefix.

## D2. no_new_items: stop a loop when a round finds nothing new

### Schema

`crates/apb-core/src/schema.rs`, `EdgeCondition`:

```rust
/// Matches once the last `rounds` successful visits of `node` each output
/// only keys that an earlier successful visit of this run had already
/// output. Everything ever output counts as seen.
NoNewItems {
    node: String,
    field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    rounds: u32,
},
```

YAML: `{ type: no_new_items, node: hunt, field: findings, key: id, rounds: 2 }`.

### The one reading

`crates/apb-core/src/loop_items.rs` (new):

```rust
pub struct LoopItems {
    pub visits: usize,        // successful visits read
    pub streak: usize,        // trailing readable visits that added no new key
    pub seen: Vec<String>,    // every key, first-seen order, no duplicates
    pub new_in_last: usize,   // new keys in the latest visit
}
pub fn evaluate(outputs: &[String], field: &str, key: Option<&str>) -> LoopItems
```

`outputs` are the node's successful visits in journal order.

1. A visit's field is an array, or a string that parses as an array (the
   same tolerance as `apb_core::list::items::items_from_output`).
2. Without `key`: each element is a string or an integer; its text is the
   key. With `key`: each element is an object whose `key` value is a
   string or an integer.

3. Key rules: the item id rules of `apb_core::list::items` (`id_of` and
   `ITEM_ID_MAX_CHARS`, made `pub(crate)` and reused, not copied): not
   empty, at most `ITEM_ID_MAX_CHARS`, no backtick, no control or hidden
   character, no whitespace but a plain space. Exact comparison, no case
   folding. A key repeated within one visit is allowed and counts once.
4. A visit is readable when the output is a JSON object, the field is
   present and an array (or an array string), and every element yields a
   key. Anything else is unreadable.
5. A readable visit is dry when every one of its keys was already in
   `seen` before it (it adds no new key). `[]` is readable and dry, a
   first visit with `[]` included. A dry visit adds 1 to `streak`; a
   readable visit with a new key adds its new keys to `seen` and sets
   `streak` to 0.
6. An unreadable visit sets `streak` to 0 and adds nothing to `seen`.
7. The condition matches when `visits >= rounds` and `streak >= rounds`.

Only successful visits are passed in: failed, timed-out and cancelled ones
neither count nor break the streak.

### Fold, no event

`crates/apb-engine/src/state.rs`, `RunState`:
`pub success_outputs: BTreeMap<String, Vec<String>>`, appended in the
`NodeFinished` arm when `NodeStatus::from_label(status) ==
NodeStatus::Succeeded` (the label is `"succeeded"`, never `"success"`).
History is never reset: a `continue_from`, a revive, a supervisor retry or
a patch adds visits, and what a node output before stays seen. A cache hit
journals a successful `node_finished` and counts as a visit.
`node_finished.output` holds the whole output; the template path clips
separately, so the fold never sees a clipped array.

### Matching

`crates/apb-engine/src/parallel.rs`:

1. `edge_matches_at`: a new arm calls `loop_items::evaluate` over
   `state.success_outputs.get(node)` (empty slice when absent) and
   compares with `rounds`.
2. `condition_reads`: `NoNewItems { node, .. }` reads `node`.

Every exhaustive `match` on `EdgeCondition` handles the variant; the sites
that use `if let` or a catch-all are handled explicitly: the V10 and V40
reachability read, the V46 declared-field warning (for the condition field
and the seen template), the HTML report's `edge_label` (label `no new
items x2`, the number is `rounds`), `diagnostics/route.rs` (`kind_name`
returns `no_new_items`, `observed()` returns the visits, streak and
`new_in_last` figures; no top-level `RouteEvaluated` fields; diagnostics
never steer), and the web graph (`edgeLabel` and `exitCaption`, label `no
new items x2`).

`EdgeCondition` is not exported to TypeScript; the web side reads the YAML
as untyped objects. `playbookedit.ts`, `nodefields.ts` and
`NodePanel.svelte` must keep the condition with all its fields on save. No
edit form in this change. A save that drops the condition would make the
exit edge unconditional, which a vitest covers.

### The pattern authors write

Routing: unconditional edges fan out and pre-empt everything; conditional
edges are first match in order; fallback last. So:

```yaml
- { from: hunt, to: report, condition: { type: no_new_items, node: hunt, field: findings, key: id, rounds: 2 } }
- { from: hunt, to: hunt, condition: { type: node_status, node: hunt, equals: success }, max_traversals: 6 }
- { from: hunt, to: report, fallback: true }
```

The exit edge comes before the loop edge, and the loop edge is conditional.
V34 (an error) already rejects a source that has an unconditional
non-fallback edge next to a conditional one, in either order.

### The seen template

`{{nodes.<id>.seen.<field>}}`: the keys in `seen`, one per line,
first-seen order, at most 200 keys and 8 KiB. When more exist, the
earliest that fit are listed and a last line reads `(N more not shown)`.
Keys cannot hold a newline, so the format is unambiguous. On the first
visit the text is empty.

Every node prompt, judge state field and cache key is rendered through
`JudgeInputs`. The seen text goes there, so all of them agree:

1. `JudgeInputs` gets
   `#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]` with the
   ts optional attribute `pub seen: BTreeMap<String, String>`, keyed
   `"<node>.<field>"`, holding the rendered text. `JudgeInputs` is
   `deny_unknown_fields` and stored inside judge examples, so an empty map
   must not be written. `judge_eval/example.rs` `run_values` fills it with
   `seen_texts(&playbook, &state)`, each value passed through the run
   redactor like `outputs`. The judge sandbox editor offers no slot for a
   seen reference; it renders empty there, which is acceptable in this
   change. Regenerate `web/src/lib/api.gen.ts`.
2. One function `seen_texts(playbook, state) -> BTreeMap<String, String>`
   in a new `crates/apb-engine/src/loop_seen.rs`: for each `no_new_items`
   condition in the playbook, `evaluate` over `state.success_outputs` with
   that condition's `key`, rendered as above. When several conditions
   share node and field, use the first in edge order.

3. `JudgeInputs::from_state` and `render_node_prompt` get a
   `playbook: &Playbook` parameter; `from_state` fills `seen` through
   `seen_texts`. Update every call site; each has a playbook in scope.
4. `context::render` and `resolve` get a `seen: &BTreeMap<String, String>`
   parameter. `JudgeInputs::render` passes its map. The finish answer and
   a sub-playbook node's `instruction` render node text like any node
   prompt, so both pass `seen_texts(playbook, &state)`; every other direct
   caller passes `&BTreeMap::new()`.
5. `resolve` renders `["nodes", id, "seen", field]` from the map, empty
   when absent. `JudgeInputs::unfilled` labels an empty seen read `never
   ran` on a first round; that is acceptable.
6. Grammar: the template grammar accepts `["nodes", id, "seen", field]`;
   add it to the V13 known namespaces.

### Validator codes

| Code | Severity | Rule |
|---|---|---|
| V164 | error | malformed `no_new_items`: unknown `node`; `node` not an `agent_task` or `script`; `node` is a list node; `field` or `key` empty or holding `.`, whitespace or `}`; `rounds` outside 1 to 10 |
| V165 | error | `{{nodes.<id>.seen.<field>}}` with no `no_new_items` condition on that node and field, or two such conditions on them with different `key` |
| V166 | warning | the condition's `node` and the edge's source are not on one cycle. When they are the same node, it is on a cycle exactly when it is in a cyclic strongly connected component (more than one node, or a self-loop edge) |

Docs: `docs/HOWTO-authoring.md`, a section "Loops that stop when nothing
new turns up" under "Bounded loops", with the pattern above, the seen
template, the readable and dry rules, and the table.

Wrong implementations to avoid:

1. Counting attempts instead of successful `node_finished` visits.
2. Comparing the status with `"success"` (never matches).
3. Unreadable read as dry (an agent that forgets the field ends the hunt).
4. A visit called dry when all its keys are new (the rule backwards).
5. Comparing with the previous visit only (A, B, A looks new every round).
6. Matching before `rounds` visits exist.
7. Folding from `RunState.outputs` (latest only).

8. A new journal event.
9. The seen map passed at some render sites only (cache key and prompt
   drift).
10. Resetting history on `continue_from` or a patch.
11. The editor dropping the condition on save.

## D3. Rejection reason and repair scope in the retry briefing

### Journal

`crates/apb-engine/src/event.rs`, `EventPayload::AttemptFinished`:

```rust
/// Why apb rejected a report the agent called a success, scrubbed and at
/// most 500 characters. Set only for a non-fatal rejection. Additive.
#[serde(default, skip_serializing_if = "Option::is_none")]
rejected_for: Option<String>,
```

Rule: set `rejected_for` exactly when the attempt is journaled `failed`
because of a `Rejection` with `fatal == false` or a completion check's
`reject`. Never otherwise (timeouts, interruptions, quota, transient, auth,
and every fatal rejection). A git-guard rejection that restored everything
is non-fatal and so carries it; one that could not restore is fatal and
does not.

The four sites, all in `crates/apb-engine/src/scheduler/node.rs`:

1. the `success_check_rejection` arm on the report path;
2. the completion enforce on the report path;
3. the same two on the path that recovers a status-file verdict after the
   agent process died.

The `AttemptFinished` appends happen before `if fatal`, so the field there
is `(!fatal).then(...)`. Scrub, then bound:
`Scrubber::for_workspace(workdir, Vec::new())` with the node context's
`workdir`, then `chars().take(500).collect()`, never a byte slice.

### Briefing

`crates/apb-engine/src/scheduler/attempt_memory.rs`:

1. `AttemptRecall` gets `rejected_for: Option<String>`, read from the
   attempt's `attempt_finished`.
2. `ending` renders such an attempt as
   `reported success; apb rejected it: "<reason>"`, the reason whole (up to
   its 500 characters) and quoted with `serde_json::to_string`, instead of
   the agent's summary.
3. `rejections` keeps the seq of each `ListItemSettled` rejection.
4. A fixed paragraph is appended when the last attempt in scope has
   `rejected_for`, or when the latest list item rejection's seq is greater
   than the seq of the last `attempt_finished` in scope:

```rust
pub(crate) const REPAIR_SCOPE: &str = "Repair scope: change only what the rejection above names. Keep the parts of your earlier work it does not name; do not rework, reformat or refactor them, and make no unrelated edits. Write your full outputs again.";
```

It makes no claim about what the tree holds, so it is true for an isolated
attempt in a fresh directory too.

Not for transient, infrastructure, timeout or interrupted retries (the work
was not finished), and not for a loop visit after a success (the effect
scope resets; the prompt carries the scope there).

Cache key: untouched. The briefing is appended outside
`render_node_prompt`; do not move any of this into the template context.
Docs: `docs/HOWTO-authoring.md` "The retry briefing".

### Bundled content

1. `bundled/playbooks/apb-feature-delivery/playbook.yaml` `fix` prompt,
   after "Fix the confirmed ones; reject the others with a reason.": "Change
   only what a confirmed finding names; keep everything else of the
   delivered change as it is; no unrelated edits, refactors or formatting.
   A fix that needs a change outside a finding's location says so in
   evidence with the reason."
2. `bundled/skills/apb-delivery/references/stages.md`, the Repair row: the
   same rule in its own words.

3. Bump `bundled/skills/apb-delivery/SKILL.md` `metadata.version` (patch).
   The feature-delivery playbook `version` is bumped once for the whole
   change.

Wrong implementations to avoid:

1. The paragraph on every briefing.
2. The reason in the rendered prompt or template context (moves the cache
   key).
3. The reason taken from `rejected_output` or `summary` (the agent's
   words).
4. `rejected_for` on fatal endings.
5. Only the first two of the four sites patched.
6. A new event instead of the field.
7. An unscrubbed reason (a check script's stderr can carry a secret).

## Release notes

The change is not tagged here. User-visible changes to list when it ships:
the one-time cache miss of D0 for prompts that render an array or object
field; D1 reviewers run at once, and a bundled reviewer that writes a
tracked file now fails at once with no retry (in `apb-code-review` the
report names the gap; in `apb-feature-delivery` the panel fails and a
repair round starts); D2 the new condition and template;
D3 the retry briefing names the rejection.
