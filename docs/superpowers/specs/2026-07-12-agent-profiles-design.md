# Agent profiles, environment detection, and model table - design

Date: 2026-07-12. Status: rev 3 (after the second external review: skill
bundle-trust, identity snapshots, invocation-environment pinning, the
digest procedure, CAS storage of profiles, copy semantics for moves, and
legacy surfaces).

This spec builds on subsystems implemented earlier: workflow scopes and
trust (spec 2026-07-11-agent-transparent-workflows), the digest model, the
policy gate, and the MCP tiers. Term UA - User Agent: the host LLM agent
(Claude Code, codex, etc.) that drives workflows through the wf MCP server.

## 1. Goal

The profile becomes the single binding point between an executor and a
node: a node references only a profile, and the profile encapsulates the
agent, model, fallbacks, the role system prompt (SOUL.md), and a set of
skills. The UA manages profiles as dynamically as workflows: it creates a
missing profile on the fly while building a workflow, edits profiles, and
moves them between scopes. So the UA can choose executors meaningfully, the
CLI detects installed CLI agents and their models for free and ships a
curated advisory model table with purposes and subscriptions.

Key properties:

- detection and validation never call models; wf itself makes no network
  requests (promise boundaries - 7.5);
- the model table is strictly advisory - no hard coupling to detection or
  execution; the UA does the matching;
- portability: workflows contain no agent/model, only profile references.
  A profile holds a machine-dependent binding (agent/model), so a workflow
  is syntactically portable and the environment is repaired by the
  adoption flow (5.2): on a new machine the UA rebinds executors via a
  standard operation. Splitting the profile into a portable role and a
  local binding is a possible evolution (13); for now it is deliberately a
  single object.

## 2. Summary of decisions

| # | Question | Decision |
|---|--------|---------|
| 1 | Fate of executors | The profile absorbs the executor wholesale; executors disappear from the workflow schema, global config, and overrides |
| 2 | Profile scopes | project + global; a typed reference carrying scope; a move = copy + a separate deletion of the source (4.2) |
| 3 | Model table | Two normalized blocks: model facts and purposes (purpose -> a ranked list with scores 1-10) |
| 4 | Table delivery | Compiled into the binary from the wf git repository + a mergeable user overlay |
| 5 | Coupling the table to execution | None; the table is a hint, the source of truth for what runs is detection, and the UA does matching |
| 6 | Trust | Bundle-digest: the profile plus its skills are trusted as a bundle (5.1); an adoption flow for anything brought in from outside |
| 7 | Wiring depth | Full: the executor part + SOUL + skills at once |
| 8 | Skill mechanics | Canonical location `.agents/skills/`; a symlink bridge for claude; materialize the set as copies in an isolated workdir, otherwise an advisory string of names |
| 9 | Profile versioning | No user-level versioning (history is git); internal snapshots in a run are mandatory (3.6) |

Additionally: user subscriptions are a declared store with an onboarding
prompt; auth hints in detection; CLI-agent categories (vendor / aggregator)
are a descriptive property of detection.

## 3. Domain model

Central types: `QualifiedProfileRef` (how it is referenced),
`ResolvedProfileSnapshot` (what was chosen and pinned),
`ResolvedInvocation` (what and how it will be executed),
`RunExecutionManifest` (what the run is obligated to remember).

### 3.1. Profile: contents and digest

```
profiles/<name>/
|- profile.yaml
`- SOUL.md
```

```yaml
# profile.yaml
name: architect
description: Project architect, planning and decomposition
executor:
  agent: claude                    # agent id from detection/global config
  model: claude-opus-4-8           # a string exactly in this agent's --model format
  fallbacks:
    - { agent: opencode, model: opencode/claude-opus-4-8 }
soul: any                          # SoulRequirement: any | native_required (6.3)
skills:                            # a string or {name, scope} (6.4)
  - coding-standards
  - { name: writing-plans, scope: global }
