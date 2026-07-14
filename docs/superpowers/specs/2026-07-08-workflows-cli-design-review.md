# Review of the Workflows CLI design document

Review date: 2026-07-08  
Source document: `docs/superpowers/specs/2026-07-08-workflows-cli-design.md`

## Summary

The document describes a strong and largely coherent idea: a local workflow runner with a visual editor, an event log, agent adapters, and a supervising agent. The biggest value of the project is stated clearly: not just running a graph, but being able to recover from failures via the supervising agent (SA).

The main risk of the current version of the spec is that it simultaneously tries to be an MVP plan and a target architecture. Because of this, some decisions look finalized earlier than the invariants have actually been pinned down: what exactly is immutable, who has the right to change a workflow, how side effects of parallel runs are prevented, which SA actions are allowed without confirmation, how replay distinguishes external effects that already happened from ones that have not happened yet.

Below, the notes are sorted by importance. They do not call for expanding functionality; on the contrary, they mostly suggest narrowing and clarifying the current stage.

## Critical remarks

### 1. MVP invariants and target architecture need to be separated

Right now the stated goals include a full visual editor, MCP, the SA, retry/fallback, semver, two-way YAML sync, a local server, and real-time observation. This is split across phases, but in the main sections many later-stage capabilities are described as baseline properties of the system.

Risk: the implementation will start dragging in architectural complexity before the core's basic invariants are proven. This is especially dangerous for event sourcing, versioning, and the SA: if they are built "roughly," the run and workflow formats will later have to be broken to fix them.

Proposal:

- Add a subsection near the start of the document: "Invariants of the first working version."
- Explicitly state that only the following must be stabilized in the first working version:
  - the `workflow.yaml` schema;
  - the validator;
  - the immutable workflow version directory;
  - the append-only event log;
  - running a linear/conditional graph;
  - deterministic replay/resume without automatic workflow edits.
- Mark everything related to autonomous SA behavior, visual-editor write-back, parallel branches, MCP CRUD, and ACP as extensions layered on top of these invariants.

This does not change the product goal, but makes the project technically more manageable.

### 2. `ui.xyflow` breaks the stated immutable version model

The versions section states that version folders are immutable, but then introduces an exception: changes to `ui.xyflow` are overwritten in place in the current version and do not create a new version.

Risk: a single exception undermines the simple rule "version = immutable snapshot." This affects auditing, diffing, replay of old runs, and potential races between the web editor, the file watcher, and the SA.

Proposal:

- Do not store mutable layout inside the immutable `workflow.yaml` of the current version.
- Options that do not expand functionality:
  - move the layout into a separate file next to the `current` pointer, e.g. `.wf/workflows/<name>/layouts/<version>.xyflow.yaml`;
  - or treat a layout change as a new patch/minor version, but exclude it from the semantic diff;
  - or store the layout as user editor state outside the version, e.g. `.wf/ui/workflows/<name>/<version>.yaml`.

The best option for the current stage: move the layout out of the immutable version. That way `workflow.yaml` stays a clean execution definition, and the UI layout does not compromise replay.

### 3. The SA's autonomy is too broad for a local tool that changes the user's files

Right now the SA is "fully autonomous": it fixes, patches, and restarts without confirmation. There are limiters, but they are mostly quantitative: patch count, the patch version component, logging.

Risk: the SA can make a technically valid but undesired change to the workflow or the working folder. It is especially dangerous that the SA has both the ordinary project access of a coding agent and supervisor tools for changing execution.

Proposal:

- Introduce an SA capability model into the spec, not as a new feature but as a mandatory constraint on the current design:
  - `observe` - read events, logs, context;
  - `retry` - restart nodes and add one-off clarifications;
  - `patch_workflow` - change the workflow;
  - `edit_workspace` - change project files via its own agent access.
- Fix the default for local runs: only `observe` and `retry` are autonomous, and `patch_workflow` is autonomous only if explicitly enabled in the workflow policy.
- For `edit_workspace`, note that `wf` itself cannot fully forbid the agent from changing the project, but must show the risk mode and recommend `workspace: worktree` for such runs.

Right now the document pretends that a "supervisor tool token" sufficiently isolates the risk. In practice this only isolates the `wf` API, not the file actions of the coding agent itself.

### 4. Event sourcing is described well, but lacks a model of idempotency and external effects

The document says that run state is a fold over events, and that resume continues from any point. But nodes perform real external effects: agents change files, scripts may push, call APIs, write to the environment.

