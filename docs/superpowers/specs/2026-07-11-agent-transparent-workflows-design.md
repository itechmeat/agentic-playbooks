# Agent-transparent workflows: design

Status: design for future work, rev 3 (after two rounds of external
review). Implementation is not planned for the current cycle; the
document records the outcome of the brainstorm, the decisions made, and
the answers to review. Before implementation, the document is broken
down into sub-specs (section 13).

## 1. Goal and distribution of ownership

Shift day-to-day ownership of workflows from the user to the agent.
Today WF is a workflow editor for a human: the user sees workflows in
the UI and decides for themselves what to automate. The target picture:
the user barely thinks about workflows at all:

- the agent notices on its own that an action it just performed is
  repeatable and proposes saving it as a workflow (one question, with
  answer options);
- the agent applies existing workflows on its own when the user's task
  fits them, announcing it in one line without unnecessary questions;
- the agent manages the lifecycle on its own: modification, cloning with
  rework, deletion;
- workflows can be per-project or global, plus workflows from the user's
  other projects are available;
- none of this eats into the agent's context: brief instructions once
  per session, details lazily from MCP - following the skills model.

At the same time, ownership is distributed across layers rather than
handed entirely to the agent:

- the **user** owns policy and permissions (what can be automated, what
  to trust, where the boundaries are);
- the **agent** owns discovery and orchestration (noticing
  repeatability, matching a task, running the lifecycle);
- the **WF server** owns storage, validation, structural policy, and
  audit.

The UI and manual editing are not going away, but they stop being the
primary interface.

## 2. Principle: instructions are a UX layer, the server provides structural guarantees

WF is an MCP server; it does not see the host session (Claude Code,
opencode, etc.). The target behaviors - "noticed repeatability,"
"remembered a workflow," "proposed saving it" - are performed by the
host model, which WF programs through instructions. So the design of
instructions (what lives where, what it costs in tokens, how it survives
summarization) is a first-class part of the spec.

The boundary of guarantees is drawn honestly, based on what each side is
physically able to verify:

- **The host (agent + its UI)** is responsible for semantic
  authorization from the user's request and for showing confirmations:
  only it sees the dialogue.
- **The WF server** is responsible for structural policy and integrity:
  the allowlist of target workspaces, trust/digest/preflight, the
  immutability of the shown plan (7), the effects policy (9), rejecting
  a direct cross-workspace run that bypasses the two-phase contract, and
  audit.

The server does not see the user's request and cannot prove consent -
and the spec does not claim it does. A security-grade consent guarantee
would require a token issued by a trusted UI after the user's action and
inaccessible to the model; that is a separate integration outside the
normal MCP flow - deferred (section 14).

An honest estimate of scope: this is not "a bit of code on top of
instructions." Separating the source of definition from the place of
execution (section 3) is a refactor of wf-core, wf-engine, wf-mcp,
wf-cli, run provenance, and part of the UI. The instructions part is
cheap; the domain part is not.

## 3. Domain model

Right now in the engine, the source of definition and the place of
execution are one and the same root: `Registry` reads
`<root>/.wf/workflows`, `prepare_run` loads workflows from the same
place, locks the workdir there, creates `.wf/runs` there, and copies
scripts there. Global and cross-project workflows require splitting this
into four independent axes:

| Axis | What it describes |
| --- | --- |
| Definition origin | global / project(workspace): the directory and version of the definition |
| Applicability | requirements on the project: files, commands, repository type |
| Execution target | the workspace where commands run and the run is written |
| Trust / effects | the trust model (3.1) and the effects model (8.5) |

Minimal new concepts:

- `WorkflowRef { origin, id, version }`, where `origin` is a tagged
  union: `Global` or `Project { workspace_id }`;
- `ExecutionTarget { workspace_id, root }` - where to execute;
- `ResolvedWorkflow { definition_dir, execution_root, digest }` - the
  result of resolution before a run starts;
