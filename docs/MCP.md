# APB as an MCP server

APB exposes its capabilities as an MCP server (Model Context Protocol) over stdio:

```
apb mcp
```

The server operates on the current project's root (the directory containing
`.apb/`). Any MCP client that supports the local stdio transport can connect
APB and call its tools: list and read playbooks, start runs, watch status,
resolve review gates.

## Tool surface

The public set is narrow and deliberate. Each tool carries safety annotations
(`readOnlyHint` / `destructiveHint`) that the client uses to build
confirmations.

Important: the annotations are hints (advisory metadata), not authorization
and not enforced control. The server does not enforce them. An MCP client
that ignores them can call a destructive tool, and it will run with the full
privileges of the local `apb` process: filesystem, git, keys. So only connect
APB to trusted clients, and rely on OS/process-level privilege restriction
(a separate user, a sandbox, minimally required access) rather than on the
annotations themselves.

Reads (read-only):

| Tool | What it does |
| --- | --- |
| `playbook_list` | List of the project's playbooks |
| `playbook_catalog` | Compact structural catalog (project + global scope): trigger, effects, trust, shadowing; `catalog_revision` for cheap repeat calls |
| `projects_list` | User's workspace registry: id, name, path, state |
| `playbook_howto` | Tier 2: authoring detail (pull only when creating/reworking) |
| `playbook_get` | Playbook definition by id and (optional) version |
| `playbook_validate` | Validate a playbook, list of issues |
| `playbook_prepare_run` | Phase 1 of a cross-workspace run: preflight + a signed `plan_token` (executes nothing) |
| `runs_list` | List of runs |
| `run_status` | Current run status (nodes, outputs) |
| `run_events` | Run events, optionally from a given seq |
| `run_report` | Short run summary |
| `profile_list` | Profiles (project + global) with bundle trust status |
| `profile_get` | Profile contents (profile.yaml + SOUL.md) and digests |
| `agents_detect` | Agent detection: presence, version, category, local hints for models/providers/auth. The detection itself is local - apb runs `--version` and reads local config, makes no network requests of its own (what the third-party CLI does when actually run is not something apb controls) |
| `profile_howto` | How to write profiles: format, selection rules, model table with assignments, subscriptions, detection (pull only when working with profiles) |
| `playbook_adopt_report` | Adoption readiness: profile resolvability, skill presence, bundle trust, model availability by detection |

Read-only tools over definitions and runs accept an optional `workspace`
(workspace_id from `projects_list`) to read from another of the user's
workspaces; without it, the current project is used. Structural workspace-
resolution errors (in `effective_root` and `playbook_prepare_run`):
`workspace_unreachable` - the workspace path was removed or is unreachable;
`workspace_unknown` - the id is not registered in the registry.

Mutations (destructive):

| Tool | What it does |
| --- | --- |
| `playbook_run` | Run a playbook (spawns agents, changes project files). Server-side policy gate: draft/untrusted/cross-workspace are rejected |
| `playbook_capture` | Distill an action into a draft playbook in the chosen scope (not executed until trial) |
| `playbook_trial` | Trial run of a draft against the effects matrix: filesystem writes go into a git worktree with a diff; irreversible effects are forbidden. Accepts an `instruction`, exactly like `playbook_run` |
| `playbook_approve` | Activation after trial/confirmation: lifecycle active, digest trusted |
| `playbook_execute_plan` | Phase 2: execute a confirmed cross-workspace plan by `plan_token` |
| `suggestion_dismiss` | Record the user's decline of a suggestion (do not suggest again) |
| `playbook_create` | New playbook or a new minor version (creating via the tool approves the digest) |
| `playbook_update` | New minor version of an existing playbook |
| `playbook_delete` | Soft delete to trash |
| `run_resume` | Resume a run, optionally from a node. Returns immediately (see Detached runs below) |
| `run_stop` | Stop a run: interrupt whatever node it is executing right now, and finalize it outright if the process driving it is gone |
| `review_decide` | Decide a run's human_review node |
| `run_answer` | Answer a pending interactive question on a run (an `agent_task` with `interactive: true`); plain `run_id` path posts `answered_by: "human"`, supervisor-token path posts `answered_by: "supervisor"` |
| `profile_write` | Create/update a profile (CAS via expected_digest, auto-approves the bundle); current workspace only |
| `profile_move` | Copy a profile between scopes (the source remains) |
| `profile_delete` | Delete a profile (blocked on references unless forced) |
| `subscriptions_set` | Record agents' declared subscriptions, or opt out of the poll (overlay + onboarding state) |