Risk: after a process crash or `resume --from-node`, the system may re-run a node that already partially performed an effect but did not manage to write `node_finished`. The event log by itself does not solve this problem.

Proposal:

- Add an explicit classification to section 8.1:
  - the event log guarantees recovery of `wf`'s internal state;
  - the event log does not roll back or make idempotent the effects of nodes;
  - every node attempt has an `attempt_id`, a working log folder, and a terminal `attempt_finished` event;
  - if a `node_started` is found without a terminal event after a restart, the node gets the status `interrupted`, rather than being automatically treated as failed/succeeded.
- For resume, fix a default policy: re-running an interrupted node requires an explicit `resume` or an SA decision, because automatic re-running can be dangerous.

This substantially improves the honesty of the model without adding new user-facing capabilities.

### 5. Parallel runs in the same working folder are dangerous as a default

Section 8.3 states that by default parallel runs work in the same working folder, and the user is responsible for conflicts. That is fine for a read-only workflow, but the project's main example use case is coding agents that change code.

Risk: two runs can modify the same files at the same time, break git state, mix results together, and each run's event log will still look correct even though the actual project state has already been contaminated by the other run.

Proposal:

- Make the spec's default stricter:
  - parallel write-runs in the same working folder are forbidden, or require an explicit `--allow-shared-workdir`;
  - `workspace: worktree` - the recommended and safe mode for parallel runs with agent_task/script;
  - read-only mode can be declared at the workflow or run level.
- If introducing a read-only declaration right now is undesirable, a simpler approach: when a second active run starts in the same workdir, show a warning and require an explicit flag.

This is not an expansion of functionality, but protection against a class of bugs that would otherwise be hard to diagnose.

## Significant remarks

### 6. The workflow schema needs stricter validation rules

The document lists many rules but does not formulate the validator's full contract. For a project where YAML is the source of truth, this is critical.

Proposal to add a separate subsection "Minimal validator contract":

- uniqueness of `workflow.id`, `node.id`, executor names and params;
- `version` matches the folder name;
- exactly one `start`;
- all edges reference existing nodes;
- no incoming edges into `start`;
- `finish` has no outgoing edges;
- all non-finish nodes are reachable from `start`;
- all branches either lead to `finish`, or the validator explicitly warns about a potential hang;
- a condition node has outgoing edges with conditions or a fallback;
- conditions only reference nodes that can execute before the condition in this graph;
- cycles are only allowed via a condition with `max_loops`;
- the script path does not escape the version folder;
- templates reference existing params/nodes/hooks;
- executor/profile references resolve unambiguously.

Some of this is currently implied, but it is better to make the validator the centerpiece of the first phase.

### 7. Workflow semver conflates change authorship with compatibility

Current semantics: major - a new edition, minor - a user change, patch - an SA change. This is convenient for history, but differs from ordinary semver semantics, where major/minor/patch describe compatibility and the nature of the change, not the author.

Risk: a small user edit to a prompt is always minor, and an autonomous SA edit is always patch, even if the SA changed the graph so substantially that the workflow's behavior meaningfully changed. The name "semver" can be misleading.

Proposal:

- Either honestly call it not semver but a "workflow revision number in semver shape."
- Or keep semver, but add `change_author: user | supervisor` and `change_reason`, and choose the bump based on the type of change.

For the current stage, the first option is simpler: keep the `X.Y.Z` format, but do not promise full semver compatibility semantics.

### 8. `current` should not automatically move to an SA patch without an explicit rule

In section 10.2, an SA patch updates `current`. This is questionable: the SA is fixing a specific run, but its patch may be a local workaround tied to specific parameters or project state.

Risk: the next user run will silently follow the version created during the emergency recovery of the previous run.

Proposal:

- Fix one of the following modes:
  - safe default: an SA patch creates a new version and migrates the current run, but does not update `current`;
  - promotion to `current` requires the policy `promote_supervisor_patches: true`;
  - the web UI shows such versions as "candidate patch."

This preserves the SA's ability to continue a run, but does not turn an emergency fix into the new norm without the user's knowledge.

### 9. A formal run/node state machine is missing

Statuses appear scattered across different places: `success`, `failure`, `unknown`, `interrupted`, `paused`, `aborted`, `timeout`, `slow`, `stuck`. But there is no single state machine.

Risk: different parts of the system will start interpreting statuses differently: the UI, the scheduler, conditions, resume, the SA, and MCP.

