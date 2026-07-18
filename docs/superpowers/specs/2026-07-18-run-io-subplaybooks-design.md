# Run input, finish answer, and sub-playbooks - design

Date: 2026-07-18
Status: implemented, owner-review fixes applied

## Goal

Three connected features that together make a playbook callable like a
function:

- **Part A - run input.** The Start node carries an editable "input prompt"
  for the run: the existing `instruction` gets a home in the editor, an
  autosaved draft that does not version the playbook, and an immutable copy
  in every run for history.
- **Part B - finish answer.** The Finish node can carry a prompt and a
  profile; when it does, an agent composes the final answer of the run from
  the run's accumulated context. That answer is the run's result, visible on
  the dashboard and over MCP.
- **Part C - sub-playbooks.** A new node kind `playbook` runs another
  playbook as a full child run: the node's rendered instruction becomes the
  child's input (Part A), the child's finish answer (Part B) becomes the
  node's output.

Parts are implemented in order A, then B, then C; each later part consumes
the previous one. One PR, commits may land per part.

## Non-goals (out of scope for this iteration)

- Structured parameter passing to a child playbook (only the instruction
  text travels; `params` of the child keep their defaults).
- Fan-out: one `playbook` node spawning N parallel children.
- Supervision inheritance: a supervised parent does not auto-supervise its
  children.
- Draft storage for arbitrary `params` values (only the instruction draft).

## Part A - run input prompt on the Start node

### What exists

`RunConfig.instruction: Option<String>` already flows into `render()` for
every node's prompt template, and `playbook_run` (MCP) and the CLI accept it
per run. Missing pieces: an editor surface, a persistent draft, and a
visible copy in the run.

### Draft storage

- One file per playbook, outside the versioned content:
  `<registry>/playbooks/<id>/meta/instruction-draft.md`. The `meta`
  directory already exists as a non-version sibling (the registry's version
  listing excludes `layouts` and `meta`), so the draft can never collide
  with a version directory.
- The draft is NOT part of the playbook digest. Editing it creates no new
  version and does not touch trust. It is plain text, written atomically via
  `apb_core::fsutil`.
- Both project and global registries support the draft at the same relative
  path.

### Editor and autosave

- The Start node form in the web editor gets a textarea "Input prompt".
  Typing autosaves the draft (debounced, roughly 500 ms after the last
  keystroke) via a dedicated endpoint:
  - `GET /api/workspaces/:ws/playbooks/:id/input-draft` returns
    `{ "instruction": string | null }`.
  - `PUT` with the same shape stores it (empty string clears the draft file).
- The draft endpoint bypasses the playbook update flow entirely: no version
  bump, no digest change, no freeze interaction (a frozen playbook still
  accepts draft edits - the draft is run input, not definition content).

### Run start semantics

- Precedence: an explicitly passed `instruction` (MCP `playbook_run`
  argument, CLI flag, or a parent playbook node in Part C) wins; otherwise
  the current draft content is read at start time; otherwise `None`.
- The chosen value is snapshotted into the run's persisted `RunConfig`
  exactly as today - the run directory copy is already immutable in
  practice; nothing rereads the draft after start.
- The run view (dashboard) shows the snapshotted instruction read-only when
  selecting the Start node, labeled as the run's input. The runs API detail
  response includes `instruction` so the UI needs no second fetch.

### Not changing

- Template semantics of `instruction` in prompts (whatever placeholder
  behavior exists today stays as is).
- `playbook_run` argument shape (the argument already exists).

## Part B - the Finish node composes the run answer

### Schema

`NodeKind::Finish` gains two optional fields, additive to schema 2:

```yaml
- id: finish
  type: finish
  outcome: success
  prompt: "Summarize what this run did and produce the final deliverable summary."
  profile: writer          # optional; falls back to defaults.profile
```

- `prompt: Option<String>`, `profile: Option<QualifiedProfileRef>`, both
  `#[serde(default)]`. Existing playbooks parse unchanged.
- No `max_retries`, `timeout_seconds`, `success_check`, `isolation`,
  `workdir` on Finish: the reduced execution mode below makes them
  unnecessary; defaults apply where the engine needs a value.

### Execution

