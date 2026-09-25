# Agent profiles

A profile is the single executor binding for an `agent_task` node. Instead of
scattering agent, model and role across nodes, a node names one profile and the
profile encapsulates everything about who runs the work and how.

## What a profile contains

A profile lives in a directory:

- project scope: `<project>/.apb/profiles/<name>/`
- global scope: `<config-dir>/profiles/<name>/`

with two files:

- `profile.yaml`

  ```yaml
  name: architect            # must equal the directory name
  description: senior implementation agent
  executor:
    agent: claude            # one of the known agents (claude, codex, agy, opencode, pi, hermes, grok, cursor, qoder, zcode) or a configured one
    model: claude-opus-5-5   # exactly the string that agent's --model expects
    fallbacks:               # optional ordered chain; same role, different executor
      - { agent: codex, model: gpt-6-sol }
  soul: any                  # any | native_required (does the role need a native system-prompt channel)
  hermetic: false            # optional; default false. See "Hermetic isolation" below
  skills:                    # names (scope auto) or { name, scope }
    - coding-standards
    - { name: writing-plans, scope: global }
  ```

- `SOUL.md` - the role system prompt (free text). Delivered natively
  (for example `--append-system-prompt`) or as a prompt prefix, depending on the
  agent's capability. Never embedded as skill content.

Names must match `[a-z0-9][a-z0-9-]*`, at most 64 chars, and equal the
directory name. Case-fold collisions are rejected.

## Hermetic isolation

`hermetic` is an optional boolean, default `false`. When `true`, an executor
that supports settings isolation is launched with an apb-owned minimal settings
profile that disables user-scope plugins and hooks, so a run does not inherit
the operator's local agent configuration. Only `claude` and `claude-code`
support this today (via their `--settings` flag); any other agent ignores the
flag with a warning rather than failing the run. The flag is snapshotted into
the run manifest at start, so a retry, fallback, or resume uses the value the
run began with. Because `profile_digest` hashes the raw `profile.yaml`, setting
`hermetic` changes the digest and therefore the bundle trust just like any
other profile edit.

### Guidance: turn it on for production profiles

Set `hermetic: true` on every profile a run actually depends on, and leave it off
only for local throwaway experimentation. A node agent is a batch worker: it runs
once, reports, and exits. It has no business inheriting the operator's personal
plugins and hooks, and a globally installed hook (a `Stop` hook that demands one
more reply, a plugin that injects unrelated context) can silently change what the
node reports or make its completion signal fire later than the agent itself
intended. A profile without `hermetic` is a profile that trusts whatever happens
to be installed on the machine running it, which is rarely something a playbook
author reviewed.

What it does not do: it is not a sandbox. It does not isolate the filesystem, the
network, or the project's working tree. A node that needs isolation from other
concurrent branches touching the same files still needs the node's own `isolation`
setting (`full`, `best_effort`, or `none`), which is an orthogonal concern.
`hermetic` suppresses the operator's personal, machine-local agent configuration
and says nothing about what the node's own prompt, skills, or connectors may do.

Pair it with `outputs.extract` on the node (a marker name on the node's `outputs`
block, sibling to `outputs.files`; see HOWTO-authoring.md) as the other half of
output hygiene. `hermetic` stops local hooks from appending extra turns to an
agent's session in the first place; `outputs.extract` is the fallback when a host
injects one anyway, or when the bound executor ignores the flag, because it scopes
the node's recorded output to the agent's own marked block rather than to whatever
an unrelated appended turn added on top.

## Scopes and resolution

A node references a profile by name (`profile: architect`, scope `auto`) or by
object (`profile: { name: architect, scope: project }`). Resolution:

- a project-origin playbook resolves `auto` as project first, then global;
- a global-origin playbook sees only global profiles; `scope: project` in a
  global playbook is an error.

The same name can exist in both scopes as two distinct profiles. Skills follow
the profile's actual scope: a global profile may use only global skills; a
project profile resolves project then global.

## Trust (bundles)

Each profile has a `bundle_digest` over `profile.yaml` + `SOUL.md` + the sorted
skill digests. Writing a profile through `profile_write` (or `apb profile`) auto
approves its bundle. Any later edit to the profile or one of its skills changes
the bundle, so the next run through the MCP gate reports
`untrusted_profile_requires_acknowledge` until the user confirms.