```

Naming rules: `name` in profile.yaml must match the directory name;
valid names are `[a-z0-9][a-z0-9-]*`, at most 64 characters; collisions
after case-folding (Reviewer/reviewer) are forbidden by the validator -
this is for portability between case-sensitive and case-insensitive
filesystems.

Digest levels:

- `profile_digest` = `sha256:<hex>` of the canonical concatenation of
  `profile.yaml` + `\0` + `SOUL.md` (a missing SOUL.md is equivalent to
  an empty one);
- `skill_digest` - the skill's content digest per procedure 3.5;
- `bundle_digest` = sha256 of the domain tag + `profile_digest` +
  a sorted list of pairs (qualified skill ref, skill_digest) in
  length-prefixed encoding.

The trust object is the `bundle_digest` (5.1): editing any skill changes
the bundle and drops trust; there are no separate trust records for
individual skills.

`SOUL.md` is the role's system prompt, free-form markdown. Fallback
semantics: a fallback is the same role, a different executor; SOUL and
skills are preserved, the agent+model pair changes (the SOUL delivery
method may degrade - 6.3). There are no inline profiles in a workflow:
only a reference.

### 3.2. QualifiedProfileRef and schema changes (schema: 2)

Removed: `executors:`, `defaults.executor`, `supervisor.executor`, the
node-level `executor` field, and the previous decorative `profile` field.

Added (the same reference shape everywhere):

```yaml
defaults:
  profile: architect               # shortcut: scope: auto
nodes:
  - id: plan
    type: agent_task
    profile: { name: architect, scope: global }   # full form
supervisor:
  profile: reviewer
```

`scope: project | global | auto` (default auto). A reference is a string
(shortcut for `{name: <s>, scope: auto}`) or an object.

### 3.3. Resolution: deterministic choice, then trust

The resolver receives the workflow's origin and works as follows:

1. Candidates by reference scope:
   - `project` - only `<root>/.wf/profiles/<name>`;
   - `global` - only `<config_dir>/profiles/<name>`;
   - `auto` - for a workflow of project origin: project, then global; for
     a workflow of global origin: only global (project profiles are not
     considered at all).
2. The choice is deterministic based on filesystem content and does not
   depend on trust: the first existing candidate wins. There is no
   ambiguity mechanism for profiles.
3. Trust is checked AFTER selection (5.1): an untrusted selected bundle
   results in a refusal with a code and a path to acknowledge, not a
   change in the resolution result.

A single run can legitimately use two different profiles with the same
name from different scopes (`{reviewer, project}` and `{reviewer,
global}`), so a profile's identity on ALL surfaces is `(scope, name)`
plus digest, not a bare name (3.6).

### 3.4. ResolvedProfileSnapshot, ResolvedSkill, ResolvedInvocation

The result of resolving each profile at the start of a run:

```
ResolvedProfileSnapshot {
  ref: { scope, name },           # the actual scope after auto
  chain: Vec<ResolvedInvocation>, # primary + fallbacks, already filtered (6.3)
  soul: String,
  soul_requirement,               # any | native_required
  skills: Vec<ResolvedSkill>,
  profile_digest, bundle_digest,
}
ResolvedSkill {
  name, scope,                    # the skill's actual scope (6.4)
  canonical_path,
  digest,
}
ResolvedInvocation {
  agent_id, model,
  invocation_spec,                # argv template, prompt_via, transport (6.2)
  soul_delivery,                  # native | prefix - capability at resolution time
  canonical_executable,
  executable_fingerprint,         # size + mtime of the canonical binary
}
```

The chain pins not just "who" but also "with what and how": a change to
the AgentDef, the argv template, a capability, or the binary itself
between start and resume cannot silently change the prompt contract
(3.6). A resolution error for any profile, skill, or invocation is a
refusal before the run spawns.

### 3.5. Snapshot procedure and content digest

The contract for skills and for the profile.yaml/SOUL.md pair is
identical and TOCTOU-resistant: copy first, then digest the copy.

1. The content is copied into the run snapshot's staging directory.
2. The digest is computed OVER THE COPY: domain tag + for each file a
   length-prefixed (relative path, type, bytes), paths sorted.
3. The resulting digest is checked against the expected one (verified at
   the policy gate); a mismatch is a refusal, and staging is removed.
4. Only after a match is staging published into the snapshot. The live
   tree is not read afterward.

Content constraints (a violation is a refusal with code
`skill_unsupported`):

- only regular files and directories; FIFO/device/socket are rejected;
- a symlink is allowed only if, after canonicalization, it stays inside
  the skill's root; it is hashed as a link (type + target); escaping
  outside is a refusal (`skill_escape`); cycles are detected via a depth
  limit;
- limits: total size, file count, depth, single-file size (constants are
  fixed in the plan; exceeding them is a refusal `skill_too_large`).

### 3.6. RunExecutionManifest

The engine already snapshots workflow.yaml and scripts into the run
directory. Profiles, skills, and invocations are pinned the same way:

```
runs/<id>/
|- workflow.yaml                       # already exists
|- scripts/                            # already exists
|- manifest.yaml                       # RunExecutionManifest
`- profiles/<scope>/<name>/            # copies of profile.yaml, SOUL.md
   `- skills/<skill>/                  # copies of skill content