- Finish without `prompt` behaves exactly as today: instant, free, empty
  output.
- Finish with `prompt` executes like a reduced `agent_task`:
  - The prompt is rendered with the full standard context (params,
    instruction, node outputs, reviews, hooks, compacted context) - the
    "attach the whole run context" requirement is the existing render
    context, no new mechanism.
  - The profile binds the executor chain and SOUL as usual (resolution,
    manifest snapshot, trust - identical to `agent_task`), but the
    profile's skills are NOT delivered, and there is no success check and
    no isolation. Timeout and retries use engine defaults
    (`defaults.max_retries`, default task timeout).
  - The agent's output text is the node output (enters `state.outputs` like
    any node output).
- The run answer is defined as: the output of the succeeded Finish node.
  It is derived by fold (no new storage), exposed as:
  - `run_status` (MCP): new `answer: string | null` key.
  - `run_report` (MCP): included in the report body.
  - Server run detail response: `answer: string | null`; the run view shows
    it in the header area when present.
  - `RunSummary` (runs list): NOT included (potentially large; the list
    stays lean).

### Policy, manifest, progress

- `policy::collect_profile_refs` includes the Finish profile when `prompt`
  is set, so the trust gate and the manifest snapshot cover it - same
  anti-TOCTOU pass, no second resolution.
- `expected_duration` defaults: a Finish node with a `prompt` defaults to
  the agent-task default (120 s) instead of 0 s; validator warning V19
  (missing estimate) extends to such nodes. A Finish without `prompt` stays
  at 0 s.

### Validation

- V21 (error): `profile` set on a Finish node without `prompt` (a binding
  that can never execute is an authoring mistake).
- Profile resolvability stays a gate/adopt-report concern, as for
  `agent_task` (the offline validator does not resolve profiles).

## Part C - sub-playbook node

### Schema

New node kind, schema 2 additive:

```yaml
- id: translate_book
  type: playbook
  playbook: book-translation      # or { id: book-translation, scope: global }
  instruction: "Translate the plan from {{outputs.plan}} chapter by chapter."
  expected_duration: 2h           # optional, like any node
```

- `playbook: QualifiedPlaybookRef` - mirrors `QualifiedProfileRef`: a bare
  string means `scope: auto` (parent's origin registry first, then global);
  the object form pins the scope. The child is an ordinary playbook - any
  playbook from the list can be a child; nothing marks it as "embeddable".
- `instruction: Option<String>` - a template rendered with the parent run's
  context; the result becomes the child run's `instruction` (Part A
  precedence: an explicit value from the parent wins over the child's
  draft). Absent means the child falls back to its own draft.
- The child always runs its `current` version as resolved AT GATE TIME and
  pinned in the permit (below); there is no per-node version pin in this
  iteration.

### Trust and policy (anti-TOCTOU, recursive)

- The parent's policy gate (`policy::check_run`) walks the sub-playbook
  references recursively in the same single pass: for every child it
  resolves the exact version, computes the playbook digest, and collects
  the child's profile bundle map (including the child's own Finish profile
  and nested `playbook` nodes).
- `RunPermit` is extended with the verified child set: for each child,
  `(id, scope, version, playbook_digest, profile bundle map)`. The engine
  receives the permit verbatim; when a `playbook` node executes, the child
  run is started against the pinned version and the engine rejects any
  drift (digest or bundle mismatch), exactly like the existing
  `expected_*` checks.
- Effects: the effective effects of the parent are the union of its own and
  all pinned children's (recursively). The user consents to the whole tree
  once, at parent start.
- Trust: an untrusted child blocks the parent gate the same way an
  untrusted profile bundle does.

### Recursion and depth

- Cycle check at gate time (and in `playbook_adopt_report`): the reference
  graph of pinned playbooks must be acyclic; A -> B -> A is a gate error
  naming the cycle. The offline validator cannot see other playbooks, so
  this is a gate/advisory concern, not a V-code.
- Runtime depth limit of 5 nested runs as a defense-in-depth backstop
  (named constant); exceeding it fails the node.

### Execution