A run started from the dashboard or from `apb run` does not gate on playbook or
profile-bundle trust at all: those paths pass no expected digest, so a changed
bundle is pinned on first use rather than held for approval, and a sub-playbook
child inherits the same posture as its parent instead of being stricter than the
run that spawned it. Connector and account trust is the exception and is
enforced on every path and at every depth, parent and child alike, because it
guards where secrets are sent (see `docs/CONNECTORS.md`). Use the MCP gate when
you want profile-bundle changes to require an explicit acknowledgement.

Snapshot scope, honestly: a run snapshots `profile.yaml` and `SOUL.md` (plus the
resolved invocation chain) into `runs/<id>/` and drives from that copy, so a
live edit to the profile or SOUL after start does not affect a running or
resuming run. Skill immutability depends on the node's `isolation`:

- `isolation: full` or `best_effort`: the run materializes real copies of the
  profile's skills from the run snapshot into a fresh per-attempt workdir
  (`runs/<id>/work/<node>/<attempt>/.agents/skills/<name>` plus a `.claude/skills`
  bridge) and points the agent at that workdir, so the agent reads only the
  snapshot. Materialization happens per attempt from the immutable run snapshot,
  so a retry or fallback does not inherit a mutated skills tree from a prior
  attempt, and a live edit to a skill file mid-run does not affect the run. Each
  attempt records `skills_mode: materialized`, and a node `success_check` runs in
  that same per-attempt workdir. Note the current boundary: this isolated workdir
  contains the
  materialized skills but not a copy of the project tree - full project-tree
  sandboxing arrives with worktree isolation (spec 8.3). A node that needs the
  project's working files should use `isolation: none` until then.
- `isolation: none` (default): skills are delivered advisory-only - the agent is
  given the skill names in its prompt and reads the live `.agents/skills/<name>`
  at run time, so a live edit mid-run can still change what it reads. Each
  attempt records `skills_mode: advisory`. Skill content is never embedded in
  the prompt in either mode; only names are passed.

## Managing profiles

MCP tools (agent-facing): `profile_list`, `profile_get`, `profile_write`,
`profile_move`, `profile_delete`, plus the advisory `profile_howto`,
`agents_detect`, `subscriptions_set` and `playbook_adopt_report`.

CLI: `apb profile list | show | move | delete | write | edit`, `apb detect`,
`apb adopt`, `apb subscriptions`, and `apb migrate` to convert legacy `executors`
playbooks. `apb profile write --scope --agent --model [--fallback a:m ...]
[--skill NAME ...] [--soul FILE] [--description ...] [--expected-digest DIGEST]`
creates or updates a profile through the same logic as the MCP tool (validation,
per-profile CAS lock, bundle auto-approve); a stale `--expected-digest` is a
reported conflict. `apb profile edit <name> [--scope]` opens `profile.yaml` and
`SOUL.md` in `$EDITOR` and saves with a CAS check against the digest read before
editing, so a concurrent change is a conflict rather than a clobber.

Web: `GET /api/profiles` lists project and global profiles with trust status;
`POST /api/profiles` creates or updates one through the same shared logic
(returning digests and trust result, `409` on a CAS conflict). The agent-node
form in the editor uses a profile selector bound to these endpoints.

## Choosing agent and model

`profile_howto` returns a curated, advisory models table (facts per model plus
purpose scores such as coding, review, planning, writing, cheap-glue,
vision-tasks) together with local detection of installed agents. Detection is
local: apb runs each agent's `--version` and reads local config, and makes no
network request of its own. That is not a claim that a spawned agent is offline
- apb does not control what a third-party CLI does when a playbook runs. The
table is a hint only: nothing is hard-bound to it, and model availability is
asserted only when detection authority is Full. Each model row carries
provenance (`source_url`, `checked_at`, `price_basis`) so a stale or estimated
price is visible rather than implied as authoritative; a user overlay at
`<config_dir>/models.yaml` merges field-wise per model (setting one price does
not reset the other fields). Declare which subscriptions you have with
`subscriptions_set` (or `apb subscriptions`) so advice matches your access.

Not every agent can run unattended. apb passes a non-interactive permission flag
where the agent has one: `--permission-mode bypassPermissions` for claude and
grok, `--force` for cursor, `--permission-mode bypass_permissions` for qoder,
`--mode yolo` for zcode, `--dangerously-skip-permissions` for agy,
`--dangerously-bypass-approvals-and-sandbox` for codex, and `--auto` for
opencode. hermes has no flag apb passes today: its `--yolo` is documented but
unverified in the one-shot form apb uses, so it is deliberately not shipped. A
node bound to an agent without such a flag can hang indefinitely on a write
permission prompt in an autonomous run, and `apb doctor --run <id>` warns about
exactly that binding.