```

`manifest.yaml` is immutable, written once at start: for each profile -
the qualified ref, profile_digest, bundle_digest, the list of skills with
digest, the full `ResolvedInvocation` chain (including the soul_delivery
of every element). Attempt events reference the manifest and record the
element of the chain actually used - the manifest is not mutated over
the course of the run.

All reads of a profile/SOUL/skills after start (retry, fallback, resume,
human_review, an MCP server restart) come from the snapshot. On resume,
each chain element's executable_fingerprint is checked against the
current binary: a mismatch is a stop with code `environment_drift`;
continuing is only possible via an explicit `resume
--allow-environment-drift`, and the fact is written to events. There is
no silent switch to a new binary.

This is NOT user-level versioning: the snapshot is an internal run
mechanic, like the copy of workflow.yaml.

Cross-workspace plan: `PlanPayload` gains a `profiles: [{scope, name,
bundle_digest}]` field - drift of a profile or skill between prepare and
execute breaks the plan the same way workflow digest drift does.

## 4. Scopes and transfer

### 4.1. Layout

- project: `<root>/.wf/profiles/<name>/`;
- global: `<config_dir>/profiles/<name>/`.

Shadowing is unambiguous and trust-independent (3.3).

### 4.2. Transfer: copy by default, deletion is separate

`profile_move` performs a copy in BOTH directions; the source remains.
Reasons: a global profile may be referenced by workflows of any project
on the machine (no full registry of references exists), and a project
profile may be referenced by historical versions of this project's
workflows with an explicit `scope: project`, which cannot be silently
rewritten.

Deleting the source is a separate, explicit operation (`profile_delete`)
with a reference scan:

- project profile: ALL versions of all of the project's workflows are
  scanned; explicit `scope: project` references, and auto-references
  that would stop resolving (no same-named global profile), block the
  deletion with a list. Output option: propose creating new workflow
  versions with updated references (history is not rewritten).
- global profile: a best-effort scan - global workflows + projects from
  the workspace registry, with an explicit note about unverifiable
  projects; if references are found, `force: true` is required.

Common guarantees:

1. A name conflict in the target scope is a refusal, no overwriting.
2. Trust: the bundle_digest from copying does not change if the skills
   resolve to the same content; when moving project -> global with
   references to project skills - a refusal with options (move the
   skills, remove them from the list, keep the profile as project-scoped).
   After a successful move, the bundle is re-checked and, if the skills
   now resolve differently, the target copy is honestly untrusted.
3. Publication - staging + journal + recovery (9.1); cross-device
   copying is crash-consistent (copy -> fsync -> publish); atomicity is
   NOT promised, but the absence of a state with "neither source nor
   copy" is.

## 5. Trust and adoption

### 5.1. Trust rails: bundle

The same TrustStore (trust.json). Entry: `bundle_digest`, `kind: workflow
| profile_bundle`, name - for auditing.

- Snapshot and trust solve different problems: the snapshot prevents
  drift AFTER start and enables resume; trust authorizes specific
  content BEFORE the first spawn.
- The trust object is the bundle (3.1): a profile plus the actual content
  of its skills. Editing `SKILL.md` in an already-accepted repository
  changes the bundle - the next run requires `acknowledge_untrusted`,
  even if the workflow and profile digests have not changed. An approved
  profile does not authorize future content of skill directories.
- A profile created or changed via an MCP tool is auto-approved as a
  bundle at the moment it is written (authorization boundaries - 9.4).
- The gate in `check_run`: after resolution, the bundle_digest of all
  selected profiles is collected (nodes + supervisor). Even one
  untrusted one causes a refusal
  `untrusted_profile_requires_acknowledge` with a list of qualified
  refs. `acknowledge_untrusted: true` covers both the workflow and the
  profiles.
- Anti-TOCTOU: digests are checked against the copy at snapshot time
  (3.5); after the snapshot, sources are not read.

### 5.2. Adoption flow

An incoming workflow or profile from elsewhere (git clone, copying) is a
normal, expected scenario at the user's initiative. For it there is a
free local report: CLI `wf adopt`, MCP `workflow_adopt_report`. It
gathers:

1. Resolution: do the referenced profiles exist? do their skills exist?
2. Environment: is each chain's agent installed (detection)? is the
   model available to the agent - only if the agent's inventory is
   authoritative; for partial / display / static inventories, a missing
   model is `model_unverifiable`, not `model_not_available`.
3. Trust: which bundles/digests are not approved.

Codes: `profile_missing`, `skill_missing`, `skill_unsupported`,
`skill_escape`, `agent_not_installed`, `model_not_available`,
`model_unverifiable`, `untrusted`. Each comes with data for a fix. The
UA shows this to the user in plain language and proposes fixes using
ordinary tools. "Fixed -> approved -> works" is the normal outcome. The
environment part reuses and extends `wf doctor`.

Not done: partial trust, network checks.

## 6. Wiring into execution

### 6.1. Executor part

`executor::resolve` is replaced by profile resolution:
`ResolvedProfileSnapshot.chain` is a chain of `ResolvedInvocation`; the
scheduler's retry and fallback logic does not change. The
`fallback_triggered` event gains the profile's qualified ref.

Context compaction (`context_compact_model`) is an explicit, documented
exception to the "the executor is set by a profile" rule: it is an
internal engine service call (compacting the log with a cheap model),
not a workflow node. It remains a model string; a possible replacement
with `context_compact_profile` is an evolution (13).

### 6.2. Per-agent invocation shape: AgentDef.invocation

The single `ClaudeAdapter` hardcodes the shape `<bin> -p <prompt>
--model <m>`. A declarative invocation schema is introduced - data, not
classes. Built-in defaults for the five agents, and the same schema for
custom agents in the global config's `agents:`:

```yaml
agents:
  mycli:
    program: mycli
    invocation:
      argv: [run, "{prompt}", --model, "{model}"]   # typed placeholders
      prompt_via: argv          # argv | stdin (with stdin, the {prompt} slot in argv is forbidden)
      soul: prefix              # native | prefix; native requires soul_flag
      # soul_flag: --system     # only when soul: native
      transport: headless       # headless | acp