Proposal:

- Add a table of node statuses:
  - `pending`, `ready`, `running`, `succeeded`, `failed`, `unknown`, `timed_out`, `interrupted`, `skipped`, `cancelled`.
- Add a table of run statuses:
  - `created`, `running`, `waiting_review`, `waiting_signal`, `paused`, `succeeded`, `failed`, `aborted`, `interrupted`.
- Separately describe which statuses are available in `condition.node_status`. For example, do not conflate the internal `timed_out` with semantic `failure`; map them explicitly instead.

### 10. The headless adapter contract is currently too optimistic

For agent_task, status is determined by the final YAML block. This is good as a contract, but headless CLI agents often stream JSON, markdown, tool output, errors, and may cut off a response.

Risk: parsing the final block will become a fragile part of the system, and the `unknown` status will occur frequently.

Proposal:

- Add mandatory result normalization to the adapter spec:
  - the raw transcript is always saved;
  - the adapter tries to extract a structured report;
  - if that fails, it creates a synthetic report with status `unknown`, a reason, and a link to the transcript;
  - `classify_error` returns a type: `transport`, `process_exit`, `structured_output_missing`, `agent_reported_failure`.
- For the first implementation, limit to one headless adapter and one output format, so as not to design for universality prematurely.

### 11. `prompt` nodes with downstream scope may behave unexpectedly in loops

The rule "a prompt applies to all downstream agent_task nodes, if it was itself executed before the start of a given node" is formally well-defined, but for a visual editor user it may not be obvious. In loops, a prompt may start applying to a node again, or to a branch where the author did not expect it to have influence.

Proposal:

- In the validator/document, add a warning for a prompt node that sits inside a loop or has downstream reach through join/parallel branches.
- In the agent_task context, explicitly record which prompt nodes were applied.
- In the monitor UI, show the effective prompt/context composition for each attempt.

This is not new semantics, just observability of behavior that is already specified.

### 12. Webhook and local server security is described too briefly

There is `127.0.0.1`, a supervisor token, and the endpoint `/api/hooks/<run-id>/<key>`. But it is not described whether `key` is a secret, how it is generated, whether it can be guessed, or what happens with `scope: workflow`.

Proposal:

- State that the webhook key in the runtime URL must be an unpredictable token, even if the user sets a logical hook name in YAML.
- Separate `hook_name` (from the workflow) from `hook_secret` (from run state).
- For `scope: workflow`, require explicit opt-in and describe how accidental signaling of all active runs is prevented.

## Medium remarks

### 13. Implementation phases should be reordered or narrowed

Phase 3 puts the SA before the editor, because that is the main value. The argument is understandable, but technically the SA relies on mature workflow patch/diff/validation/migration mechanisms. If those have not been proven yet through the editor and manual versions, the SA will be built on an unstable foundation.

Proposal:

- Phase 3A: supervisor observe/retry/report without `workflow_patch`.
- Phase 3B or 4: `workflow_patch`, after version, diff, and validator have already been proven via the editor/manual edits.

This way the SA appears early, but its riskiest capability is added later.

### 14. The MCP server as a thin client to `wf serve` needs a clear locking model

The lock file with PID and port solves part of the problem, but questions remain:

- what to do with a stale PID that is now taken by another process;
- how to tell apart a server for a different project on the same port;
- how multiple MCP sessions share one engine;
- how supervisor tokens are issued and invalidated.

Proposal:

- Add a project root fingerprint and a server instance id to the lock file.
- On connecting, have the MCP client handshake with the server: verify the root and instance id.
- Bind the supervisor token to `run_id`, `session_id`, and an expiry/heartbeat.

### 15. Atomicity of file operations needs to be described up front

The project relies heavily on files: `current`, versions, events.jsonl, index.jsonl, run.yaml, context.md. Without atomic write rules, it is easy to end up with corrupted state.

Proposal:

- Write all YAML/JSON control files via temp file + fsync + atomic rename.
- Write `events.jsonl` append-only, with flush/fsync on terminal events.
- Update `current` only via atomic rename.
- Treat `index.jsonl` as a derived index that can be rebuilt from runs.

### 16. `runs/index.jsonl` should not be the source of truth for `node_slow`

Duration history is taken from `runs/index.jsonl`. But the index is lightweight and potentially derived. If it is corrupted or deleted, `node_slow` behavior should not become incorrect.

Proposal:

- Describe `index.jsonl` as a cache/materialized view.
- If the index is missing, `node_slow` should use only `timeout_seconds`, or be disabled until history accumulates.

### 17. A global agent config may hurt workflow portability

A workflow references executors, and executors reference agent ids from a global config. That is correct for secrets and launch commands, but it reduces portability across machines.

Proposal:

- Add a `wf doctor` command/check, or at least a `wf validate --environment` rule: verify that all agents/executors/profiles actually resolve in the current environment.
- Clarify in the document itself that ordinary schema validation and environment validation are different levels.

### 18. Compressing `context.md` with a cheap model conflicts with replay determinism

Section 8.5 says that if context exceeds the limit, the engine asks a cheap model to compress older sections. This is useful, but it is no longer a deterministic materialized view.

Proposal:

- Split into a full `context.md` as a reproducible view, and `context_compact.md` as an LLM-compressed artifact.
- In events, record a reference to the compact artifact and the model used, but do not replace the primary context.

### 19. `wait`/webhook should be narrowed for the MVP

In the phases, `wait` and webhook are assigned to a later phase, but the schema's full example already includes a CI webhook. That is fine for the target design, but it may confuse the implementation.

Proposal:

- In the full workflow example, keep the webhook but mark it as post-MVP.
- For the early phases, use only `timer`, or defer `wait` entirely.

### 20. The boundary between "meaningful failure" and "infrastructure error" needs to be defined more precisely

Fallback applies only to infrastructure errors. But if an agent returns an invalid structured report, is that a transport/format problem or a meaningful failure? What if the model refused because of context limits? What if the CLI exits 0 but the output is empty?

Proposal:

- Add an error enum with examples to the adapters section.
- Specify which error classes trigger fallback, which trigger retry, and which go to the SA.

## Low-priority remarks and copy edits

### 21. The names `profile` and `SOUL.md`

`SOUL.md` nicely inherits the Hermes vocabulary, but for a standalone tool it may not be obvious. If that is intentional, a short explanation should be added that this is the role's system prompt. If not, a more neutral name like `system.md` or `instructions.md` would lower the barrier to entry.

### 22. `base-ui` is already flagged as an open question, but it is better removed from the main stack

Section 12 lists base-ui as part of the stack, while the open questions say it is a React library and does not fit Svelte. It is better not to keep it in the table as a chosen technology.

Proposal: in the main table, replace it with `bits-ui or custom headless components`, and leave the open question about the final choice.

### 23. The name `wf` should be checked before formats are locked in

There is an open question about this, but the name is already used across all commands and paths. That is fine for a draft, but before implementation starts it is better to settle the name, because it will end up in config paths, lock files, docs, install scripts, and MCP examples.

### 24. The document mixes up "profiles," "skills," and "project skills"

One sentence should be added stating where project skills physically live and how a profile references them. Right now `.wf/profiles` is described, but a skills directory is missing from the file structure.

### 25. It needs to be specified how workflow/run artifacts are deleted

The UI has workflow deletion with a trash/confirmation, but the file-level deletion model is not described. For the current stage, the following rule is sufficient:

- workflow delete moves the folder to `.wf/trash/` or sets a tombstone;
- runs are not deleted automatically;
- versions referenced by runs cannot be physically deleted without force.

## Recommended edits to the source document

1. Add a section after "Goals/Non-goals": "Invariants of the first working version."
2. Rewrite the immutable-versions rule and either move `ui.xyflow` out of the version folder, or acknowledge a layout change as a new revision.
3. Add a state machine for run and node.
4. Add a "Minimal validator contract" as a separate list.
5. Reformulate SA autonomy via a capability/policy model.
6. Change the SA-patch rule: do not move `current` by default.
7. Add idempotency/interrupted-attempt semantics to event sourcing.
8. Tighten the default for parallel runs in a shared workdir.
9. Separate the full context from the compact context.
10. Clarify atomics for file writes, and the status of `index.jsonl` as a derived index.

## Proposed minimal set of decisions before implementation

Before the first commit, five decisions should be fixed:

1. Where the mutable UI layout lives, given that workflow versions are immutable.
2. Whether a supervisor patch moves the `current` pointer.
3. Which exact node/run statuses exist, and which events change them.
4. What happens to a `node_started` with no terminal event after a process crash.
5. Whether two write-runs are allowed in the same workdir without an explicit flag.

If these decisions are made now, the rest of the architecture will become noticeably more stable and easier to develop with TDD.
