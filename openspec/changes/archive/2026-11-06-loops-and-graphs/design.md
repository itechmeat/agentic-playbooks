# Design: loops and graphs that check themselves

Read `proposal.md` first. Every decision below names its files. Line
numbers are from origin/main d8ff938f and were checked; find the code by
the names if they moved.

## D0. Array and object fields in templates

Add to `crates/apb-core/src/schema.rs`, next to `output_field_value`:

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
`["nodes", id, "output" | "report", field]` (it calls `output_field_value`
at line 753). The existing clipping bound of that path still applies. Do not
change `output_field_value`: the `output_field` edge condition keeps its
documented non-match for arrays.

Cache keys of prompts that read such a field move once (empty became text).
That is a cache miss, not a wrong hit. State it in the release notes.

Docs: `docs/HOWTO-authoring.md` "Template variables": one paragraph on what
an array or object field renders.

Wrong implementations to avoid:

1. Changing `output_field_value` itself (silently changes routing of every
   `output_field` edge whose field is an array).
2. Pretty-printed JSON (bigger prompts, unstable whitespace).
3. Accepting nested paths `.output.a.b` (they stay V13 errors).

## D1. Read-only nodes in concurrent batches

### Write class

New file `crates/apb-engine/src/scheduler/batch_class.rs` (keep it out of
`scheduler/node.rs`, which is already over 4600 lines), declared from
`scheduler.rs` next to the other `mod` lines:

```rust
pub(crate) enum WriteClass { Writer, Enforced, Detected }
pub(crate) fn write_class(playbook: &Playbook, node: &str) -> WriteClass
pub(crate) fn batch_admits(playbook: &Playbook, batch: &[String], candidate: &str) -> bool
```

1. `Writer`: a batchable node that is not `read_only`.
2. `Enforced`: `read_only: true` with `isolation: full` (the sandbox makes a
   write fail at the OS level).
3. `Detected`: `read_only: true` in any other tier (today's
   `detected_read_only` in `scheduler/node.rs`; move that helper into the
   new file and use it from there).

`batch_admits` is false when the candidate is `Detected` and any member of
`batch` is `Writer`, or the candidate is `Writer` and any member is
`Detected`. `Enforced` mixes with both. `batch` is the whole logical batch,
finished members included.

`is_batchable` (`scheduler/node.rs`, line 4627) drops
`!detected_read_only(n)`. `protect` nodes and list nodes stay non-batchable.
Update its doc comment: a detected read-only member may now batch, but only
with members that cannot write.

### The two admission sites

1. Batch formation in `crates/apb-engine/src/scheduler.rs` (line 1397,
   "Concurrent fast path"): `current` first, then each frontier node only if
   `is_batchable && batch_admits(&batch, n)`. The loop pushes as it goes, so
   each check sees the nodes admitted before it. A refused node stays in the
   frontier for a later iteration. If `current` itself is `Detected` and the
   batch would hold a `Writer`, the writer is the one left out.
2. Mid-batch admission in `crates/apb-engine/src/scheduler/pipeline.rs`
   (lines 112 to 122). Today it builds `ready` in one `.filter()` pass and
   then pushes all of it into `slots.batch`. Replace that with a loop that
   checks one candidate at a time against `slots.batch` and pushes each
   admitted node into `slots.batch` before checking the next. A single
   filter pass would admit a `Detected` and a `Writer` candidate together.

### The per-attempt check stays

Each detected read-only member keeps its own before and after tree state
(`crates/apb-engine/src/scheduler/node/read_only.rs`, `Before::take` and
`Before::finish` at line 184), exactly as today. Since no member of its
batch may write, a change it sees was made by a co-member that ran at the
same time (or by something outside apb, which the check already cannot tell
apart). Every member that saw the change fails.

### Who ran beside whom

1. Type: `pub(crate) struct BatchIntervals(Mutex<...>)` in
   `scheduler/node/read_only.rs`: a monotonic tick counter and, per attempt,
   the node, its start tick and an optional end tick.
2. Carrier: `ExecOptions` (`crates/apb-engine/src/scheduler/outcome.rs`,
   line 84, `#[derive(Default)]`) gets
   `pub(crate) batch_peers: Option<Arc<BatchIntervals>>`. It is set by the
   batch member spawn in `scheduler.rs` and by mid-batch admission in
   `pipeline.rs`, with one `Arc` per logical batch, and is `None` everywhere
   else. No global state.