```

Invariants checked at config load time: exactly one prompt slot (an argv
placeholder or stdin); `{model}` occurs at most once; placeholders are
only whole argv elements (no substitution in the middle of a string and
no shell); `soul: native` requires `soul_flag`. The built-in five:

| agent | invocation (headless) | model | SoulDelivery capability |
|-------|----------------------|--------|-------------------------|
| claude | `claude -p {prompt}` | `--model {m}` | native (`--append-system-prompt`) |
| agy | `agy -p {prompt}` | `--model {m}` | prefix |
| codex | `codex exec {prompt}` | `-m {m}` | prefix |
| opencode | `opencode run {prompt}` | `-m {provider/model}` | prefix |
| pi | to be clarified when it appears | - | - |

Shapes are confirmed from help output; details (exit codes, streaming
modes) are verified in the implementation plan.

### 6.3. SOUL: requirement and delivery are different types

```
SoulRequirement = any | native_required      # a profile property (field soul)
SoulDelivery    = native | prefix            # invocation capability
```

- `native` - the agent's system channel (claude: `--append-system-prompt`);
- `prefix` - SOUL prefixed to the node's prompt with an explicit role
  separator. This is NOT a system prompt: its priority equals the node's
  task, and its resistance to override is weaker.

`native_required` filters the chain at resolution time: elements with
`soul_delivery: prefix` are excluded; an empty chain is a resolution
error. If SOUL is absent or empty, filtering does not apply - there is
nothing to deliver, so the requirement is moot.

The manifest stores the soul_delivery of every element of the filtered
chain (immutable); each attempt's events record the delivery of the
element actually used. A fallback from a native element to a prefix
element is visible in events, not silent.

### 6.4. Skills

Canonical directories (the open Agent Skills standard,
`<name>/SKILL.md`):

- project: `<root>/.agents/skills/<name>/` (overridden by the project
  config's `skills_dir`);
- global: `~/.agents/skills/<name>/` - the ecosystem's standard location;
  wf does not introduce its own global skills directory.

Skill resolution is deterministic and mirrors profiles:

- a skill reference is a string (shortcut for `scope: auto`) or
  `{name, scope}`;
- a global profile sees ONLY global skills (`scope: project` in a global
  profile is a validation error);
- a project profile: auto = project, then global; a project skill
  shadows a same-named global one; an explicit scope settles the
  question;
- `ResolvedSkill` always stores the actual scope.

Bridge for claude: `.claude/skills/<name>` -> a symlink to the canonical
directory (both project and global levels). The bridge is maintained by
`wf init`, doctor, and the profile tools; it is idempotent; real
(non-symlink) directories belonging to the user are left untouched -
this is flagged in diagnostics.

Delivering a profile's skill set to a node:

- Isolated workdir (worktree, field `isolation`): the runner
  MATERIALIZES the profile's skills as copies from the run snapshot
  (3.6) into `<workdir>/.agents/skills/` and `<workdir>/.claude/skills/`.
  Level: `skills: materialized`. Caveat: this is materialization of the
  selected set, not isolation - the agent still sees HOME, the global
  skill directories, and the rest of the filesystem; "nothing but this"
  requires a sandbox and is not part of the first iteration.
- Shared workdir (default): the agent sees all of the project's skills;
  a single pointer line naming the profile's skills is added to the
  prompt ("Relevant skills: X, Y - use them via your skills mechanism").
  Level: `skills: advisory`.
- Embedding skill content into the prompt is forbidden at any level.

### 6.5. Observability

`RunProvenance` gains `profiles: [{scope, name, bundle_digest}]`. The
manifest (3.6) holds the full pinning; attempt events hold the chain
element index, the actual `soul_delivery`, and the skill-delivery level.

## 7. Agent and model detection

Detection does not call models. The network promise is honest: wf
itself makes no network requests, but the probes execute local agent
binaries whose behavior wf does not control (7.5).

### 7.1. Probe registry

Built-in probes for the five agents; custom probes for agents from the
`agents:` config - only via explicit opt-in (`probe: true` in AgentDef);
by default only a presence scan is done for them.

```
probe {
  id            # claude | codex | agy | opencode | pi
  bins          # binary names for the PATH scan
  category      # vendor | aggregator (a descriptive property)
  version_args  # ["--version"]
  models_source # cmd([...]) | config_file(path) | static | none
  providers_source  # none | auth_file(path) - aggregators only
  auth_source   # where the auth hint comes from (kind: oauth | api_key | none)
}
```

### 7.2. Levels

1. Presence - a PATH scan with canonicalization: the candidate is
   resolved to a canonical absolute path (symlink chain), project-local
   PATH entries are ignored by default - a defense against a malicious
   local wrapper in a cloned repository. Absence is a valid result.
2. Version - a single spawn of `<canonical_path> --version`.
3. Models / providers / auth:
   - opencode: `opencode models` (provider/model format, a built-in
     catalog) + the names of authorized providers from auth.json;
   - agy: `agy models` (display names; `format: display`);
   - codex: the active model and `[model_providers]` from
     `~/.codex/config.toml` (`partial: true`);
   - claude: a static list of CLI identifiers built into the probe
     (`source: static`) - the detector's own data, unrelated to the
     model table (8) and not a mapping to it;
   - auth hints: claude - oauth vs. api-key; codex - account vs.
     api-key; opencode - a list of providers. Hints are a hint, not the
     truth.

Availability semantics: `model_not_available` is only ever returned for
an authoritative, full inventory (opencode, agy); for partial / static
inventories, a missing model is `model_unverifiable`.

To be precise about auth files: the files are read, but secret VALUES
are not returned in results, not logged, and not cached - only provider
names and auth type are exposed.

Models are returned as-is, in the agent's own `--model` format - without
normalization. Matching against the model table is done by the UA.

### 7.3. Probe spawn hygiene

Canonical absolute path; typed argv without a shell; a sanitized
environment (minimal PATH, HOME; no tokens from wf's own environment); a
process timeout; a limit on stdout/stderr volume (truncated with a
marker); kill-by-timeout by process group, as in the adapter.

### 7.4. Cache

`<config_dir>/state/agents-detect.json`. Entry key: the binary's
canonical path + its metadata (size, mtime). If the metadata changed,
probes are rerun; if not, the cache is used. A TTL of one day is a
safety net; configs and auth files are cached by their own mtime.
`--refresh` clears everything.

### 7.5. Promise boundaries

`--version` and `models` execute someone else's code: an agent may reach
the network itself, run an auto-update, or hang. wf limits the damage
via the hygiene in 7.3, but does not promise network sterility for
third-party binaries. Wording for the docs: "wf itself makes no network
requests and does not call models; the probes run the CLIs you have
installed, locally."

### 7.6. Output

`{agent, installed, canonical_path, version, category, models[]?,
providers[]?, auth?, notes}` - with incompleteness markers (partial,
display, static). Consumers: `profile_howto`, `agents_detect`, `wf
doctor`, the adoption report.

## 8. Model table, purposes, subscriptions

### 8.1. Storage and delivery

A canonical `assets/models.yaml` in the wf git repository, compiled into
the binary (`include_str!`), carrying `as_of`. A user overlay
`<config_dir>/models.yaml` is merged on top by key (models and purposes
- by name; subscriptions - overlay-only). Freshness is handled by
ordinary PRs; there is no network self-update.

### 8.2. Models (facts)

Rows are models (20-30), columns are facts. Units are explicit:

```yaml
as_of: 2026-07-12
models:
  opus-4.8:
    vendor: anthropic
    cost_in_usd_mtok: 15
    cost_out_usd_mtok: 75
    reasoning: high          # none | low | medium | high
    context_tokens: 1000000
    vision: true
    stt: false
    tts: false
