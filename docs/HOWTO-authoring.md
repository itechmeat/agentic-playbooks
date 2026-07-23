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
- `defaults` (profile, retries, timeout)
- `trigger`, `requires`, `effects` (see below)
- `nodes` (list) and `edges` (list)

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

## Node types

`start`, `agent_task`, `script`, `prompt`, `condition`, `human_review`,
`wait`, `finish`. A playbook needs exactly one `start` and at least one
`finish`. Edges connect node ids; conditional edges gate on node status,
review status, or output match.

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
- `run.instruction` - the run's input prompt (see below).
- `run.context` - the accumulated run context (params, instruction, node
  outputs, reviews, hooks), the same text a finish-with-prompt agent sees.
- `run.hooks.*` - the payload last posted to a `wait` node's webhook, by key
  (`run.hooks.<key>`).

An unresolvable reference (an unknown param, a node id that is not in the
playbook, a namespace outside this list) fails validation before the
playbook can be saved or run, rather than silently rendering empty at run
time.

## Human review and conditional edges

A `human_review` node pauses the run for a human decision:

```yaml
- { id: review, type: human_review, options: [approve, reject] }
```

`options` is a required list of strings: the choices a reviewer can pick.
`review_decide` records one of them as the node's decision, plus a free-form
note (available downstream as `{{nodes.review.review_note}}`).

An edge's `condition` gates traversal on one of three types:

- `node_status { node, equals: success|failure }` - matches when the named
  node's status is `success` or `failure` (which also covers a timeout).
- `review_status { equals: <option string> }` - matches when the
  `human_review` node this edge starts from was decided with exactly that
  option string.
- `output_match { node, pattern }` - matches when the named node's output
  contains `pattern` as a substring (not a regex).

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
of ONE execution. Give it as integer seconds (`90`) or a single unit suffix
(`30s`, `5m`, `2h`). For a node inside a loop this is the per-iteration time.
Use a whole number of the units above: an invalid value such as a bare decimal
(`1.5`), a negative number, or a boolean still lets the playbook load but the
validator flags it as a V20 error.

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

## Secrets

Never put secret values in a playbook or in a capture synopsis. Reference them
by env or config key name, or a placeholder param. Concrete secret-looking
values are rejected at capture and should never be committed to a definition.

## Language

Machine fields (`id`, canonical `trigger.when` / `avoid_when`) are English.
Display `name`, human `description`, and node prompts may be in any language.
Anything you say to the user about a playbook should be in the language of
their recent chat.