The workaround that always works is a stdout-only reviewer profile. Give the
profile a SOUL that tells the agent to report everything it finds on stdout and
to write nothing, and let apb capture the output as the node's result. The agent
then never needs write permission at all, so no approval prompt can appear,
whatever flags the CLI does or does not offer. That is also the right shape for
review, audit and analysis nodes regardless of the agent, because the finding is
the deliverable and nothing in the workspace changes.

## Where the agent and model lists come from

There is exactly one source of truth for which agents exist and which models
apb offers for each: `apb_core::agent_catalog`. Every consumer goes through it,
so the lists cannot disagree between the dashboard, the CLI, the MCP tools, and
validation.

| Layer | Holds | Role |
|---|---|---|
| `assets/models.yaml`, embedded in the binary | curated rows, `claude_static_models`, `codex_static_models` | authoritative for claude, codex, and the curated table |
| `<config_dir>/models.yaml` (user overlay) | per-field row patches, list overrides, subscriptions | authoritative when present; merged by `models_table::load_merged` |
| `zcode::ALLOWED_MODELS` (Rust constant) | `GLM-5.3`, `GLM-5.3-Flash` | authoritative for zcode; the spawn path, `apb validate`, `profile_write` and adoption enforce the same constant |
| agent probes (`detect::probe`) | installed, version, `opencode models` output, auth and provider hints | authoritative for external facts only; never produces an apb-owned list |
| `<config_dir>/state/agents-detect.json` | the last probe results | an invisible memo of the probes, see below |
| `agent_catalog::load` / `assemble` | agents with their inventories, `options_by_agent` | the one function every consumer calls |
| `GET /api/models`, `GET /api/agents` | one catalog snapshot, `Cache-Control: no-store` | derived, served fresh on every request |
| web profile editor | nothing persisted | reads `/api/models` on every mount |
| `apb detect`, MCP `agents_detect`, `profile_howto` | `agents` and `options_by_agent` | derived from the same catalog |

`static_models_for_agent` is the single definition of an agent's closed list
(claude, codex, zcode). `assemble` sets it as the installed agent's `Static`
inventory and `model_options_for_agent` offers exactly it, so the detected
inventory and the editor's options are one list. Other agents get the curated
table (filtered to the vendor for grok) plus whatever the agent itself listed.

The memo stores only probe results, never a list apb owns: those are recomputed
from the running binary on every call (microseconds). The memo is reused only
when all of these hold, otherwise every agent is probed again:

- it was written by the same build: `detect::build_id` is the package version,
  the fingerprint (path, size, mtime) of the running executable, and a digest of
  the embedded `assets/models.yaml`;
- every probe input is unchanged: the program probed (see below), the agent
  binary fingerprint and the files the
  probe or the agent's own model listing reads (auth and config files; for
  opencode also `opencode.json`, `opencode.jsonc` and its cached model catalog),
  plus the presence of `ANTHROPIC_API_KEY`;