```

Names are free-form and canonical; matching against detection IDs is
done on the UA side. There are no curated mapping tables; advisory
aliases are a candidate (13) if practice shows matching to be unstable.

### 8.3. Purposes (judgments)

Dozens of purposes, not only development:

```yaml
purposes:
  frontend:      [{ model: opus-4.8, score: 8 }, { model: glm-5.2, score: 7 }]
  brainstorming: [{ model: fable-5, score: 9 }, { model: gpt-5.6-sol, score: 8 }]
  translation:   [{ model: haiku-4.5, score: 8 }]
  cheap-glue:    [{ model: haiku-4.5, score: 9 }, { model: gemini-3.5-flash, score: 8 }]
```

A score of 1-10 is a curated judgment; absence from the list means "not
recommended," not "forbidden." Integrity of purposes -> models: a CI
test for the built-in file, doctor diagnostics for the overlay.

### 8.4. Subscriptions

A subscription changes the economics of choice, but does NOT mean
"free." Three parts:

1. Detection auth hints (7.2) - signals, not truth.
2. A user declaration - the overlay's `subscriptions:` section:

```yaml
subscriptions:
  - name: claude-max-20x
    vendor: anthropic
    via: [claude]
    coverage: unknown      # unknown | flat | quota; default unknown
    note: covers opus/sonnet/haiku via claude CLI
