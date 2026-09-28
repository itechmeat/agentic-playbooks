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
| `playbook_catalog` | Compact structural catalog (project + global scope): trigger, effects, trust, shadowing; `catalog_revision` for cheap repeat calls; optional `query` for opt-in advisory ranking |
| `projects_list` | User's workspace registry: id, name, path, state |
| `playbook_howto` | Tier 2: authoring detail (pull only when creating/reworking) |
| `playbook_interview` | Tier 2: the interview guide for building a playbook from a user interview (pull only when the user describes a process to automate) |
| `playbook_get` | Playbook definition by id and (optional) version; `detail` selects `summary` (default: interface only, no node prompt bodies) or `full` (complete authoring payload) |
| `playbook_validate` | Validate a playbook, list of issues |
| `playbook_trash_list` | The project's deleted playbooks, newest first: `name` (the restore handle), `id`, `deleted_at_ms`, `versions`, `current`, and `conflict` (a playbook with that id exists again) |
| `playbook_prepare_run` | Phase 1 of a cross-workspace run: preflight + a signed `plan_token` (executes nothing); the plan lists the parent's and every sub-playbook child's digest and trust |
| `runs_list` | List of runs |
| `run_status` | Current run status (nodes, outputs, `worktree`: the run's working tree, null for the project root). `usage`: token totals over the attempts whose agent CLI reported them (`attempts`, `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens`, `cost_usd` only when a CLI reported a cost, `cost_attempts`, `estimated`: set only for counts apb estimated itself, which none are yet), absent when none did. The numbers are recorded as each agent CLI reports them (apb only moves the cache reads a CLI counts inside its input into the cache fields), so they may not be comparable across agents. `decisions`: decision-model totals (`decisions`, `requests`, `replayed`, `errors`, `cost_usd`, `cost_estimated`, `p50_latency_ms`, `p95_latency_ms`, `by_use` with `requests`, `errors`, `applied`, `shadow_would_change` per use), absent when the run journaled no decision; each decision is a `decision_made` in `run_events` (see `docs/DECISIONS.md`). `unknown_events` and `unknown_events_note`: events of a type this apb does not know (a newer apb wrote them), skipped; absent when there are none |
| `run_wait` | Block server-side until a run finishes, needs input or stops, or `timeout_ms` ends; compact answer with `reason` and `next`. Use it instead of polling `run_status` |
| `run_events` | Run events, optionally from a given seq |
| `run_report` | Short run summary; carries `usage`, `decisions` and `unknown_events` like `run_status` |
| `profile_list` | Profiles (project + global) with bundle trust status |
| `profile_get` | Profile contents (profile.yaml + SOUL.md) and digests |
| `connectors_list` | Installed connectors an `agent_task` can bind: version, trust, `update_available` (the built-in version when the installed copy differs), function names, configured account names and `account_commands` (per account, each secret read from a command, with the command line); never other account fields or secrets |
| `trust_list` | The approvals in the user's trust store: `digest`, `id`, `kind` (`playbook`, `profile_bundle`, `connector`, `connector_account`), `origin_kind`, `approved_at_ms`; optional `kind` filter |
| `agents_detect` | Agent detection: presence, version, category, local hints for models/providers/auth. The detection itself is local - apb runs `--version` and reads local config, makes no network requests of its own (what the third-party CLI does when actually run is not something apb controls) |
| `profile_howto` | How to write profiles: format, selection rules, model table with assignments, subscriptions, detection (pull only when working with profiles) |
| `playbook_adopt_report` | Adoption readiness: profile resolvability, skill presence, bundle trust, model availability by detection |

Read-only tools over definitions and runs accept an optional `workspace`
(workspace_id from `projects_list`) to read from another of the user's
workspaces; without it, the current project is used. Structural workspace-
resolution errors (in `effective_root` and `playbook_prepare_run`):
`workspace_unreachable` - the workspace path was removed or is unreachable;
`workspace_unknown` - the id is not registered in the registry.
A workspace's id is its `.apb/workspace.local` (a regular file, gitignored);
apb does not follow a symlinked `workspace.local`, and it does not move a
registered id to another directory while the original directory still holds
it (a copied checkout keeps its own registration off until it gets its own id:
delete the copied `workspace.local`).

`playbook_catalog` returns both `dismissed_patterns` (the slug list, unchanged) and `suppressed_suggestions`: the active suggestion-decision records for the current project, merged from the project store `.apb/suggestions.json` and the global `<config-dir>/suggestions.json`, each with `pattern`, `synopsis`, `kind`, `scope`, `declines` and `snoozed_until`. Matching a candidate action against those records is done by the meaning of the synopsis, on the agent side; the server does no language processing unless the user enabled catalog ranking (below). Both fields fold into `catalog_revision`, so an `unchanged: true` response stays correct after any dismiss write. Timing defaults are `soft_backoff_days: [1, 7, 30, 90]` and `hard_ttl_days: 90`, overridable per key by a `suggestions:` section in the global `config.yaml` and in the project `.apb/config.yaml` (project wins). `apb suggestions list|allow|reset` and the dashboard's silenced-suggestions section manage the same records.

`playbook_catalog` also takes an optional `query` (the task in one sentence). It changes nothing unless the machine enabled decision-model catalog ranking (`uses.catalog_rank` in `decisions.yaml`, see DECISIONS.md "Catalog ranking") and a provider key resolves. Then, in advise mode, the response carries the unchanged catalog plus `ranked: [{ref, p}]` (top five), `confidence`, `needs_playbook_p`, `covered_by: {pattern, scope, p}` when a silenced suggestion covers the task, and `ranking: {provider, model, calibrated}`; on a provider failure `ranking: {error}` instead. The fields are advisory: entries are not filtered or reordered, the host's own matching still decides, and `revision` is bypassed while a query is ranked. The decision request runs on the blocking pool, so a slow provider never delays other tool calls. This is the one place the server does language processing, and only through this opt-in; TIER0 is unchanged whether the feature is on or off.

Mutations (destructive):

| Tool | What it does |
| --- | --- |
| `playbook_run` | Run a playbook (spawns agents, changes project files). Server-side policy gate: draft/untrusted/cross-workspace are rejected. `worktree` gives the run its own working tree (a directory in the project or a git worktree of it): its nodes run there and it does not wait on runs over other trees |
| `playbook_capture` | Distill an action into a draft playbook in the chosen scope (not executed until trial) |
| `playbook_trial` | Trial run of a draft against the effects matrix: filesystem writes go into a git worktree with a diff; irreversible effects are forbidden. Accepts an `instruction`, exactly like `playbook_run` |
| `playbook_approve` | Activation after trial/confirmation: lifecycle active, digest trusted |
| `playbook_execute_plan` | Phase 2: execute a confirmed cross-workspace plan by `plan_token` |
| `suggestion_dismiss` | Record the user's decline of a save-as-playbook suggestion: `kind` `soft` (a not-now decline whose silence escalates along the backoff schedule) or `hard` (an explicit never-again, the default so an old-style call is unchanged), a one-sentence `synopsis` of the action, and `scope` `project` (default) or `global`. The `pattern` must be a lowercase slug (`[a-z0-9][a-z0-9-]*`, at most 64 chars), so the record stays addressable by `apb suggestions` and the dashboard. Returns the stored record with the server-computed `snoozed_until`, plus a `diagnostics` array when the `suggestions:` config section is invalid or a broken store had to be moved aside. A project-scope dismiss on a directory with no `.apb` yet initializes it, since the call only happens on a root the user already connected apb to |
| `playbook_create` | New playbook or a new minor version (creating via the tool approves the digest); a definition equal to the current version creates nothing and answers `unchanged: true` |
| `playbook_update` | New minor version of an existing playbook (approves the saved digest, like create); `unchanged: true` when the definition did not change |
| `playbook_delete` | Soft delete to trash (`.apb/trash/<id>-<millis>`; runs stay) |
| `playbook_trash_restore` | Restore a trash entry by `name`, or a playbook id's latest deletion, with every version; the restored current version is approved like a save when it has no scripts. A playbook that exists again under the id is a conflict and nothing moves. Current workspace only |
| `trust_revoke` | Revoke approvals: `target` is a digest (exactly that approval) or an id (every approval under it; `kind` narrows it). Returns what was revoked. The same path as `apb trust revoke` and the dashboard's Trust view |
| `run_resume` | Resume a run, optionally from a node. Returns immediately (see Detached runs below). Only a run apb created on this machine resumes (`run_not_created_locally` otherwise, not bypassable); a run whose snapshot digest is not approved needs `acknowledge_untrusted: true` |
| `run_stop` | Stop a run: interrupt whatever node it is executing right now, and finalize it outright if the process driving it is gone |
| `review_decide` | Decide a run's human_review node |
| `run_progress_report` | Report cycle progress from inside a run: `done` of `total` iterations of the current cycle group, optional `label`; pass your own node id (`APB_NODE_ID`) when branches run concurrently |
| `run_answer` | Answer a pending interactive question on a run (an `agent_task` with `interactive: true`); plain `run_id` path posts `answered_by: "human"`, supervisor-token path posts `answered_by: "supervisor"` |
| `profile_write` | Create/update a profile (CAS via expected_digest, auto-approves the bundle); current workspace only |
| `profile_move` | Copy a profile between scopes (the source remains) |
| `profile_delete` | Delete a profile (blocked on references unless forced) |
| `subscriptions_set` | Record agents' declared subscriptions, or opt out of the poll (overlay + onboarding state) |

## Run policy and trust

Match confidence and execution risk are kept separate (spec 9). A playbook
carries a lifecycle (`draft`/`active`/`retired`) and trust tied to a content
digest of the version: its `playbook.yaml` plus every file under its
`scripts/` (`apb_core::scope::definition_digest`). Any file change (an edit
outside apb, a git pull, a changed script) drops trust, and the same YAML
shipped with other scripts is not the approved content. A version without
scripts digests exactly as its YAML alone. A run copies the scripts into its
run directory and refuses to start when the copy does not match the digest
the gate checked.

A save through apb itself - `playbook_create` / `playbook_update`, the
dashboard editor, `apb import` - goes through one save path
(`apb_core::versioning::save_definition`). A definition equal to the current
version (compared in the stored form, the `version:` field and formatting set
aside) writes nothing: no version is created, trust is untouched and the
answer carries `unchanged: true`. Otherwise the save approves the digest it wrote:
the user asked for that write, so its result is trusted (the same rule as
`profile_write`). A save writes YAML only and carries the base version's
scripts along, so a result with scripts is approved only when its base was
approved and the scripts are unchanged. A restore from the trash
(`playbook_trash_restore`, the dashboard's Trash view, `apb trash restore`)
goes through one path too (`apb_core::versioning::restore_from_trash`) and
approves the restored current version the same way when it has no scripts;
with scripts it keeps the approval its digest already had.

Upgrading to a build whose digest covers scripts invalidates the approvals of
playbooks that have scripts (playbooks without scripts keep theirs). apb does
not migrate those approvals automatically, because that would approve
whatever scripts are on disk now, which is exactly what the digest exists to
catch. Re-approve such a playbook after reviewing its scripts: MCP
`playbook_approve`, or confirm the next `playbook_run` with
`acknowledge_untrusted: true`. `trust_list` (and `apb trust list`, the
dashboard's Trust view) shows every approval; `trust_revoke` removes one by
digest, or all of an id's.
`playbook_run` goes through a server-side gate: draft is rejected (only via
`playbook_trial`), an unapproved digest requires `acknowledge_untrusted: true`
after user confirmation, and running in another workspace only happens via the
two-phase `playbook_prepare_run` / `playbook_execute_plan`.
`playbook_execute_plan` runs the same gate in the target workspace, with the
caller's `acknowledge_untrusted`: the parent and every sub-playbook child must
be approved (or acknowledged), and the verified child pins go to the engine,
so a child that changes after the check is refused when it would start. The
read-only/destructive annotations remain client hints; enforcement lives on
the server.

A resume executes what the run directory holds, and a run directory lives in
the workspace, so a repository can ship one. apb stamps every run it prepares
with an HMAC keyed by a per-installation secret (`<config-dir>/run-origin.key`,
created on first use), and MCP `run_resume` refuses a directory without a valid
stamp (`run_not_created_locally`, an acknowledge does not bypass it). It then
gates the snapshot's digest (its `playbook.yaml` plus its `scripts/`) like a
start. Runs created before the stamp existed resume from the dashboard or
`apb resume`, where the person resuming is the confirmation.

The dashboard's Run button and `apb run` (with or without `--detach` or
`--supervise`) go through the same gate (`apb_engine::gate::check_run`). The
person starting the run there is the confirmation, so an unapproved digest is
acknowledged for them; everything else refuses on every surface: a draft or
retired playbook, unmet `requires`, an unapproved connector or account, and a
broken sub-playbook tree.

Supervisor tools (`supervisor_*`) are only available inside a supervisor
session (behind a session gate) and are not listed here as part of the normal
surface. A supervisor's authority is its capability set
(`supervisor.policy.capabilities`) on those token-bearing tools. The background
supervisor agent that `apb run --supervise` spawns gets
`APB_MCP_ROLE=supervisor` in its environment; an `apb mcp` started under it
(agents pass their environment to their MCP servers) serves only the
`supervisor_*` tools, the read-only tools and `run_answer`, whose `run_id`
path (answering as the human) it refuses. The operator's run control
(`run_stop`, `run_resume`, `review_decide`), authoring and run starts take no
token, so they are not offered to a supervisor. One is worth naming regardless, because its polling contract is easy
to get wrong: `supervisor_wait_event { token, after_seq, timeout_ms }` blocks
until the run's next wake, a new human_review gate, the end of the run, or a
timeout, whichever comes first. Pass `after_seq` as the response's
`next_after_seq` (omit it on the first call), so you walk the event stream
forward instead of re-scanning wakes you already handled. `reason` says why
it returned: `wake`, `review` (relay `pending_review`), `ended` or `timeout`.
`timeout_ms` bounds the block (default 50000, max 1800000). Every return is a
model turn for the supervisor, so pass the largest value the host's tool-call
limit allows: the server refreshes the supervisor heartbeat while it blocks
(so a long wait never reads as a lost supervisor) and sends progress
notifications every 15 s when the call carries a progress token. On `timeout`,
just call again. A park wake may carry `triage: {action, p, confidence,
looping_p, provider, model, applied?}`, a decision model's recommendation
(only when the machine's `decisions.yaml` puts `supervisor_triage` in advise
or enforce; `applied: true` means the engine already posted the retry). A
`pending_review` may carry `recommendation: {option, p, confidence,
provider, model, calibrated, applied?}`, advisory and never preselected (see
DECISIONS.md). A wake's `detail` is capped at its last 16 KiB
(`detail_truncated: true`); `supervisor_run_inspect` has the full output.
`supervisor_run_inspect` itself elides texts over 512 bytes inside `events`
(they repeat `outputs`, `context` and `wakes`); pass `full_events: true` for
the raw journal.

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

`supervisor_node_retry` and `supervisor_run_continue_from` also act while the
run is parked on an undecided `human_review` gate, the moment a supervisor
often notices that an earlier node went wrong: the driver withdraws the open
review (a `review_withdrawn` event; the gate is no longer pending and the
decision surfaces refuse it), moves to the named node, and asks for the review
again when the gate is reached anew. A `supervisor_patch_playbook` queued behind
such a directive is applied right after it.

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

### Instruction source precedence

An `agent_task` (and finish-with-prompt) attempt assembles instructions from three sources: the node template (`prompt` in the playbook), the run-level `instruction`, and applied supervisor notes from `supervisor_context_append`. The engine appends the run instruction and the supervisor notes to the rendered template as explicit trailing sections on every new attempt, so both reach the executor even when the template references neither `{{run.context}}` nor `{{run.instruction}}`; the run instruction lands in its own `## run instruction` section and the notes in a trailing supervisor notes block. When the template does reference `{{run.context}}`, the context already leads with the `## run instruction` section, so the trailing copy is left out and the instruction appears exactly once; applied notes also appear inside the context, next to their trailing block. On conflict the engine frames the higher sources as overrides: supervisor notes override the run instruction, and the run instruction overrides the node template's boilerplate. The notes block header and a short trailing `## instruction precedence` section state this order explicitly; neither re-embeds the full instruction text. When both the run instruction is empty and no notes are applied, the prompt is left byte-unchanged (no spurious framing).

## Asynchronous run model

A run can take minutes, while some hosts have a short timeout on a single
tool call (for example, ChatGPT Apps at around 60 seconds). That's why
`playbook_run` supports a non-blocking mode:

- `playbook_run` with `background: true` starts the run in the background and
  returns `run_id` **immediately**, without waiting for completion.
- The client then calls `run_wait { run_id, timeout_ms }`, which blocks
  server-side and returns only when the run finishes (`reason: finished`),
  needs input (`needs_input`, with `pending_question`, `pending_review` or
  `pending_supervisor`), is paused or driverless (`stopped`), or `timeout_ms`
  runs out (`timeout`: call it again). Do not poll `run_status` in a loop:
  every call is a model turn that re-reads the whole conversation plus every
  node output, while `run_wait` costs one turn per decision. `timeout_ms`
  defaults to 50000 (under the strictest ~60 s host limits) and goes up to
  1800000; pass the largest value your host allows. Progress notifications go
  out every 15 s when the call carries a progress token. A gate must stay
  pending for 1.5 s before `run_wait` reports it, so a wait right after
  `run_answer` or `review_decide` does not return the gate just answered.
- If the run hits a human_review node, the client resolves it via
  `review_decide`, and the run continues.
- If the run hits an interactive `agent_task` node that asked a question
  (`run_status.pending_question` non-null), the client resolves it via
  `run_answer`, and the run continues.

Without `background: true`, behavior is unchanged: `playbook_run` blocks
until completion and returns the result. This remains the default for
backward compatibility.

The CLI has the same shape for an agent that drives apb through a shell:
`apb run <id> --detach` prints the run id and returns at once, and
`apb wait <run_id> [--timeout SECS]` blocks until the run finishes, needs
input or stops. Run `apb wait` as one background command and act on its exit
code (0 succeeded, 1 failed or aborted, 3 needs input, 4 paused or
driverless, 5 timeout); no status polling, no tokens while it blocks. A plain
`apb run <id>` also blocks, but it cannot report a gate: it just keeps waiting
until someone answers it.

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
`from_node`). Follow it with `run_wait` the same way you would for a
`background: true` run.

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

## Protocol compatibility and adoption plan

apb serves the MCP over the local stdio transport, through rmcp pinned at `=3.0.1` with the `transport-io` feature. rmcp 3.0.1 implements MCP spec revision 2026-07-28 (the stateless core) and keeps the older revisions; it negotiates the client's requested revision at `initialize` time, so one binary serves both an old-revision host (Claude Code 2.1.220 speaks 2025-06-18) and a future new-revision host. This is the partial adoption of issue #57: everything not gated on a host that actually speaks 2026-07-28. The exact-pin (`=3.0.1`) is deliberate: rmcp is a beta-tier SDK and a patch can change wire behavior, so the version moves only under a conscious re-verification of the compatibility below.

What apb depends on from the protocol: the stdio transport; the server `instructions` field for tier-0 behavior delivery (see `docs/HOST-INTEGRATION.md`); `tools/list` and `tools/call`; and the `readOnlyHint` / `destructiveHint` annotations as client-side hints only. Nothing in the run lifecycle depends on protocol-level session state. Tier-0 delivery now reaches both channels from a single source: rmcp 3.0.1 derives the new-revision `server/discover` response (`DiscoverResult`) from `get_info`, copying its `instructions` verbatim, so overriding `get_info` alone feeds TIER0 to an old-revision host through the `initialize` result and to a new-revision host through `server/discover`, with no separate wiring.

What apb deliberately does not use: none of the features that the 2026-07-28 revision deprecates. No Roots, no Sampling, no MCP-level Logging, no legacy HTTP+SSE transport, and no Dynamic Client Registration. Supervisor "sessions" and cross-workspace plan tokens are application-level handles minted by apb, carried as tool arguments and persisted to the run directory, not protocol sessions bound to an `Mcp-Session-Id`. This is already the explicit-handle pattern that the stateless core canonizes, so no protocol session state needs migration when apb adopts that revision. `playbook_catalog` runs its own revision-digest caching with `unchanged` responses, semantically aligned with the revision's `ttlMs` / `cacheScope` metadata but not using those spec fields.

The `ttlMs` / `cacheScope` alignment is documented-and-skipped, but the reason is compatibility, not absence of the fields. rmcp 3.0.1 exposes `ttlMs` / `cacheScope` on `DiscoverResult` (the `server/discover` response) and, through the `paginated_result` macro, on `ListToolsResult` (the `tools/list` response) as `Option` fields with `withTtlMs` / `withCacheScope` builders. `DiscoverResult` gets them fixed at `ttl_ms = 0` and `cache_scope = Private` by `from_server_info`; the default `discover` handler offers no injection point from `get_info`, so reaching the builders would mean overriding `discover`. `CallToolResult` (every `tools/call`, including `playbook_get` and `playbook_catalog`) genuinely carries neither field, so the `playbook_catalog` tool payload has nothing to align and keeps its content-digest `unchanged` scheme as the tool-payload-level equivalent.

The `tools/list` fields, however, cannot be emitted safely yet. They default to `None` and are skip-serialized (which is why the old-revision wire is unchanged today), but rmcp does not gate them by negotiated revision: `strip_result_type_for_legacy_peer` strips only `resultType`, not `ttlMs` / `cacheScope`. Setting either would therefore add the field to the `tools/list` response seen by an old-revision (2025-06-18) host as well, which changes the old-client wire shape and violates the hard compatibility acceptance. Emitting them correctly needs per-negotiated-revision gating plus a measured TTL, both of which depend on a host that actually speaks 2026-07-28. apb also serves `tools/list` through the macro-derived `list_tools` handler and never constructs `ListToolsResult` itself, so wiring this would mean overriding `list_tools`. The alignment is deferred to the same host-gated open item; apb sets neither field for now.

Initialize-time state audit (`crates/apb-mcp/src/server/`, one-time, read-only, 2026-07-30): the `WfMcp` handler in `crates/apb-mcp/src/server/mod.rs` overrides only `get_info`; there is no custom `initialize`, `on_initialized`, `set_peer`, or `Mcp-Session-Id` handling in the crate. `get_info` returns the tool capability set, the server implementation identity, and `with_instructions(crate::instructions::TIER0)`. The `instructions` field is therefore the single load-bearing dependency on the `initialize` handshake. All other server state is minted at tool-call time, not initialize time: supervisor session tokens (`mint_token`, on `playbook_run` with `supervise: "self"`) and single-use plan nonces (`used_nonces`), both keyed by tokens that travel as tool arguments and resolve from the run directory across processes. The sidecar `AskServer` (`crates/apb-mcp/src/ask_server.rs`) sets no instructions and holds no initialize-time state. Conclusion: adopting the stateless core requires re-homing only the tier-0 instruction delivery; no run, supervisor, or plan state has to migrate.

Post-migration state (rmcp 3.0.1). The handler model needed no source change: rmcp 3.0.1 kept the exact API surface apb used under 2.2.0 (`ServerInfo::new` / `with_server_info` / `with_instructions`, the `#[tool_router]` / `#[tool_handler]` macros, `ServiceExt::serve`, `transport::stdio`), so `WfMcp` still overrides only `get_info` and the audit above holds unchanged. Old-revision compatibility was verified on this branch (2026-07-30), not assumed: a scripted stdio client speaking `protocolVersion` 2025-06-18 (`initialize`, `notifications/initialized`, `tools/list`, `tools/call playbook_get`, `tools/call playbook_catalog`) produced byte-identical output from the 2.2.0 binary and the 3.0.1 binary. That was a one-time diff, not a committed regression; the repeatable multi-host regression is an open item below. rmcp negotiates the client's requested revision back to it, strips the 2026-07-28 `resultType` discriminator for any peer that negotiates an older revision (`strip_result_type_for_legacy_peer` over `ListToolsResult` and `CallToolResult`), and omits `_meta` when unset, so the legacy wire shape is preserved exactly. Supervisor sessions, the policy-gate `RunPermit`, and the anti-TOCTOU expected map are untouched and their tests stay green.

Open issue #57 items, both gated on a host that actually speaks 2026-07-28 and therefore left open. First, re-run the controlled headless measurement harness against a new-revision Claude Code and re-measure the tier-0 delivery contract (instructions delivery path, truncation limit, offer-to-save trigger), then update `docs/HOST-INTEGRATION.md` with the new measured contract; the current 2KB truncation figure is measured on the old revision only. Second, run the full multi-host regression on one binary (Claude Code old and new revision, Codex, Hermes) and re-check the supervisor long-poll (`supervisor_wait_event`) under the new revision. Neither can be done until a host ships the revision, which none has yet. Follow-up work unlocked by this migration (MCP Tasks for runs, MRTR for human gates, MCP Apps) stays tracked against issue #57 and is split into separate issues when this lands.