- it is younger than 24 hours, the bound for what no local input shows (for
  example opencode's remote model catalog).

Each built-in agent is probed at the program a run launches: `agents.<id>.program`
from `config.yaml` when set (a name looked up on PATH, or a path used as is),
otherwise its default binary. The default binaries (`cursor` runs as
`cursor-agent`, `claude-code` is `claude`, zcode falls back to its home-deployed
CLI) live in one table, `detect::BUILTIN_BINS`; the engine launches
`detect::default_program` from the same table.

Why a memo at all: the probes run in parallel, and a cold detection on a machine
with claude, codex, opencode, grok and zcode installed still takes about 1.6 s
(`opencode models` alone is about 1.1 s); a memo hit takes about 2 ms. `apb
detect --refresh` and MCP `agents_detect` with `refresh: true` bypass it.

The dashboard never keeps its own copy. A reload always shows the lists of the
`apb` that is running, and a rebuilt binary shows its new lists on the next
reload without any cache to clear.

## ZCode (zcode)

`agent: zcode` runs the headless CLI of Z.ai's ZCode desktop app. apb finds it
as `zcode` on PATH, else at `~/.zcode/server/agents/glm/zcode-agent`, where the
desktop deploys it (it never puts it on PATH); `agents.zcode.program` in the
global config overrides both. `apb detect` and `apb doctor` report it.

apb allows exactly two zcode models, both on Z.ai's paid Individual coding
plan: `GLM-5.3` and `GLM-5.3-Flash`. A model string is the bare model id with
an optional effort suffix:

```yaml
executor:
  agent: zcode
  model: GLM-5.3                 # <model>[@<effort>]
  fallbacks:
    - { agent: zcode, model: GLM-5.3-Flash@low }
```

- `<model>`: `GLM-5.3` or `GLM-5.3-Flash`, matched case-insensitively and
  stored in that spelling. A bare model resolves to the Individual coding plan
  of the account family ZCode is set to (`zai` unless ZCode's settings say
  `bigmodel`).
- `@<effort>`: optional reasoning level (ZCode's effort setting): `@low`,
  `@high` or `@max`. Omitted, apb fills in the model's highest level, which is
  ZCode's own default. There is no profile-level effort field; the suffix is
  zcode-only.
- The older plan-qualified spelling `zai-individual/GLM-5.3-Flash@high` is
  still accepted for backward compatibility and means the same as
  `GLM-5.3-Flash@high`; the dashboard and the profile tools save the bare
  form, and apb never emits the prefix.
- Any other model (GLM-5.2, GLM-5-Turbo, ...) or plan (Start, Team, Idle) is
  refused with a message naming the two allowed models: by the profile editor
  and `profile_write`, by `apb validate` (`zcode_model_not_allowed`), by the
  adopt report (`model_not_allowed`), and before zcode is spawned.
- apb also validates the selection against ZCode's built-in provider config
  before spawning (the model must be enabled on the plan, the effort one the
  model supports). ZCode itself does not reject an invalid selection: it
  silently runs GLM-5.3 at max effort instead (observed), so apb fails such a
  step up front.
- A custom provider from the user's ZCode provider config
  (`<provider-id>/<model>`) is passed through unchecked.

`apb detect` and the profile editor list exactly `GLM-5.3` and
`GLM-5.3-Flash`, whatever else ZCode's built-in config enables.

How it runs: `zcode-agent -p <prompt> --json --mode build`. ZCode's `--mode`
defaults to `yolo` for `-p`, so apb always pins one: `build` normally (in
headless mode every approval request is denied, so file writes and shell
commands are refused: effectively read-only), `yolo` only for an authorized
effectful run (appended last, the last value wins). The CLI has no `--model`
flag: apb writes a run-scoped copy of the user's ZCode personal provider
config (`<run>/agent-home/zcode/<node>/provider_config.json`) with the
selection as its `defaultModelSelection` and points
`ZCODE_PERSONAL_PROVIDER_CONFIG_FILE` at it; the user's own ZCode config is
never written. The node output is the `response` of the `--json` result, and
its `sessionId` feeds `--resume` for the interactive `resume` transport. The
SOUL travels as a prompt prefix (no system-prompt flag).

Login and plans. The headless CLI needs its own one-time login,
`~/.zcode/server/agents/glm/zcode-agent login`; the desktop app's login does
not cover it, and `apb doctor` warns when it is missing. Note that this login
also sets the default model in ZCode's shared provider config. In
zcode-agent 0.16.9 the headless CLI can only use the individual coding plan:
the Start (free), Team and Idle plans the desktop offers are desktop-only.
Because ZCode silently runs its first usable plan when asked for one it cannot
use, apb refuses such a step before spawning (an auth-class failure that the
fallback chain skips) instead of letting it spend the paid plan.

Fallbacks. A spend or quota stop (Z.ai `Usage limit reached`,
`Weekly/Monthly Limit Exhausted`, `Insufficient balance`) is a budget failure
and blocks the Individual plan for the rest of the chain, not the whole agent:
the other zcode model is skipped too (same quota), while a fallback on another
agent is still tried.

## Codex (codex)

`apb detect` and the profile editor offer codex a fixed list, in this order
(the first one is the editor's default): `gpt-6-sol`, `gpt-6-astra`,
`gpt-6-luna`, `gpt-5.6-sol`, `gpt-5.6-terra`, `gpt-5.6-luna`, `gpt-5.5`. The
list lives in `assets/models.yaml` (`codex_static_models`), next to a pricing
row for each model; the `model` line of `~/.codex/config.toml` does not extend
it.