```

3. Matching subscription -> models is done on the UA side (vendor + via
   + note). Guidance in the howto: `coverage: flat` - prefer covered
   models; `coverage: quota` - prefer them with an eye on limits;
   `coverage: unknown` - the economic effect is unknown, use only as a
   weak tie-breaker per the user's explicit declaration, do NOT treat as
   "cheaper than usual." There are no curated "plan -> models" tables.

### 8.5. Onboarding survey

State store: `uninitialized | configured | declined`
(`<config_dir>/state/onboarding.json`; a user's refusal is `declined`,
and neither channel asks again; if they change their mind - `wf
subscriptions` manually).

- CLI (human): the survey runs on the first PROFILE scenario
  (`wf profile ...`, `wf adopt`, `wf detect`) in an interactive terminal
  (state uninitialized, stdin is a TTY), pre-filled from auth hints. Not
  any first run of wf: `wf run` in CI is not the place for surveys.
  Non-TTY - skipped without changing state.
- MCP (UA): while `uninitialized`, every `profile_howto` carries
  `subscriptions_uninitialized: true` and an instruction: "ask the user
  once and save via subscriptions_set; on refusal save declined."

## 9. MCP surface and behavior rules

### 9.1. Tools

- `profile_list` - both scopes: qualified ref, description, bundle trust
  status, skills, chain, digest.
- `profile_get` - full content + digests.
- `profile_write` - create/update: `{name, scope, description, soul_md,
  skills[], executor{...}, soul?, expected_digest?}`.
  Concurrency and atomicity:
  - the whole operation (create check, re-verification of
    `expected_digest`, publication, journal marker) runs under a
    per-profile inter-process lock (fsutil::lock_dir); checking the
    digest outside the lock is not sufficient - two processes could read
    the same digest and both pass;
  - create requires the profile's absence; update requires the
    `expected_digest` of the current content; a mismatch under the lock
    is a refusal `conflict`;
  - publication: staging directory -> operation journal -> swap rename
    -> journal marker. Resolvers that see an unfinished journal perform
    recovery before reading; the "directory doesn't exist" window is
    closed by the journal, not by luck;
  - partial failure: the profile is published, but the auto-approve
    write to trust.json fails -> the profile exists untrusted, the
    response carries its digest and the explicit code
    `trust_write_failed` (content is not rolled back for the sake of the
    trust record).
  Validation: the agent is known, skills exist in an allowed scope, the
  name follows the rules in 3.1. A model not found in detection is a
  warning, not a refusal.
- `profile_move` - copy semantics per 4.2.
- `profile_delete` - reference scans per 4.2; global requires `force:
  true` when references are found.
- `profile_howto` - tier 2 of profile work, only served when actively
  working with profiles: format, selection rules, table + purposes +
  subscriptions + auth hints + fresh detection. One call, one context
  payment.
- `agents_detect` - raw detection.
- `subscriptions_set` - save the declaration (or declined), updates the
  onboarding state.
- `workflow_adopt_report` - the report from 5.2.

### 9.2. Tier 0 (addition to instructions)

Three lines: nodes only have profiles; before creating a profile, call
`profile_list`; after creating or changing profiles while working on a
workflow, briefly mention it in the final message to the user. Details -
in `profile_howto`.

### 9.3. Catalog hygiene

`workflow_catalog` does not include profiles or the table. A field
`profiles_hint: {count}` is volatile and does NOT enter the
`catalog_revision` computation (documented in the tool's contract).

### 9.4. Auto-approve authorization boundaries

Auto-approve is trust in the RESULT of an already-authorized operation,
not authorization of the operation itself. The authorization model is
inherited from the transparent-workflows spec (its section on run
policy and consent): a direct user request authorizes the actions it
needs; agent initiative and blast-radius expansion require confirmation.
Examples:

- the user asked for a workflow that needs a new project profile -
  created without an extra question (mentioned in the final message,
  9.2);
- an agent-initiated change to an existing profile used by other
  workflows, or an unexpected global mutation - ask the user before
  proceeding;
- cross-workspace profile mutations are FORBIDDEN in the first version
  (profile_write/move/delete reject a foreign workspace) - a two-phase
  contract is not introduced for them.

The server does not see the dialogue and cannot prove consent - that is
the host's/UA's responsibility, as with workflows; server-side gates
(trust, scope validation, reference scans) limit the damage but do not
substitute for consent.

## 10. Migration (schema: 1 -> 2)

The existing `migration.rs` is about run patch compatibility and is NOT
reused. A separate schema migrator (`wf migrate`, module
`schema_migrate`):

- **dry-run by default**: the full plan is printed before any writes;
  `--apply` executes it; a backup is taken before apply
  (`.wf/backup-<ts>/`); idempotent (a repeat run is a no-op).
- **History is not rewritten**: saved schema-1 workflow versions remain
  untouched (digest immutability); migration creates a NEW schema-2
  version of each workflow and moves `current` to it.
- **Content-based dedup**: named executors are local to a workflow;
  identical content under the same name across different workflows ->
  one shared profile; different content under the same name ->
  deterministic names with a hash suffix (`default-a1b2c3`) plus a
  diagnostic "rename it meaningfully." An inline executor ->
  `<workflow-id>-<node-id>-<hash>`.
- **default_executor**: `merge_global_config` currently silently
  substitutes it into workflows without `defaults.executor`. The
  migrator materializes this implicitness: such workflows get
  `defaults.profile` set to a profile created from `default_executor`.
  The engine's implicit-substitution mechanic is removed along with
  executors.
- **Placeholders**: `SOUL.md` is created EMPTY (a TODO text would be
  sent to the agent as its role); diagnostics list profiles with an
  empty SOUL.
- **Schema-1 runs**: a run snapshot contains a workflow.yaml with
  executors. For resume, a limited legacy shim is introduced: on the
  first resume of an old run, the executors in its snapshot are
  converted into an ephemeral `ResolvedProfileSnapshot` (without writing
  to the project's profiles/), and a manifest is added to the run.
  Viewing and inspecting old runs works without the shim. The shim is
  temporary and marked for removal after the transition period ends.
- **RunOverrides v2**: the `executors` section is removed;
  `nodes.<id>.profile = QualifiedProfileRef` selects a different
  existing profile; for one-off experiments,
  `nodes.<id>.ephemeral_executor {agent, model}` - a run-local ephemeral
  snapshot (role and skills are inherited from the node's profile),
  recorded in the manifest with an ephemeral marker and authorized by
  the same run request. Named executors do not return under a different
  guise: an ephemeral one has no name and is not saved.
- The conflict with the old decorative `profile` field is resolved in
  favor of the new meaning, with a warning in the migration plan.

Global config: `executors:` and `default_executor` are converted into
global profiles using the same dedup.

## 11. CLI and web

CLI: `wf profile list|show|write|edit|move|delete` (`edit` opens
`$EDITOR` with a digest check on save), `wf subscriptions`, `wf adopt`,
`wf detect [--refresh]`, `wf migrate [--apply]`.

Web (wf-server): the node form gets a profile select instead of executor
fields; the profile editor is a minimal page (list + forms). The rest is
web phase.

## 12. Testing

- wf-core: profile/skill/bundle digest stability; resolution per 3.3
  (scope forms, workflow origin, a global workflow does not see project
  profiles); a single run with a project profile and a global profile of
  the same name; naming rules (matches the directory, case-fold
  collisions); move: copy semantics in both directions, name conflicts,
  skills during a move, journal crash-consistency (kill between phases);
  delete: scanning explicit `scope: project` references across all
  versions, best-effort for global + force; the snapshot procedure of
  3.5: symlink escape, symlink cycle, special file, exceeding limits,
  source drift between copy and digest (the copy wins); the schema
  migrator: dedup, hash suffixes, default_executor gets materialized,
  history untouched + a new version, idempotency, dry-run with no
  writes;
- wf-engine: the fallback chain from the manifest; the manifest is
  immutable, resume uses the snapshot (editing the live profile/skill
  after start has no effect); SoulRequirement/SoulDelivery:
  native_required filters the chain, an empty SOUL does not filter,
  events record the actual delivery; environment_drift: a binary
  metadata change between start and resume stops the run,
  `--allow-environment-drift` continues with a record; skill
  materialization by copy; the advisory string; an ephemeral override in
  the manifest; the legacy shim: resuming a schema-1 run with an
  unexecuted agent node; context compaction works without executors;
- detection: probes against stub scripts; path canonicalization and
  ignoring project-local PATH; caching by binary metadata; timeout and
  output limit; an absent agent; model_unverifiable for
  partial/static inventories;
- table: parsing the built-in one (a CI integrity test), merging the
  overlay and subscriptions, the coverage default of unknown;
- wf-mcp: profile_write - two concurrent calls with the same
  expected_digest: exactly one commit succeeds; create against an
  existing one - conflict; a crash between phases does not make the
  profile invisible (recovery before reading); trust_write_failed;
  bundle auto-approve; editing a skill after approval makes the next run
  untrusted; the `untrusted_profile_requires_acknowledge` gate;
  cross-workspace profile mutations are rejected; the howto package;
  subscriptions_set: declined is not asked again by the second channel;
  PlanPayload: skill drift between prepare and execute breaks the plan;
- e2e: an mcp_cli test "create a profile -> create a workflow with it ->
  run it" against a stub agent; resume after editing a profile.

Documentation: HOWTO authoring (profiles instead of executors), MCP.md
(new tools), a new docs/PROFILES.md.

## 13. Out of scope and possible evolutions

- Network self-update of the model table.
- Advisory aliases mapping model -> agent IDs (if LLM matching turns out
  to be unstable).
- Splitting the profile into a portable ProfileSpec and a local
  ProfileBinding (if team work on committed profiles produces conflicting
  binding diffs).
- `context_compact_profile` instead of a model string for compaction.
- A two-phase contract for cross-workspace profile mutations (forbidden
  in the first version).
- Full isolation of the skill set (filesystem sandbox). IMPLEMENTATION
  NOTE: materializing skills by copy requires real worktree isolation of
  the node, which the engine does not yet provide; until it appears,
  skills are delivered as an advisory string (names) regardless of
  `isolation`, so an isolated node isn't left without the project's
  files. The `materialized` level will turn on together with worktree
  isolation.
- Curated "subscription -> models" tables.
- User-level profile versioning.
- pi support: the probe is staged, invocation details once it appears.
- Full ACP transport for non-claude agents: the first iteration is
  headless.

## 14. Implementation phasing

One plan, two phases (a review recommendation, accepted):

1. **Core**: schema 2 + QualifiedProfileRef + resolver + bundle-trust +
   snapshots/manifest + invocations + SOUL/skills + migration +
   profile_* tools. The product is consistent already after this phase.
2. **Advisory layer**: detection + table + subscriptions + onboarding +
   profile_howto + adoption report. Additive, does not touch the
   phase-1 public schemas.
</content>
