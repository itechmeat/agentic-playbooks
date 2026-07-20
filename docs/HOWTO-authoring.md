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

- `params.<name>` - a declared playbook param's value, by name.
- `nodes.<id>.output` - the node's output text.
- `nodes.<id>.report` - the same value as `.output` (an alias; both names
  resolve identically).
- `nodes.<id>.review_note` - the reviewer's note from a `human_review` node's
  decision.
- `run.instruction` - the run's input prompt (see below).
- `run.context` - the accumulated run context (params, instruction, node
  outputs, reviews, hooks), the same text a finish-with-prompt agent sees.
- `run.hooks.<key>` - the payload last posted to a `wait` node's webhook `key`.

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

An edge with no `condition` always matches. A worked example wiring a review
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

## Bounded loops

An edge may carry `max_traversals` (an integer >= 1): the number of times
that edge may be traversed in one run. A cycle in the graph is only legal
when at least one of its edges is bounded this way (validator V11); an
unbounded cycle is refused, and `max_traversals: 0` is refused separately
(validator V30). Once a bounded edge reaches its cap, edge selection treats
it as non-matching, so the run takes whatever alternative edge is wired (or
hits the ordinary no-matching-edge behavior if none is). The canonical
fix-loop:

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