3. Ticks: the start tick is taken immediately before `Before::take`
   (`node.rs` lines 1754 in the agent attempt loop and 3525 in the script
   arm); the end tick is set after `Before::finish` has taken its after
   state (`node.rs` lines 2355 and 3539).
4. Overlap: an interval whose end is not set yet is open (end is infinity).
   Two intervals overlap when `a.start < b.end && b.start < a.end`. Self is
   excluded. Every overlapping member is listed, `Enforced` ones included.
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
(`scheduler/node/read_only.rs` `scripted`, line 102, message at line 134)
keeps today's message and an empty `concurrent_with`.

Retry: none, exactly as today (no retry, no fallback, no deferral). Resume:
nothing new; a crash mid-batch reaps attempts as now, and a re-run takes a
fresh before state.

### Surfaces

The read-only rows reach people through this chain; change each:

1. `crates/apb-engine/src/run_view.rs`: `ReadOnlyViolation` gets
   `#[serde(default, skip_serializing_if = "Vec::is_empty")]`
   `#[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]`
   `concurrent_with: Vec<String>`, filled from the event (line 265).
   `ReadOnlyRow::line()` (lines 220 to 224) today writes
   `; changed the tree (read_only_dirty), N paths: ...`. With
   `concurrent_with` set that segment reads
   `; the tree changed while it ran beside Y, Z (read_only_dirty), N paths: ...`
   and the rest of the line is unchanged; with it empty the line is exactly
   today's. The CLI, MCP and `apb doctor --run`
   (`run_doctor/isolation.rs` line 69) print that line.
2. TypeScript: `ReadOnlyViolation` and `ReadOnlyRow` are exported
   (`crates/apb-server/src/ts_contract.rs` lines 126 and 127) to
   `web/src/lib/api.gen.ts`; regenerate.
3. `web/src/lib/AttemptIsolationPanel.svelte` (lines 52 to 63, today "The
   node changed the tree" with the `read_only_dirty` code span): when
   `v.concurrent_with?.length`, the heading reads `The tree changed while
   this node ran beside Y, Z; the change cannot be attributed to one of
   them` followed by the same code span; test in
   `AttemptIsolationPanel.test.ts`. `web/src/lib/journal.ts` (line 128, the
   `read_only_violated` line) appends ` (ran beside Y, Z)`. `readOnlyText`
   and `readOnlyBadge` in `attemptisolation.ts` describe only the tier and
   stay as they are.

The HTML report renders no read-only data; do not add a section there.

### Bundled playbooks

1. `bundled/playbooks/apb-feature-delivery/playbook.yaml`:
   `review_correctness` and `review_security` get `read_only: true` and lose
   `protect` (keeping both triggers V139 and keeps them out of batches).
   `review_tests` keeps `protect`: its prompt runs the project's checks,
   which write tracked files in many projects, and restore plus retry is
   right there. `intake`, `design`, `decompose`, `collect` and `report` keep
   `protect`.
2. `bundled/playbooks/apb-code-review/playbook.yaml`: `review_correctness`,
   `review_security` and `review_tests` get `read_only: true` and lose
   `protect` (their prompts forbid commands that change the tree).
   `scope` and `native_checks` keep `protect`.
3. Each changed bundled playbook bumps its `version` once (patch) for the
   whole change. Follow the bundled-content tests for any digest sidecar.
4. One new scenario per playbook in its `evals/scenarios/`: a reviewer
   scripted as `read_only_dirty` (the scenario boundary exists:
   `scheduler/node/read_only.rs` `scripted`, line 102) fails at once, with
   no retry; each scenario asserts that node's `failed` status (through
   the scenario expectation `status`, added for this: the status a node's
   last execution recorded) and its `read_only_dirty` output. The run outcome follows each playbook's own
   routing. In `apb-code-review` the reviewers deliver into a `join: all`
   on the `report` finish node, so the report still runs, names the gap,
   and the run succeeds. In `apb-feature-delivery` the panel's `collect`
   join fails, its fallback starts a repair round, and the run ends failed
   at `reconsider` only when that round cannot converge.
   Update `clean-panel` and `single-change-clean-panel` only if they assert
   order.

Docs: `docs/GUARDRAILS.md` (rows for `protect` and `read_only`),
`docs/ISOLATION.md` (read-only nodes), `docs/HOWTO-authoring.md` "Read-only
node (read_only)" and the `protect` sentence "A node with protect never runs
in a concurrent batch".