- `RunRef { workspace_id, run_id }` - a qualified reference to a run.

Run provenance is extended: currently `RunStarted` carries only
`workflow` and `version`; origin, source workspace, definition digest,
and execution target are added.

### 3.1 Trust model

Lifecycle and trust are independent axes; they are not mixed into a
single scale:

- **lifecycle**: `draft` / `active` / `retired` - whether the definition
  is ready for normal matching;
- **trust**: `untrusted` / `approved`, where approved is pinned to a
  specific `trusted_digest` - a local fingerprint of the approved
  version;
- **origin_kind**: `bundled` / `agent-generated` / `locally-approved` /
  `repository-provided` - where the definition came from.

Rules:

- any change to content - by the agent, by hand outside WF, or by a
  repository update - changes the digest and drops approved: prior trust
  is not inherited, because a change made outside WF proves nothing;
- `repository-provided` (a workflow that arrived with a cloned repo)
  always starts as untrusted;
- approved is granted via a trial (8.3) or explicit user confirmation and
  is recorded locally (not in the repository's files).

## 4. Instruction tiers and the structural catalog

The skills model is used as the basis: a permanent minimum stays in
context, details on demand. With corrections from review: MCP only
guarantees the presence of an optional `instructions` field - how the
host uses it is not something the MCP spec promises; and any text from
project files placed in the system prompt is a persistent prompt
injection surface that length limits do not close. So the source of
truth is structural tools, and instructions contain only our own static
text.

### Tier 0: static behavior rules (~10-15 lines)

Lives in `ServerInfo.instructions` (the hook point already exists,
`crates/wf-mcp/src/server.rs`). Only trusted static text that we wrote -
no project data:

- you have WF; workflows are saved repeatable processes;
- the rule for calling the catalog (see the contract below);
- when to propose saving one (criteria and rate limits, section 8.1);
- the format of the single question to the user (section 8.2);
- run policy: when to proceed without confirmation, when to ask (section 9);
- lifecycle (modify, clone, delete) - details in `workflow_howto`;
- about other projects - a single pointer line to `projects_list`;
- the language rule (section 11).

A compatibility spike across target hosts (Claude Code, opencode,
Hermes, Pi) remains a mandatory first step: do they read instructions,
where do they place them, do they survive compaction, how do they handle
tool approvals.

### Tier 1: the catalog via a structural tool

`workflow_catalog` - a read-only tool that returns a compact structural
list: qualified `WorkflowRef`s (including shadowed ones, with
diagnostics), trigger fields (8.5), effective effects, trust,
applicability. It is the only discovery mechanism; embedding the whole
catalog into instructions is deferred (section 14) - the catalog tool
solves the problem without the injection risk.

Call and freshness contract (otherwise proactivity stays accidental, and
the catalog would get called on every message):

- **When to call it** - a tier-0 rule: once at the start of processing a
  user task that describes an actionable action (not a clarifying reply,
  not chit-chat); again only if the task changed or after a workflow was
  created. No more than once per task.
- **Parameters**: an optional `intent` (a free-form query for a future
  FTS stage, section 10; ignored at stage 1), `workspace`, `limit` (with
  room to grow into pagination).
- **Freshness**: the response carries `catalog_revision`; the agent can
  pass it in the next call, and the server replies "unchanged" with no
  body.
- **Resilience**: one broken workflow does not bring down the catalog -
  it is skipped with a diagnostic entry; the output order is stable
  (scope, then id).

### Tier 2: details on demand

`workflow_howto` (a tool or MCP resource): YAML authoring, node types,
parameterization, the rules for trigger fields and scopes. Pulled only
when creating/reworking a workflow; it never enters context during a
normal session. Rules needed outside of authoring (e.g., "phrase the
`workflow_search` query in English") live not here but in the
description of the relevant tool.

### Fallback for clients without server instructions

A documented manual insertion of tier 0 into CLAUDE.md/AGENTS.md (no
automation: the section goes stale, and we don't touch other people's
files). Since the catalog is a tool, the fallback preserves proactive
use as well.

## 5. Scopes: storage is separate from applicability

### 5.1 Storage and trust-aware collision resolution

- Project: `<project>/.wf/workflows/` (as now).
- Global: `<config_dir>/workflows/`, where `config_dir` is the existing
  resolution `WF_CONFIG_DIR` -> `XDG_CONFIG_HOME/wf` -> `~/.config/wf`.
- No overlays - YAGNI; cloning into the project (8.4) gives the same
  result.

Unconditional "project always wins" shadowing is dangerous combined with
transparent execution: an untrusted local `project-review` from a
freshly cloned repo must not silently replace an approved global
`project-review`. Rules:

- the catalog always returns both definitions as qualified
  `WorkflowRef`s; a shadowed one is marked but visible (diagnostics);
- "project outranks global" precedence applies only among candidates
  that are active + applicable + approved;
- an untrusted/draft/invalid local definition does not hide an approved
  global one;
- if a bare id that previously resolved to one origin starts resolving
  to a different one, a silent run is forbidden - only an explicit
  confirmation;
- a collision between two approved definitions is an ambiguous match (a
  short question) or an explicit user rule in the config.

### 5.2 Applicability is not a property of scope

Scope means only the storage location and visibility. The requirement
"a global workflow must be executable in any project" is unverifiable,
so applicability is a separate declarative field `requires` (presence of
files/commands, repository type, etc.) with a server-side preflight
before running: mismatches are returned to the agent as a list before
the start. Validation of the global scope is limited to what is
checkable: no absolute paths, parameterization instead of hardcoded
project-specific values. Scope-recommendation heuristic for the question
to the user: an action tied to project specifics -> project; a universal
process -> global.

## 6. Workspace registry

### 6.1 Identity: workspace, not "project"

A single git repository can live in several checkouts (clones,
worktrees), so a single id in `.wf/` that gets committed to git cannot
be an identity - clones would collide. Identity is two-tiered:

- `workspace_id` - local, generated on the spot, and not committed to
  git;
- `repository_fingerprint` - optional (e.g. by remote/initial commit),
  links workspaces belonging to the same repository.

The registry (`<config_dir>/projects.json`) is keyed by `workspace_id`
and supports several workspaces of the same repository. "The project was
moved" remains a non-event: a familiar `workspace_id` at a new path ->
the record updates the path in place.

### 6.2 Auto-registration: best-effort and silent

Any run of wf inside a workspace updates the entry: workspace_id,
fingerprint, path, name, last-seen, workflow count. There is no manual
registration; `wf projects` is for viewing and manual removal. Hygiene:

- registration is best-effort: an unavailable or corrupt global config
  never breaks or slows down the command itself;
- opt-out: a config flag and an environment variable disable
  registration;
- filtering out noise: CI environments (standard CI env vars), temp
  directories - not registered.

### 6.3 Concurrency and format

The registry is written concurrently by several processes (CLI, MCP,
server), so: a file lock + atomic write via rename (or SQLite, if the
registry merges with the FTS index of section 10), a `schema_version`
field, 0600 permissions, recovery from a corrupt file - recreate empty
with a warning (data is recoverable via auto-registration).

### 6.4 Unreachable workspaces (404)

`active` -> `unreachable` -> `tombstoned`.

- Checks are lazy only, on actual access. There are no background
  cleaners.
- Access failed (path missing / no `.wf/`) -> `unreachable` (timestamp of
  the first failure). The tool returns a structural error with the state
  to the agent; the agent tells the user in one line and continues
  without that workspace.
- Transition to `tombstoned` is time-based only: T days in unreachable
  (default 14), with backoff on repeat checks. The number of failed
  accesses does not speed up the transition: three calls in a row while
  an external disk is temporarily unplugged is not a reason to bury the
  workspace.
- Tombstoned entries are not shown in `projects_list`; resurrection is
  automatic only - wf is run again in the workspace with that id ->
  `active`.
- File hygiene: tombstoned entries are physically purged after a long
  period (default 90 days). Thresholds are global-config defaults.

## 7. Cross-project behavior

Scenario: from the current project's session, do something in a related
project.

- Discovery is lazy: the agent figures out from the prompt that the task
  is about another project -> `projects_list` -> `workflow_catalog
  (workspace=X)`. No preloaded lists of other projects sit in context.
- There is no explicit "project relatedness" mechanism: the set of
  projects changes, and accumulated links turn into noise.
- Execution: the engine works with the target workspace's root; the run
  and its events are written into its `.wf/` and are visible in its UI.
- **Qualified references across the whole surface.** Right now `WfMcp`
  holds one fixed root, and a supervisor session holds only a run_id;
  after a cross-project run, `run_status`, `run_events`, `review_decide`,
  supervisor tools, and UI links won't find the run. All run operations
  are moved to `RunRef { workspace_id, run_id }` (defaulting to "the
  current workspace" for backward compatibility), including supervisor
  sessions and the disk fallback.
- **Two-phase server contract.** `workflow_prepare_run` resolves the
  workflow, returns the target, version/digest, parameters, effective
  effects, preflight results, and a `plan_token`; after user
  confirmation, the agent calls
  `workflow_execute_plan(plan_token)`. A direct cross-workspace
  `workflow_run` that bypasses the contract is rejected by the server.
- **The plan_token contract**: a stateless signed token (HMAC with a
  server key) over target + WorkflowRef + digest + parameters +
  effective effects + expiry + nonce; a short TTL (minutes); single-use
  via an in-memory registry of used nonces (there are no durable
  mutations, so `workflow_prepare_run` stays read-only; the audit record
  is written by `workflow_execute_plan`); bound to the server session;
  at execution time a repeat preflight is run - if the target's state
  changed the outcome, the plan is rejected. What the token guarantees:
  no drift between the plan shown to the user and what is executed
  (TOCTOU), a forced preview step, and audit. What it does not
  guarantee: that the user was actually asked - semantic consent remains
  with the host (section 2). Paths are canonicalized and checked against
  the registry; MCP roots are not treated as a security boundary.
- User confirmation before a cross-project run is always mandatory
  (section 9).

## 8. Agent-driven workflow lifecycle

### 8.1 Detecting repeatability

Criteria - text in tier 0:

- the action was multi-step and looks reproducible (not a one-off fix);
- a similar workflow doesn't already exist (the agent checks the
  catalog; the server additionally checks for duplicates at capture
  time);
- the user hasn't already dismissed the same proposal before (the
  dismiss store, 8.2);
- no more than one proposal per reasonable interval within a session.

Rate limits are a first-class part of the design: an agent that proposes
a workflow after every little thing will kill trust in the feature
faster than not having it at all.

### 8.2 A single question, and dismiss

The format is standardized in tier 0: exactly one question. The agent
marks and puts first the recommended option - the marker is movable, not
nailed to "global" (the agent computes the recommendation via the scope
heuristic from 5.2). Example for the case where the project scope is
recommended:

> Want me to make this action repeatable (a workflow)?
> 1. Yes, in the current project (recommended)
> 2. Yes, globally
> 3. Not now
> 4. No, and don't suggest this again
> 5. Custom answer

"Not now" and "never" are handled separately: the first records
nothing, the second is written via the `suggestion_dismiss` tool into
the dismiss store - one entry per proposal pattern, with a
TTL/reset via config. This is designed from scratch: the dismiss
mechanism from the remote feature isn't implemented itself yet (only
planned), and its single boolean wouldn't fit semantic suppression of
many different proposals anyway.