## Run policy and trust

Match confidence and execution risk are kept separate (spec 9). A playbook
carries a lifecycle (`draft`/`active`/`retired`) and trust tied to a content
digest: any file change (an edit outside apb, a git pull) drops trust.
`playbook_run` goes through a server-side gate: draft is rejected (only via
`playbook_trial`), an unapproved digest requires `acknowledge_untrusted: true`
after user confirmation, and running in another workspace only happens via the
two-phase `playbook_prepare_run` / `playbook_execute_plan`. The
read-only/destructive annotations remain client hints; enforcement lives on
the server.

Supervisor tools (`supervisor_*`) are only available inside a supervisor
session (behind a session gate) and are not listed here as part of the normal
surface. One is worth naming regardless, because its polling contract is easy
to get wrong: `supervisor_wait_event { token, after_seq, timeout_ms }` blocks
until the run's next wake, or a timeout, whichever comes first. Pass
`after_seq` as the `seq` of the last wake you already saw (omit it on the
first call); the response's `wake.seq` becomes your next `after_seq`, so you
walk the wake stream forward instead of re-scanning wakes you already
handled. `timeout_ms` bounds the block (default 25000). `wake: null` means
the run already reached a terminal state, or the call simply timed out with
nothing new - the caller decides whether to wait again.