Wrong implementations to avoid:

1. Only `is_batchable` changed: writer plus detected read-only batches form.
2. Admission rule at batch formation but not in `pipeline.rs`, or there as
   one filter pass.
3. A batch-wide comparison, or a second `node_finished` for a routed member.
4. Failing only the first member that noticed.
5. Admitting `protect` members as read-only.
6. Overlap computed over closed intervals only (the member that finishes
   first then lists nobody).
7. A failure message without the `read_only_dirty` prefix.

## D2. `no_new_items`: stop a loop when a round finds nothing new

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

### The one reading: `crates/apb-core/src/loop_items.rs` (new)

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
   key. With `key`: each element is an object whose `key` value is a string
   or an integer.
3. Key rules: the item id rules of `apb_core::list::items` (`id_of` and
   `ITEM_ID_MAX_CHARS`; make `id_of` `pub(crate)`, do not copy it): not
   empty, at most `ITEM_ID_MAX_CHARS`, no backtick, no control or hidden
   character, no whitespace but a plain space. Exact comparison, no case
   folding. Call `id_of(value, index)` (it returns
   `Result<String, ItemsError>`) on the element itself, or on its `key`
   value; an error makes the visit unreadable. Do not call
   `items_from_output` or `items_from_values`: they require objects, take a
   maximum and refuse duplicates. Only copy their reading of an array or an
   array held in a string. A key repeated within one visit is allowed and
   counts once.
4. A visit is readable when the output is a JSON object, the field is
   present and an array (or an array string), and every element yields a
   key. Anything else is unreadable.
5. A readable visit is dry when every one of its keys was already in
   `seen` before it (it adds no new key). `[]` is readable and dry, a first
   visit with `[]` included. A dry visit adds 1 to `streak`; a readable
   visit with a new key adds its new keys to `seen` and sets `streak` to 0.
6. An unreadable visit sets `streak` to 0 and adds nothing to `seen`.
7. The condition matches when `visits >= rounds` and `streak >= rounds`.

Only successful visits are passed in: failed, timed-out and cancelled ones
neither count nor break the streak.

### Fold, no event

`crates/apb-engine/src/state.rs`, `RunState`:
`pub success_outputs: BTreeMap<String, Vec<String>>`, appended in the
`NodeFinished` arm (lines 588 to 601) when
`NodeStatus::from_label(status) == NodeStatus::Succeeded`. The label is
`"succeeded"`, never `"success"` (that is the `StatusEq` spelling of edge
conditions). History is never reset: a `continue_from`, a revive, a
supervisor retry or a patch adds visits, and what a node output before stays
seen. A cache hit journals a successful `node_finished` and counts as a
visit.

Confirm in the code that `node_finished.output` holds the whole output (the
template path clips separately). If the journal copy can be clipped, a
clipped array is unreadable and therefore not dry: safe, but say so in the
docs.

### Matching

`crates/apb-engine/src/parallel.rs`:

1. `edge_matches_at` (line 67): a new arm calls `loop_items::evaluate` over
   `state.success_outputs.get(node)` (empty slice when absent) and compares
   with `rounds`.
2. `condition_reads` (line 124): `NoNewItems { node, .. }` reads `node`.

Every exhaustive `match` on `EdgeCondition` must handle the variant; the
compiler lists them (in `apb-engine` at least `parallel.rs` 68 and 124 and
`diagnostics/route.rs` 77 and 95). These sites use `if let` or `_ =>` and
the compiler will not report them; handle each explicitly:

1. `crates/apb-core/src/validate/graph.rs` line 232 (V10 and V40: a
   condition's node must exist and reach the source): add
   `Some(EdgeCondition::NoNewItems { node, .. }) => Some(node)`.
2. `crates/apb-core/src/validate/node_io.rs` line 138 (V46: an
   `output_field` field must be a declared output): give
   `no_new_items.field` and the seen template the same declared-field
   warning through the same `report(...)`.
3. `crates/apb-engine/src/report_html/graph.rs` line 508 (`edge_label`,
   `_ => None`): an explicit arm with the label `no new items x2` (the
   number is `rounds`).
4. `crates/apb-engine/src/diagnostics/route.rs`: `kind_name` returns
   `"no_new_items"`, and `observed()` returns
   `json!({"visits": .., "streak": .., "new_in_last": ..})` for this
   variant. Do not add top-level fields to `RouteEvaluated`. Diagnostics
   never steer.
5. `web/src/lib/graph.ts`: both `edgeLabel` (line 205) and `exitCaption`
   (line 291) get a case for `no_new_items` (`no new items x2`).

`EdgeCondition` is not exported to TypeScript; the web side reads the YAML
as untyped objects. `playbookedit.ts`, `nodefields.ts` and
`NodePanel.svelte` must keep the condition with all its fields on save. No
edit form in this change. A save that drops the condition would make the
exit edge unconditional: cover it with a vitest.

### The pattern authors write

Routing: unconditional edges fan out and pre-empt everything; conditional
edges are first match in order; fallback last. So:

```yaml
- { from: hunt, to: report, condition: { type: no_new_items, node: hunt, field: findings, key: id, rounds: 2 } }
- { from: hunt, to: hunt, condition: { type: node_status, node: hunt, equals: success }, max_traversals: 6 }
- { from: hunt, to: report, fallback: true }
```

The exit edge comes before the loop edge, and the loop edge is conditional.
V34 (an error, `validate/graph.rs` line 588) already rejects a source that
has an unconditional non-fallback edge next to a conditional one, in either
order; add a test that it fires for this variant.

### The seen template

`{{nodes.<id>.seen.<field>}}`: the keys in `seen`, one per line, first-seen
order, at most 200 keys and 8 KiB. When more exist, the earliest that fit
are listed and a last line reads `(N more not shown)`. Keys cannot hold a
newline, so the format is unambiguous. On the first visit the text is
empty.

Every node prompt, judge state field and cache key is rendered through
`JudgeInputs` (`crates/apb-engine/src/judge_eval/inputs.rs`: "a node
prompt, a judge state field and a cache key render alike everywhere"). The
seen text goes there, so all of them agree:

1. `JudgeInputs` gets
   `#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]`
   `#[cfg_attr(feature = "ts", ts(as = "Option<BTreeMap<String, String>>", optional))]`
   `pub seen: BTreeMap<String, String>`, keyed `"<node>.<field>"`, holding
   the rendered text (the same pattern as `RenderedField.unfilled`).
   `JudgeInputs` is `deny_unknown_fields` and stored inside judge examples
   (`judge_eval/example.rs` line 92), so an empty map must not be written.
   `judge_eval/example.rs` `run_values` (line 290, "fill from a run") fills
   it with `seen_texts(&playbook, &state)`, each value passed through the
   run redactor like `outputs`. Every other struct literal (`sandbox.rs`,
   `judge_eval/mod.rs` tests, `example.rs` line 396) uses
   `..Default::default()` or an empty map. The judge sandbox editor
   (`web/src/lib/judgefields.ts` `slotKey`) offers no slot for a seen
   reference; it renders empty there, which is acceptable in this change.
   Regenerate `web/src/lib/api.gen.ts`.
2. One function `seen_texts(playbook: &Playbook, state: &RunState) ->
   BTreeMap<String, String>` in a new `crates/apb-engine/src/loop_seen.rs`:
   for each `no_new_items` condition in the playbook, `evaluate` over
   `state.success_outputs` with that condition's `key`, rendered as above.
   When several conditions share node and field, use the first in edge
   order.
3. `JudgeInputs::from_state` and `render_node_prompt`
   (`scheduler/node.rs` line 24) get a `playbook: &Playbook` parameter;
   `from_state` fills `seen` through `seen_texts`. Update every call site;
   each has a playbook in scope. Known sites of `render_node_prompt`:
   `scheduler/cache.rs` 142, 585, 621; `scheduler/node.rs` 725, 839, 892,
   3450; `scheduler/resume.rs` 336; `scheduler/list/mod.rs` 63;
   `scheduler/publish.rs` 202, 309; `scheduler/worktree.rs` 137;
   `scheduler.rs` 1109; and the provenance tracker. Of `from_state`:
   `scheduler/judge.rs` 74, `scheduler/node.rs` 32 and 921. Let the compiler
   find the rest.
4. `context::render` (`context.rs` line 594) and `resolve` get a
   `seen: &BTreeMap<String, String>` parameter. `JudgeInputs::render` passes
   its map. The finish answer (`execute_finish_answer`) and a sub-playbook
   node's `instruction` (`run_playbook_node`) render node text like any node
   prompt, so both pass `crate::loop_seen::seen_texts(playbook, &state)`;
   a seen template there must not render empty. Every other direct caller
   passes `&BTreeMap::new()`: `baseline/read.rs` 112,
   `judge_eval/inputs.rs` 138, `scheduler/prepare.rs` 98,
   `scheduler/worktree.rs` 85, and the tests in `context_test.rs` and
   `run_retro_test.rs`.
5. `resolve` renders `["nodes", id, "seen", field]` from the map, empty when
   absent. `JudgeInputs::unfilled` labels an empty seen read `never ran` on
   a first round; that is acceptable.
6. Grammar: `crates/apb-core/src/validate/templates.rs` accepts
   `["nodes", id, "seen", field]`; add it to `V13_KNOWN_NAMESPACES`.

### Validator codes

| Code | Severity | Rule |
|---|---|---|
| V164 | error | malformed `no_new_items`: unknown `node`; `node` not an `agent_task` or `script`; `node` is a list node; `field` or `key` empty or holding `.`, whitespace or `}`; `rounds` outside 1 to 10 |
| V165 | error | `{{nodes.<id>.seen.<field>}}` with no `no_new_items` condition on that node and field, or two such conditions on them with different `key` |
| V166 | warning | the condition's `node` and the edge's source are not on one cycle. When they are the same node, it is on a cycle exactly when it is in a cyclic strongly connected component (more than one node, or a self-loop edge); use the SCC helper of `apb_core::graphutil` as `validate/graph.rs` line 280 does |

Verify V164 is still free before use:
`git grep -ohE '"V1[0-9][0-9]"' crates | sort -u | tail -3`. If taken, use the
next free codes and update this file. Put the rules in a new
`crates/apb-core/src/validate/loop_items.rs`, declared in `validate/mod.rs`,
and call its check from `validate()` next to line 383 with a
`// V164-V166` comment, the way `edges::check_one_of_order` is called with
`// V123, V124`. There is no other code registry.

Docs: `docs/HOWTO-authoring.md`, a section "Loops that stop when nothing new
turns up" under "Bounded loops" (line 3025), with the pattern above, the
seen template, the readable and dry rules, and a three-row table (V164 to
V166) in the format of the V160 to V162 table at line 211.

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
is non-fatal (`sandbox/git_guard.rs` line 249: `self.fatal |= !restored`)
and so carries it; one that could not restore is fatal and does not.

The four sites, all in `crates/apb-engine/src/scheduler/node.rs`:

1. the `success_check_rejection` arm on the report path (line 2741);
2. the completion enforce on the report path (line 2801);
3. the same two on the path that recovers a status-file verdict after the
   agent process died (lines 3025 and 3072).

The `AttemptFinished` appends at lines 2751 and 3082 happen before
`if fatal`, so the field there is `(!fatal).then(...)`.

Scrub, then bound: `Scrubber::for_workspace(workdir, Vec::new())` with
`workdir` from the node context, as at line 3539; then
`chars().take(500).collect()`, never a byte slice.

### Briefing

`crates/apb-engine/src/scheduler/attempt_memory.rs`:

1. `AttemptRecall` gets `rejected_for: Option<String>`, read from the
   attempt's `attempt_finished`.
2. `ending` renders such an attempt as
   `reported success; apb rejected it: "<reason>"`, the reason whole (up to
   its 500 characters) and quoted with `serde_json::to_string`, instead of
   the agent's summary.
3. `rejections` keeps the seq of each `ListItemSettled` rejection
   (`Vec<(u64, u32, String)>` or a small struct).
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

Docs: `docs/HOWTO-authoring.md` "The retry briefing" (line 2753).

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
   change (section 2 does it; section 7 does not bump again).

Wrong implementations to avoid:

1. The paragraph on every briefing.
2. The reason in the rendered prompt or template context (moves the cache
   key).
3. The reason taken from `rejected_output` or `summary` (the agent's words).
4. `rejected_for` on fatal endings.
5. Only the first two of the four sites patched.
6. A new event instead of the field.
7. An unscrubbed reason (a check script's stderr can carry a secret).

## Release notes

The change is not tagged here. List the user-visible changes in the final
report: D0 cache miss once; D1 reviewers run at once, and a bundled
reviewer that writes a tracked file now fails at once with no retry (in
`apb-code-review` the report names the gap; in `apb-feature-delivery` the
panel fails and a repair round starts); D2 the new
condition and template; D3 the retry briefing names the rejection.