### 8.3 Creation: `workflow_capture` and the lifecycle

A one-off retelling mistake must not turn into permanent automation, so
capture creates a draft, not an active object.

- On "yes," the agent calls `workflow_capture` with a summary (steps,
  parameters, inputs/outputs, a draft of the trigger fields) and
  `selected_scope` - what the user actually chose, not the recommendation.
  The result is a **draft** (untrusted), which does not enter normal
  matching.
- **Secrets are a contract, not a post-hoc filter.** By the time the
  server checks it, a value has already passed through a tool call and
  potential logs, so the boundary is on the host side: a tier-0 rule -
  secret values are never included in the summary; instead use
  SecretRef / env- and config-key names / placeholders. Server-side
  scanning of the summary for secret-looking strings is an additional
  heuristic (reject and ask for a replacement), not a guarantee.
  Provenance does not store the original, unredacted summary.
- The server checks for duplicates before writing (via the catalog) and
  requires parameterization of concrete values.
- Turning the summary into YAML is itself a meta-workflow, "create a
  workflow from a summary" (an agent node + validation). Dogfooding:
  creating a workflow is itself a workflow. The host agent does not pull
  YAML authoring rules into context. Stage v1 (until the meta-workflow
  exists): the agent pulls `workflow_howto` itself and writes the YAML;
  the `workflow_capture` interface is designed up front so the upgrade
  doesn't change host behavior.