- Executing a `playbook` node starts a full child run: own `run_id`, own
  run directory, events log, manifest - a completely ordinary run. The
  child's persisted `RunConfig` gains `parent_run: Option<String>`
  (`#[serde(default)]`) carrying the parent run id; the parent appends a
  new event `ChildRunStarted { node_id, run_id }` (fields
  `#[serde(default)]`) before driving the child.
- The child runs in-process and synchronously from the parent node's point
  of view: the node completes when the child run reaches a terminal state.
  Child succeeded -> node Succeeded with the child's run answer (Part B) as
  the node output (empty string when the child's Finish has no prompt).
  Child failed or aborted -> node Failed with a diagnostic naming the child
  run id.
- Retries on the node re-run the child as a NEW child run (a fresh
  `ChildRunStarted` event); a failed child run stays on disk for forensics.
- Resume: if the parent is resumed while its latest `ChildRunStarted` child
  is not terminal, the node resumes THAT child run instead of starting a
  new one (the event log is the source of truth).
- Abort: aborting the parent aborts its running children (walk
  `ChildRunStarted` events for non-terminal children); a child can still be
  aborted individually, which the parent sees as node failure.
- Waiting states propagate naturally: a child sitting on `human_review`
  keeps the parent node running; the child's own run view is where the
  decision is made.

### Progress and dashboard

- The child is a normal run on the dashboard with its own progress bar. The
  runs list groups or indents children under their parent (`parent_run` in
  the summary), and the parent run view links to the child run from the
  `playbook` node.
- Parent percent: at read time (`progress::from_run_dir` level, not in the
  pure fold), a RUNNING `playbook` node contributes fractional credit
  `child_percent * expected_seconds(node)` to the parent summary. The pure
  fold stays untouched; the enrichment lives beside the existing
  run-dir-loading helper. Terminal nodes need no enrichment (they are
  counted by the fold as usual).
- `expected_duration` default for a `playbook` node: the agent-task default
  (120 s). Summing the child's own estimates would need registry access from
  the schema layer, so it is deliberately out; V19 nudges authors to set an
  explicit estimate instead.

### Surfaces

- Web editor: node form for the `playbook` kind with a dropdown of
  playbooks (project + global) and an instruction textarea.
- MCP: `run_status` of the parent lists child runs
  (`children: [{ node_id, run_id, status }]`); `playbook_howto` documents
  the node kind.
- Validation: V22 (error): `playbook` node with an empty/invalid reference.
  Resolvability of the reference is a gate/adopt concern.

## Testing

- Part A: registry draft read/write unit tests; server endpoint tests (GET
  empty, PUT then GET, clear); run-start precedence test (explicit beats
  draft beats none) and snapshot immutability; web test for the autosave
  debounce state.
- Part B: schema parse tests (finish with/without prompt); execution test
  with a stub adapter producing the answer; fold test that `answer` is the
  finish output; V21 test; MCP `run_status`/server detail shape tests;
  progress default test (finish-with-prompt = 120 s).
- Part C: gate tests (recursive permit, cycle error, untrusted child
  blocks); execution tests with a trivial child (success, failure,
  retry-new-child, resume-reattach, abort-propagation); output mapping test
  (child answer -> node output); progress enrichment test; V22 test; runs
  list parent/child grouping test.

## Known limitations (accepted at review, tracked for follow-up)

- Cross-workspace `execute_plan` runs carry no child pins: the plan token
  holds only the parent digest and profile bundles, so per-child bundle-trust
  acknowledgment and child drift-pinning apply only to the local
  `playbook_run` path. The whole-tree effects union still reaches the consent
  surface via preflight, and the parent digest stays trust-gated.
- `run_report` progress uses the pure fold (no child credit) while
  `run_status` uses the enriched read; a running child shows slightly lower
  percent in reports than in status.
- The web playbook picker offers only current-workspace playbooks: the HTTP
  playbooks listing has no global-store enumeration yet, so global children
  are referenced by typing the id (resolution still works at gate/run time).

## Compatibility notes

- All schema additions are `#[serde(default)]`; schema stays 2; no
  migration.
- New `EventPayload` variants/fields follow the house rule
  (`#[serde(default)]` only).
- Playbooks without the new fields behave byte-for-byte as before (Finish
  instant, no drafts, no children).