An interactive `agent_task` node (`interactive: true`) can park a run on a
question mid-attempt; `run_status`'s `pending_question` (`{ node, question,
options, answer_by, asked_at }`, `null` when nothing is pending) and
`progress.waiting_kind: "question"` report it, and `supervisor_wait_event`
raises a wake the moment it is asked. The node's `answer_by` sets who may
resolve it, and this is a contract for a supervisor agent, not just a
capability check: for `answer_by: human`, relay the question to the user
verbatim, in the user's chat language, and post back their answer to
`run_answer` verbatim - never answer such a question with the supervisor's
own judgment. A supervisor that tries anyway is refused: `run_answer`'s
supervisor-token path against an `answer_by: human` node returns an error
instructing it to relay the question instead. For `answer_by: supervisor`,
the supervisor may answer directly from its own judgment, and should still
escalate to the user when unsure rather than guess.

Each supervisor tool requires a capability the run's `supervisor.policy.capabilities`
grants; the default when the key is absent is all of them
(`observe`, `retry`, `rebind`, `patch_playbook`). `observe` covers reads
(`supervisor_wait_event`, `supervisor_run_inspect`, `supervisor_report`);
`retry` covers in-run control-flow interventions (`supervisor_node_retry`,
`supervisor_run_continue_from`, `supervisor_run_pause`, `supervisor_run_abort`,
`supervisor_context_append`, `supervisor_interrupt_attempt`); `patch_playbook`
gates `supervisor_patch_playbook`.

`rebind` gates `supervisor_rebind_profile { token, node, profile, scope?,
acknowledge_untrusted?, reason? }`, the sanctioned escape hatch for switching a
node's executor profile mid-run when its bound agent is wedged (a service that
hangs on every attempt). Per-node executor bindings are pinned in the immutable
run manifest, so a playbook patch that only swaps a node's profile does not move
the running binding; this tool does. It re-runs the trust gate for the NEW
profile bundle exactly as run start does (an unapproved bundle is refused with
`untrusted_profile_requires_acknowledge` unless `acknowledge_untrusted: true` is
set after user confirmation; a missing profile with `profile_unresolved`),
journals the accepted rebind as a `profile_rebound` event, and changes the
node's effective binding for future attempts through a journaled overlay while
leaving the original manifest intact as the record of what the run started with.
The verified bundle is pinned and re-checked from the run snapshot when the drive
applies it, so any drift between gate and apply is refused (`rebind_rejected`).
It is its own capability because it is strictly larger than a retry, so a policy
can grant `retry` without granting `rebind`. The usual sequence is
`supervisor_rebind_profile` then `supervisor_node_retry`: the next attempt picks
up the new profile.

## Asynchronous run model

A run can take minutes, while some hosts have a short timeout on a single
tool call (for example, ChatGPT Apps at around 60 seconds). That's why
`playbook_run` supports a non-blocking mode:

- `playbook_run` with `background: true` starts the run in the background and
  returns `run_id` **immediately**, without waiting for completion.
- The client then polls `run_status` (or `run_events` with an increasing
  `seq`) until the status becomes terminal (`succeeded` / `failed`).
- If the run hits a human_review node, the client resolves it via
  `review_decide`, and the run continues.
- If the run hits an interactive `agent_task` node that asked a question
  (`run_status.pending_question` non-null), the client resolves it via
  `run_answer`, and the run continues.

Without `background: true`, behavior is unchanged: `playbook_run` blocks
until completion and returns the result. This remains the default for
backward compatibility.

## Detached runs, resume, and stop

A run started via `playbook_run` with `background: true`, via
`supervise: "self"`, or via `run_resume`, is handed to a separate DETACHED
driver process (spawned from the current `apb` executable, stdio nulled) that
survives the calling MCP session: the run keeps going even if the chat
session, or the `apb mcp` process that started it, exits. That process writes
`runs/<id>/driver.pid` while it drives the run and removes it on a clean
exit; `run_status`'s `driver_alive` field reports whether that pid is
currently alive (`null` when no driver ever claimed the run, for example a
run driven synchronously in-process).

`run_resume` does not wait for the resumed run to finish. It computes the
resume decision, hands the run to a detached driver, and returns an ack right
away:

```json
{ "run_id": "...", "resumed_from": "some_node", "reason": "interrupted_restart", "detached": true }
```

`resumed_from` is the node id the run resumes at; `reason` is one of
`interrupted_restart` (exactly one node was cut off mid-execution and
restarts), `advance_past_finished` (nothing was interrupted; the run
continues past the last finished node without re-running it),
`parallel_fallback` (two or more branches were cut, so the run restarts from
the last finished node), or `explicit_from_node` (the caller named
`from_node`). Poll `run_status` / `run_events` afterward the same way you
would for a `background: true` run.

When the run still has an unapplied stop in its control queue, the ack also
carries `"stops_on_pending_abort": true` and a `note` saying so. Control
commands are consumed in order, so that resume applies the stop and the run
stops again without executing anything; call `run_resume` once more to
continue past it. This is what the stop, note, resume recovery pattern looks
like from the tool side.

`run_stop { run_id }` posts an abort. If a live driver owns the run, that
driver's watcher interrupts the in-flight node and its own drive loop writes
the terminal event (`outcome: "signaled_live_driver"`). If nothing is driving
the run any more, `run_stop` finalizes it itself
(`outcome: "finalized_dead_run"`). If the run was already terminal, nothing
is written (`outcome: "already_terminal"`). `apb stop <run_id>` is the CLI
equivalent; `apb doctor --run <id>` diagnoses a run's process state read-only
(open attempts and their pid liveness, the driver and workdir-lock holders,
unapplied control entries).

## Connecting (local agents, no relay)

Local agents run the stdio MCP server themselves, so APB works for them as-is,
with full access to the filesystem, git, and keys.

Claude Desktop (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "apb": {
      "command": "apb",
      "args": ["mcp"],
      "cwd": "/path/to/your/project"
    }
  }
}
```

opencode (`opencode.json`):

```json
{
  "mcp": {
    "apb": {
      "type": "local",
      "command": ["apb", "mcp"]
    }
  }
}
```

Hermes and Pi consume local stdio MCP servers the same way: the launch command
is `apb mcp` in the project directory.

## Cloud hosts (ChatGPT, Claude.ai web)

These products run in the vendor's cloud and can only call a public HTTPS
remote MCP endpoint, while APB runs locally. That means they need a hosted
relay (remote MCP + OAuth) that reaches the local machine. This is a separate
milestone; see the design doc
`docs/superpowers/specs/2026-07-10-remote-access-design.md`, section 13.3.