- **A trial across the effects matrix**, not just a worktree (a worktree
  rolls back files, but not an email, a deploy, or an API call):
  - filesystem -> worktree + show the diff to the user;
  - network / external -> dry-run or mock, otherwise an explicit
    confirmation for every run until approved;
  - secrets -> only via SecretRef;
  - irreversible effects -> trial is forbidden, only explicit
    confirmation.
- A successful trial (or explicit user confirmation) moves the draft to
  `active` + `approved` with a `trusted_digest` recorded (3.1).
- A generated workflow stores provenance: who created it, from which
  (redacted) summary, by which meta-workflow/model.

### 8.4 Modification, cloning, deletion

A line in tier 0 plus tools: `workflow_update` (a new minor version; the
digest changes - approved is dropped until reconfirmed, 3.1),
`workflow_delete` (soft, into trash). Cloning with rework into a new id
is designed explicitly: today's `workflow_create` has no `basis`
parameter (its arguments are only id and yaml, base_version is always
None), so a `basis: WorkflowRef` is added, with a provenance record
"cloned from." The agent recognizes the user's intent ("remove step X
from the review," "make a staging variant") and performs the operations
itself, without the user being involved in the mechanics.

### 8.5 Structured triggers and the effects model

A free-form description can neither be reliably validated nor safely
displayed. Matching is built on structural fields of the definition:

- `trigger.when` - canonical statements of "when to apply";
- `trigger.avoid_when` - when not to apply;
- `trigger.examples` - examples of user requests;
- `requires` - applicability (5.2);
- `effects` - see below.

Fields are bounded in length and format, validated on read. Display
name and human-readable description remain free-form but do not enter
matching.

**Effects are not self-declared.** What the author claims cannot be
taken on faith: a workflow may declare `filesystem: read` and actually
write and reach the network. Three concepts:

- `declared_effects` - stated by the author;
- `inferred_effects` - conservatively derived by the server from node
  types and runners; an arbitrary script- or agent-runner gets a
  pessimistic filesystem + network + external/unknown;
- `effective_effects` - the union; this is what policy (9), the catalog,
  and prepare_run actually use.

A declaration can only extend the inferred set (add what the server
didn't see), never narrow it; understating it is a validation error, at
minimum a block on transparent execution. Provably reducing effects will
only be possible with real sandbox/capability enforcement in the engine
- which does not exist yet, and the spec acknowledges that.

## 9. Run policy: match confidence is separate from risk

Two independent quantities: match confidence (how well the task
resembles the trigger) and execution risk (effective effects + trust).
Match confidence by itself is not permission. MCP annotations
(readOnlyHint/destructiveHint) remain hints to the client, not
enforcement - policy is checked by the server against effective effects
and trust.

Principle of authorization by direct request:

> The user is not confirming the way an already-requested action is
> carried out; they are only confirming an expansion of intent or blast
> radius. Authorization by direct request extends only to effects that
> are necessary for, or clearly implied by, that request; unexpected and
> external effects require separate permission.

| Situation | Behavior |
| --- | --- |
| Direct request + confident match + active/approved workflow of the current project or global + effects within the scope of the request | One line, "Found workflow X, running it (matched Y)," and run without asking |
| Same, but effective effects exceed what the request implies (external, secrets, irreversible, unexpected network) | A short confirmation naming the extra effect |
| Unconfident match (several candidates, ambiguous shadowing) | One short question |
| Agent initiative (the user did not directly request this action) | Always confirm |
| Draft / untrusted (including repository-provided without approval) | Only via the trial procedure (8.3) |
| Another workspace | Always confirm + the two-phase contract (7) |

Examples of effects boundaries (to become request -> allowed/not-allowed
pairs in a policy sub-spec): "do a review" authorizes reading and a
report, but not auto-fixes, commits, and pushes; "prepare a release"
does not mean "publish the release"; "update the changelog" does not
authorize sending a Slack notification.

Trust is pinned to `trusted_digest` (3.1): any change to the definition
- including a manual edit outside WF and a repository update - drops
approved. Separate modes `auto_safe` (auto-run only for provably
read-only or isolated workflows) and `trusted_auto` (an explicit
per-digest user opt-in) are a deferred layer (section 14): the current
matrix is sufficient while run isolation remains declarative and is not
enforced by the engine.

Every auto-run is accompanied by one line explaining the match ("chose
X: matched Y") - a cheap defense against silent false positives.

## 10. Retrieval: an escalation ladder

The corpus of workflows is small (tens, at most hundreds of compact
records), so an LLM ranks, not a search engine. Stages are added only
once the previous one demonstrably breaks, without requiring a rewrite
of it:

1. **Now: LLM-as-matcher.** `projects_list` + `workflow_catalog`; the
   host model ranks on its own. Multilingual support comes for free: an
   LLM reading the user's prompt is the strongest semantic matcher in
   the system.
2. **Escalation: `workflow_search` on SQLite FTS5** over trigger fields
   and tags across all registry workspaces. Multilingual support is
   handled by the caller: the rule "phrase the query in English" lives
   in the tool's own description (canonical trigger fields are English,
   section 11). No hardcoded linguistics in the code. Triggers for
   adoption are quality signals: dropping top-1 accuracy, rising
   ambiguous matches, catalog token cost, latency; a size benchmark
   (~300 workflows) is secondary. So these signals have somewhere to come
   from, minimal local matching telemetry (anonymous aggregates: chosen
   candidate, override, dismiss, failed auto-run - no raw prompts) and a
   small offline evaluation corpus are part of the policy sub-spec, not
   deferred.
3. **Deferred: vector search.** There is no room for it anywhere in the
   current scope. The only future candidate is "run-experience memory"
   (section 14), where vectors are an implementation detail of a
   separate feature.

## 11. Language policy

- Machine-facing fields are English: id, tags, canonical `trigger.when` /
  `avoid_when`. This is a condition for the FTS stage to work and for
  matching consistency, independent of the project's authoring language.
- Display name, human-readable descriptions, and node prompts - in
  whatever language the project uses.
- All communication with the user about workflows (proposing to save
  one, announcing a run, questions) - in the language of the user's
  latest chat messages, determined by the agent at runtime. The language
  is not hardcoded.

## 12. MCP surface changes (summary)

New tools:

| Tool | Purpose | Annotation |
| --- | --- | --- |
| `workflow_catalog` | Structural catalog: WorkflowRef, trigger, effective effects, trust, applicability, shadowing diagnostics; catalog_revision | read-only |
| `projects_list` | Workspace registry: id, name, path, last-seen, state | read-only |
| `workflow_capture` | Accept a summary + selected_scope, create a draft via the meta-workflow | destructive |
| `workflow_prepare_run` | Resolve + preflight + stateless plan_token; no durable mutations | read-only |
| `workflow_execute_plan` | Execute a confirmed plan, write the audit record | destructive |
| `workflow_howto` | Tier 2: authoring, trigger and scope rules | read-only |
| `suggestion_dismiss` | Record "don't suggest this" with a TTL | destructive |

Changed:

- `workflow_list` is not broken: the compact catalog is the new
  `workflow_catalog`; the current summary format (name, description,
  current, versions) is kept for existing clients.
- Run operations (`run_status`, `run_events`, `run_report`, `run_resume`,
  `review_decide`, supervisor tools) - accept `RunRef` defaulting to
  "the current workspace."
- `workflow_run` - `WorkflowRef`; a cross-workspace call that bypasses
  the two-phase contract is rejected by the server.
- `workflow_get`, `workflow_validate` - `WorkflowRef` / a workspace
  parameter.
- `workflow_create` - `scope` and `basis` (cloning) parameters, global
  scope validation.
- `ServerInfo.instructions` - the static tier 0.

## 13. Decomposition before implementation

Product-owner decision: instead of five sub-specs, the work is planned
as a single plan,
`docs/superpowers/plans/2026-07-11-agent-transparent-workflows-plan.md`,
preserving the implementation order of this section. The list of areas
below remains a map of responsibility within the plan:

1. **Host integration contract** - tier 0, the host compatibility
   matrix, behavior under compaction, the fallback, the catalog call
   contract.
2. **Workflow addressing and stores** - WorkflowRef, global/project
   storage, the resolver, trust-aware shadowing, provenance, digest.
3. **Project catalog and cross-project authorization** - the registry,
   workspace identity, 404, qualified references, the two-phase
   contract and plan_token.
4. **Capture lifecycle** - the summary schema, the secrets contract,
   draft, deduplication, the trial matrix, activation, dismiss, the
   meta-workflow.
5. **Autonomy and safety policy** - triggers, the effects model
   (declared/inferred/effective), run policy with boundary examples,
   trust, match explanation, minimal telemetry and the eval corpus.

Implementation order (each step stands on its own; the policy gate sits
before the first transparent run):

1. Compatibility spike for server instructions across target hosts.
2. Split definition origin from execution target in the core.
3. Structured triggers + the effects model + the read-only
   `workflow_catalog` (matching works, without executing anything).
4. Run policy (the matrix in section 9, the effects boundary, match
   explanation, minimal telemetry) - a gate before any transparent run.
5. `workflow_capture`: draft without activation.
6. Trial infrastructure (worktree + diff, dry-run) and activation.
7. Global scope with trust-aware shadowing.
8. Registry and cross-project reading.
9. Cross-project execution with the two-phase contract.
10. Deferred autonomy modes (auto_safe/trusted_auto) - only after
    everything above has been proven out.

## 14. Deferred

- Overlaying global workflows with project-local patches - cloning
  covers this.
- Promotion project -> global (and back) based on observed usage.
- Scope selection when manually creating a workflow in the UI editor.
- Embedding the catalog into server instructions as a per-host
  optimization - only for locally approved digests, without the
  examples fields, after a separate injection-risk assessment; the
  catalog tool makes this unnecessary for correctness.
- Security-grade consent confirmation: a token issued by a trusted
  UI/host after the user's action and inaccessible to the model
  (section 2). A separate integration outside the normal MCP flow.
- `auto_safe` / `trusted_auto` modes (section 9) - after real
  isolation/sandboxing appears in the engine (which is also the only way
  to provably narrow effects, 8.5).
- Full privacy-safe analytics for the feature on top of the minimal
  telemetry from section 10.
- "Run-experience memory": before running a node, feed the agent a
  digest of similar past failures and decisions. The only honest use
  case for vector search. A separate feature with its own brainstorm.
- Product name: the current "Workflows" is the alpha's technical name;
  choosing a new one is a separate discussion.

## 15. Decisions made (traceability)

Brainstorm:

| Question | Decision |
| --- | --- |
| Where tiers 0/1 live | Tier 0 - static instructions; tier 1 - the structural `workflow_catalog`; embedding the catalog into instructions is deferred |
| How workflows get created | `workflow_capture` -> draft -> trial across the effects matrix -> active/approved, a meta-workflow; the v1 stage - the agent writes the YAML itself via tier 2 |
| Scopes | `~/.config/wf/workflows/` + trust-aware shadowing; applicability is a separate `requires` |
| Registry | Auto-registration only; identity is workspace_id + repo fingerprint; 404 based on time (14/90 days) |
| Source priority | Current project -> global -> other workspaces (lazily, by pointer) |
| Project relatedness | Not doing it: a changing set of projects turns links into noise |
| Cross-project | Root and history of the target workspace; always confirm + the two-phase contract |
| Retrieval | LLM-as-matcher -> FTS5 (based on quality signals, with minimal telemetry) -> vectors deferred |
| Language | Machine fields English, display free-form, outbound in the chat's language |

Response to rev-1 review (briefly; details in git history):

- Accepted: separating confidence/risk, the domain model and an honest
  scope estimate, the catalog instead of injecting into instructions,
  RunRef and the two-phase contract, the capture lifecycle, scope is not
  applicability, workspace identity, registry hygiene, structured
  triggers, `workflow_catalog` without breaking `workflow_list`, FTS
  based on quality signals, the language policy.
- Rejected: confirm-by-default on a direct user request (the request
  itself is the authorization; the reviewer agreed in round 2).

Response to rev-2 review:

| Review comment | Decision |
| --- | --- |
| Contradiction between "the server provides authorization" and "the token doesn't prove consent" | Accepted: the boundary is reworded (section 2) - the host owns semantic consent, the server owns structural policy; a security-grade consent token goes into deferred |
| A direct request authorizes the goal, but not hidden effects | Accepted: an effects boundary in the run-policy matrix, the principle "effects within what the request implies," example pairs go into the sub-spec |
| Effects are self-declared | Accepted: declared / inferred / effective, a declaration can only extend, understating is an error; provable narrowing is only a future sandbox |
| Unconditional shadowing can replace a trusted workflow | Accepted: precedence only among active+applicable+approved, untrusted does not hide approved, an origin change forbids a silent run, diagnostics |
| Remove tier 1 from instructions entirely | Accepted: the catalog is tool-only; embedding it goes into deferred, with conditions |
| Formally separate active and trust | Accepted: lifecycle x trust x trusted_digest x origin_kind (3.1); any digest change drops approved, including edits made outside WF |
| A worktree does not roll back external effects | Accepted: a trial matrix by effect type; irreversible - trial forbidden |
| Secrets can't be caught after the fact | Accepted: the SecretRef contract on the host side, server-side scanning is a heuristic, provenance is redacted |
| The plan_token contract and read-only prepare | Accepted: a stateless signed token, TTL, single-use via in-memory nonce, re-preflight at execution; no durable mutations in prepare - read-only is correct |
| The catalog call and freshness contract | Accepted: call rules in tier 0, intent/limit, catalog_revision, resilience to broken definitions, stable order |
| Minor: movable recommendation marker, selected_scope, workspace_id in WorkflowRef, the English rule in the tool description, telemetry earlier, implementation order | All accepted: the question with a movable marker (8.2), selected_scope (8.3), the tagged union origin (3), the rule in the `workflow_search` description (10), minimal telemetry in the policy sub-spec (10), the policy gate and the draft/trial split in the order (13) |
</content>
