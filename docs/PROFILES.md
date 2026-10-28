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
  environment: full          # optional; default minimal. See "Agent environment" below
  zcode_mode: edit           # optional, zcode steps only: yolo (default) | edit. See "ZCode (zcode)"
  process_env:               # optional; default inherit. See "Process environment" below
    policy: allowlist
    allow: [GH_TOKEN, NPM_CONFIG_*]
  skills:                    # names (scope auto) or { name, scope }
    - coding-standards
    - { name: writing-plans, scope: global }
  ```

- `SOUL.md` - the role system prompt (free text). Delivered natively
  (for example `--append-system-prompt`) or as a prompt prefix, depending on the
  agent's capability. Never embedded as skill content.

Names must match `[a-z0-9][a-z0-9-]*`, at most 64 chars, and equal the
directory name. Case-fold collisions are rejected.

## Agent environment

`environment` says what an executor loads of the operator's own agent setup:
`minimal` (the default, also when the key is absent) or `full`. A node agent is a
batch worker: it runs once, reports, and exits. Loading every plugin, MCP server,
user skill and personal instruction file installed on the machine costs thousands
of tokens on every spawn (on one development setup, 2026-09-25: about 24k tokens of harness
context for a claude spawn with the full setup, 18k with the minimal one, measured
with `claude -p /context`, which needs no model call) and changes what the node
does: a globally installed `Stop` hook can demand one more reply after the agent
already reported, and a plugin can inject unrelated context.

With `minimal`, a claude or claude-code executor is launched with:

- `--settings <run>/agent-settings/<node>.json`: an apb-owned settings file with an
  empty `hooks` object, no plugins and no auto-enabled project MCP servers (the
  empty `hooks` does not remove the project's own hooks: those still run, see
  GUARDRAILS.md "Host agent settings");
- `--setting-sources project,local`: the user's settings (and their hooks),
  `CLAUDE.md`, skills and agents stay out, while the project's own `CLAUDE.md`,
  `.claude/skills` and `.claude/settings.json` hooks still load;
- `--strict-mcp-config`: only the MCP servers apb passes itself (the `ask_user`
  server of a live interactive node);
- `--add-dir <run>/agent-skills/bundle-<digest>` when the profile declares
  `skills`: a copy of those skills from the run snapshot, so the profile's own
  skills still reach the agent although the user source is not loaded (an
  isolated node already has them in its working directory). The copy is shared
  by every node of the profile in the run and checked against the snapshot
  digests before every attempt (laid down again when an attempt changed it).
  One path per profile keeps the agent's system portion byte-identical across
  those nodes, which is what lets a provider prompt cache hit: the directories
  an agent is pointed at are part of its system prompt.

`full` is the explicit opt-in to the operator's whole personal environment, as an
interactive session would have it. Use it for a profile that depends on something
only that environment provides: a skill that comes from a plugin (for example the
superpowers review skills), a user-scope MCP server, or a user skill that is not
listed in `skills`. Command-line tools (`gh`, `zg`, `code-ranker`, ...) and files
read by path (`~/.agents/skills/<name>/SKILL.md`) are reachable either way:
`environment` governs the agent's own configuration, not the shell.

The repository's own `branch-reviewer` profile (the reviewer of the
`branch-quality-review` playbook) runs `minimal`: it lists no skills and needs
no plugin, user skill or user-scope MCP server, and the command-line tools its
review uses are reachable either way. A profile of your own that does depend on
the personal setup still works with `full`, in runs and in `apb eval` alike.

Only claude and claude-code have such a mechanism today. Other agents run as they
are configured (codex already gets a run-scoped config home). The value is
snapshotted into the run manifest at start, so a retry, fallback, or resume uses
the environment the run began with, and a run started before this setting existed
keeps the full environment it started with. Because `profile_digest` hashes the
raw `profile.yaml`, setting `environment` changes the digest and therefore the
bundle trust just like any other profile edit.

`environment` is not a sandbox. It does not isolate the filesystem, the network, or
the project's working tree; a node that needs that needs its own `isolation`
setting (ISOLATION.md). Pair it with `outputs.extract` on
the node (see HOWTO-authoring.md) as the other half of output hygiene: the minimal
environment stops local hooks from appending extra turns in the first place, and
`outputs.extract` scopes the recorded output to the agent's own marked block when
a host injects one anyway or the bound executor has no minimal environment.

The older `hermetic` key is deprecated. It is still accepted in `profile.yaml`
but selects nothing: every write path used to emit `hermetic: false` whether or
not anyone chose it, so it cannot mean the opt-in, and `hermetic: true` is what
the default does anyway. Rewrites drop it. On the write surfaces (`--hermetic`,
the MCP and HTTP `hermetic` field) it is a deprecated alias: `true` means
`minimal`, `false` means `full`.

## Process environment

`process_env` says which of the operator's environment variables an agent
process inherits. It is separate from `environment`, which is about the
agent's own configuration (settings, plugins, MCP servers), not the process
environment.

```yaml
process_env:
  policy: allowlist          # inherit (the default, written as the absence of the key) | allowlist
  allow:                     # names added to the built-in sets
    - GH_TOKEN
    - NPM_CONFIG_*           # a trailing * matches by prefix
    - SSH_AUTH_SOCK
```

- `inherit` passes everything apb itself runs with, as before.
- `allowlist` clears the inherited environment and passes back only:
  - the base set every agent gets: `PATH HOME USER LOGNAME SHELL LANG LANGUAGE
    TZ TERM`, the locale categories by name (`LC_ALL LC_CTYPE LC_COLLATE
    LC_MESSAGES LC_MONETARY LC_NUMERIC LC_TIME LC_ADDRESS LC_IDENTIFICATION
    LC_MEASUREMENT LC_NAME LC_PAPER LC_TELEPHONE`, never an `LC_` prefix),
    `TMPDIR TMP TEMP`, the `XDG_*_HOME` dirs and
    `XDG_RUNTIME_DIR`, the proxy variables (`HTTP_PROXY HTTPS_PROXY NO_PROXY
    ALL_PROXY` and their lowercase forms), the certificate bundles
    (`SSL_CERT_FILE SSL_CERT_DIR NODE_EXTRA_CA_CERTS REQUESTS_CA_BUNDLE
    CURL_CA_BUNDLE`), `APB_CONFIG_DIR` and, on Windows, `SYSTEMROOT USERPROFILE
    APPDATA LOCALAPPDATA PATHEXT COMSPEC`;
  - the variables the agent CLI reads (the table below);
  - the names in `allow`.
- apb's own variables (`APB_RUN_DIR`, `APB_NODE_ID`, `APB_ATTEMPT_TOKEN`,
  `APB_STATUS_FILE`, `CODEX_HOME`, the zcode provider config, an eval overlay)
  are set after the plan, so they always arrive.
- A variable a connector references is withheld under either policy, even when
  `allow` names it.

| Agent | Always | Only when the trigger is on | Derived |
|---|---|---|---|
| claude | `ANTHROPIC_* CLAUDE_CODE_* CLAUDE_CONFIG_DIR MCP_TIMEOUT MCP_TOOL_TIMEOUT DISABLE_TELEMETRY DISABLE_AUTOUPDATER MAX_THINKING_TOKENS` | `CLAUDE_CODE_USE_BEDROCK`: `AWS_*`; `CLAUDE_CODE_USE_VERTEX`: `GOOGLE_APPLICATION_CREDENTIALS GOOGLE_CLOUD_PROJECT GCLOUD_PROJECT CLOUD_ML_REGION VERTEX_REGION_* CLOUDSDK_CONFIG`; `CLAUDE_CODE_USE_FOUNDRY`: `AZURE_*` | |
| codex | `CODEX_* OPENAI_*` | | every `env_key` and `env_http_headers` value of the `[model_providers.*]` tables in your codex `config.toml` (as tables, dotted keys or inline tables; exact names only, a `*` there is ignored) |
| opencode | `OPENCODE_*` | | the key of the model's provider (`anthropic/`, `openai/`, `openrouter/`, `zai/`, `zai-coding-plan/`, `google/`, `google-vertex/`, `amazon-bedrock/` (`AWS_*`), `xai/`, `deepseek/`, `groq/`, `mistral/`, `moonshotai/`, `opencode/`, `github-copilot/`) |
| zcode | `ZCODE_*` | | |
| hermes | `HERMES_*` | | its keys come from `~/.hermes/.env`, found through `HOME` |
| grok | `GROK_* XAI_API_KEY` | | |
| cursor | `CURSOR_*` (not verified against an installed CLI) | | |
| qoder | `QODER_* QODERCLI_*` | | |
| agy | `AGY_* ANTIGRAVITY_* GEMINI_API_KEY GOOGLE_API_KEY GOOGLE_CLOUD_PROJECT GOOGLE_APPLICATION_CREDENTIALS CLOUDSDK_CONFIG` | | |
| any other agent (config `agents:`) | the base set and `allow` only | | |

A trigger is on when its value is `1`, `true`, `yes` or `on` in any case;
`CLAUDE_CODE_USE_BEDROCK=0` or `=false` keeps the cloud credentials withheld.

Anything else the agent's own tools need comes through `allow`: for example
`SSH_AUTH_SOCK` for git over ssh, `GH_TOKEN` for `gh`, or `NPM_CONFIG_*` for a
private registry.

Rules for `allow`: each entry is `[A-Za-z_][A-Za-z0-9_]*`, optionally followed by
one `*`; the part before `*` needs at least 2 characters (a bare `*` is refused);
no duplicates; at most 64 entries; names only, never values. `allow` under
`policy: inherit` is refused as meaningless. A malformed block is reported as
`profile_process_env_invalid` by `apb validate`, refused by every profile write
and refuses a run start.

The plan is made per chain step, so a fallback onto another agent gets that
agent's set from the same profile policy. The policy applies to every agent
process apb spawns for the profile: node attempts in their start and resume
forms, the finish answer, the judge, the background supervisor and the quota
probe. It is snapshotted into the run manifest at start, so a retry, a fallback
or a resume runs under the policy the run began with, whatever the live profile
says by then. Setting `process_env` changes `profile_digest`, so the bundle needs
trust again. An apb older than this field rejects a profile that has it, and
ignores it when it resumes a run: resume a run started with an allowlist with
this apb version or a later one.

Every attempt under `allowlist` journals `agent_env_applied`: the agent, how many
variables it kept, the names it did not get (sorted, the first 200) and the
profile's own exact `allow` names that the operator does not set. Values are
never written anywhere. When an attempt fails under an allowlist, `apb doctor
--run <id>` names those variables and suggests adding the one the agent needs to
`allow`.

What it does not cover:

- `script` nodes, `success_check` commands, goal scripts and baseline commands
  run author code and inherit the operator environment as before.
- A connector call (`apb connector call`, a child of the agent) inherits the
  agent's allowlisted environment. `{{env.VAR}}` secrets resolve from the dotenv
  files as before; a `{{cmd:...}}` helper that needs variables such as
  `PASSWORD_STORE_DIR`, `GNUPGHOME`, `OP_SESSION_*` or `DBUS_SESSION_BUS_ADDRESS`
  gets them only when `allow` names them (see CONNECTORS.md).
- Context compaction (the internal summarizer) has no profile of its own: in
  a run where any profile (a node's, the supervisor's or a rebound one) sets
  `allowlist`, it runs under an empty allowlist for claude (the base set and
  claude's set); in any other run it inherits.
- In host execution mode the host's own subagent runs the step: nothing is
  applied there, and `apb doctor --run` says so.

What `allowlist` is, and what it is not: it stops the agent from inheriting
the operator's variables. On Linux, apb also makes itself non-dumpable before
its first allowlisted spawn (`prctl(PR_SET_DUMPABLE, 0)`), so the agent cannot
read apb's own environment back through `/proc/$PPID/environ` (apb's
`/proc/<pid>/stat` and `ps` stay readable; apb then writes no core dump and
cannot be attached to with a debugger by the same user). Should the call
fail, apb prints one `apb: warning: could not make apb non-dumpable ...` line
with the OS error and spawns the agent anyway, without that protection. It is
not isolation
from a hostile agent running as the same user: the operator's other processes
(a shell, an editor, another apb) stay readable through `/proc`, and files the
user can read stay readable. Real isolation needs a sandbox: a node's
`isolation` (ISOLATION.md), which also forces the allowlist on a profile that
says `inherit` (`agent_env_applied` records `forced_by: isolation`). macOS has no equivalent of the `/proc` hiding: there, `ps eww` by the
same user can show another process's environment.

On macOS, a claude login kept in the Keychain does not depend on environment
variables; check a profile with `allowlist` once on the machine it runs on.

Set it with `apb profile write --process-env allowlist --env-allow GH_TOKEN`
(`--env-allow` is repeatable and implies `allowlist`), the MCP `profile_write`
field `process_env: { policy, allow }`, or `POST /api/profiles` with the same
object (on every surface `allow` without `policy` means `allowlist`). A write that leaves it out keeps the stored value; `policy: inherit`
clears it.

## Host execution mode: what a profile still means

A run started with `execution: host` (see `docs/HOST-INTEGRATION.md`) spawns
no agent CLI: the host session executes every agent step with its own
subagents. Nothing in a profile has to change for it, but part of the
profile no longer applies:

- ignored: `executor.agent`, `executor.model` (the host runs every task with
  its own subagent tool and picks its own model; it never launches the
  profile's agent CLI for a task, see "The execution contract" in
  `docs/HOST-INTEGRATION.md`),
  the `agent` of each fallback, `command` and every invocation setting of the
  agent (`transport`, `ui_sync`, `zcode_mode`), `environment: minimal` (a
  claude-only mechanism), and session continuation (`continue_session` starts
  cold with reason `host_mode`);
- turned into hints: the `model` of each fallback entry and of a routed tier
  becomes the host task's `model_hint` (the profile's own model gives none),
  which is advisory only:
  the host may honor it or not. The chain itself still engages, and each
  step is a new host task: an expired deadline or an unanswered question
  advances to the next fallback or tier at once, while a `failed`
  submission (or a reply the status file, `success_check` or the completion
  check rejects) first spends the node's retries on the same step. A host
  submission is a reply, so a routed tier below the executor spends those
  retries too; only a CLI agent failure there goes up a tier at once. The
  hint is only the declared model of that step, never the model the host
  ran, so every surface labels it with its source (`hint_source`: `primary`,
  `fallback` with its index, `tier`, or `host_fallback` for the host step
  of a `cli` run whose CLIs could not start) and, on the first task of a
  later step, with what closed the previous one (`fallback_of`: the attempt
  and `failed`, `expired`, `question_timeout`, ...). `apb tasks` prints it
  as `advisory: model hint M (fallback 1 of 1 declared by profile P after attempt 1
  failed; the host picks its own model)`. With a tier routed above the
  executor, that tier is the engine's first step and the profile's executor
  behind it is labelled `primary` but reached like a fallback;
- still in force: the role prompt (`SOUL.md`, handed over as `role_prompt`),
  the skills (their snapshot copies are materialized as for a CLI step and
  their paths handed over), and on the node `timeout_seconds` (the task's
  deadline), `expected_duration`, `outputs`, `success_check`,
  `require_verdict`, `completion_check` and `protect` (a protected file a host
  subagent changed is restored and fails the attempt, as for a CLI step).

Profile trust is checked exactly as for a CLI run: host mode changes who
executes, not what is allowed. `apb doctor` and `playbook_adopt_report` judge
the profile CLIs as usual and say that a host-mode run ignores them.

## Executor tiers (optional)

A profile may declare `tiers`, lightest first, for decision-model tier
routing (see DECISIONS.md). A node opts in with `route: auto`.

```yaml
executor: { agent: claude, model: <mid-size model id> }
tiers:
  light:    { agent: opencode, model: <small model id>, for: "Mechanical edits, renames, formatting, small read-only lookups." }
  standard: { use: executor, for: "Ordinary implementation and review tasks." }
  heavy:    { agent: claude, model: <largest model id>, for: "Cross-cutting design changes, subtle concurrency or security work." }
```

Each tier is an `agent` plus `model`, or `use: executor` (the profile's own
executor chain; without one the executor counts as the heaviest tier). `for`
describes the work, never the price. Tiers are part of the profile digest,
so adding or editing them means granting trust again, and they are
snapshotted into the run manifest. `apb validate` and `apb doctor` check
every tier's model against the model lists and `model_policy`, and a run
start fails on a tier with an unknown agent. An apb older than this field
rejects a profile that has it (`profile.yaml` is strict). No write surface
edits tiers yet: `profile_write` keeps a profile's stored tiers on an update.

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
skill digests. Writing a profile through `profile_write` (or `apb profile`, or
the dashboard) auto approves its bundle, as far as the write can vouch for it:
the write produces `profile.yaml` and `SOUL.md`, not the skills. A new profile,
or a skill the write newly names, is the user's choice in that write. A skill the
profile already had is vouched for only when the bundle before the write was
approved; if its content changed since then (a `git pull` of `.agents/skills`),
the bundle stays untrusted and the write returns `trusted: false` with
`skills_unapproved` (`skill`, `digest`) for the user to review. Any later edit to
the profile or one of its skills changes the bundle, so the next run through the
MCP gate reports `untrusted_profile_requires_acknowledge` until the user
confirms.

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
  profile's skills from the run snapshot for each attempt, so the agent reads
  only the snapshot. Materialization happens per attempt from the immutable
  run snapshot, so a retry or fallback does not inherit a mutated skills tree
  from a prior attempt, and a live edit to a skill file mid-run does not
  affect the run. Each attempt records `skills_mode: materialized`. Where the
  copies live depends on whether the attempt gets a sandbox (ISOLATION.md):
  - In a sandbox (Linux with bubblewrap) the attempt runs in the node's own
    tree, the project's working files included; the copies sit in the
    attempt's box under the run (`runs/<id>/sandbox/attempts/`)
    and are mounted read-only over the tree's `.agents/skills` and
    `.claude/skills`. Mount points apb had to create are removed after the
    attempt. A `success_check` runs inside the same sandbox.
  - Without a sandbox (`best_effort` on a machine that cannot build one) the
    attempt runs in a fresh scratch directory
    (`runs/<id>/work/<node>/<attempt>/.agents/skills/<name>` plus a
    `.claude/skills` bridge) that holds the copies and no project tree, and a
    `success_check` runs there.
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
[--skill NAME ...] [--soul FILE] [--description ...] [--expected-digest DIGEST]
[--environment minimal|full] [--zcode-mode yolo|edit]` creates or updates a profile
through the same logic as the MCP tool (validation, per-profile CAS lock, bundle
auto-approve); a stale `--expected-digest` is a reported conflict. An update that
omits `--environment` (or the MCP `environment` field, or the web editor) keeps
the stored value. `apb profile edit <name> [--scope]` opens `profile.yaml` and
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
CLI) live in the agent modules (`apb_core::agents`, see "Adding an agent"
below); the engine launches `detect::default_program`, which reads the same
registry.

Why a memo at all: the probes run in parallel, and a cold detection on a machine
with claude, codex, opencode, grok and zcode installed still takes about 1.6 s
(`opencode models` alone is about 1.1 s); a memo hit takes about 2 ms. `apb
detect --refresh` and MCP `agents_detect` with `refresh: true` bypass it.

The dashboard never keeps its own copy. A reload always shows the lists of the
`apb` that is running, and a rebuilt binary shows its new lists on the next
reload without any cache to clear.

## Adding an agent

Everything apb knows about a built-in agent lives in its module under
`crates/apb-core/src/agents/` (a file, or a directory when the agent has its
own output reader or helpers). The rest of apb asks the registry
(`agents::lookup`, `agents::all`) and never compares agent ids. A module
implements `agents::Agent`:

| Part | Holds |
|---|---|
| `descriptor()` | id, legacy aliases, binaries, version and help arguments, home install locations, category, capabilities (hermetic settings, live ask-server, long tool waits, the transcript the grader reads, the streaming transport, desktop history) and `Verified`, the CLI version and date its forms and reader were last checked against |
| `forms()` | the built-in launch form, the resume argv and how a fresh attempt makes its session findable (an id apb assigns, a title looked up in the agent's session listing, or an id the agent prints); `None` when apb cannot launch the agent without `agents.<id>.invocation` |
| `output()` | the reader of the agent's output (reply, usage, closing record, session id, a session header on stderr) and `promises()`, what the built-in form must carry; an agent that prints plain text uses the shared `PlainText` reader |
| `setup()` | what detection reads (models listing, login hint, provider notes, memo inputs), the environment set kept under `process_env: allowlist`, the variables a model or the agent's own config names, and the run-scoped environment of a spawn (codex's config home, zcode's provider config) |
| `models()` | the vendor, the closed model list, how a model id is spelled and checked, and the billing account a model spends |

To add one:

1. Write the module and add it to `agents::all()` at its place in the
   detection order (the dashboard and `apb detect` keep that order).
2. Add a contract fixture, `crates/apb-engine/tests/fixtures/agents/<id>.yaml`,
   and the id to the `contract!` list in
   `crates/apb-engine/tests/suite/agent_contract/mod.rs` (see
   `docs/TESTING-GUIDELINES.md`, "The agent contract suite").
3. Add the agent's row to the golden tables of
   `crates/apb-core/tests/suite/agents_test.rs`.

A guard test (`agents_test::agent_knowledge_stays_in_the_agent_modules`) fails
when non-test code outside the modules names a built-in agent id as a string
literal. One rule ties the readers to the forms (`agents::conformance`): a
built-in form whose output is not the machine format its module promises means
the CLI's output format changed, while a custom `agents.<id>.invocation` read
as plain text is the allowed fallback.

### Checking an installed agent

`apb doctor` checks each built-in agent a profile names, and `apb doctor
--agents` every installed built-in agent, with no model call
(`agents::check`). The binary checked is the one a run launches: the
configured `agents.<id>.program` when set, else the detected one. The config
is read under the agent name a profile uses, as a run reads it:

| Row | Means |
|---|---|
| `version X verified on D` | the installed version is the one the module's form and reader were checked against (`Verified`) |
| `[warn] version X is newer than the verified Y` | the CLI moved on since; `apb doctor --probe-agent <id>` checks its output |
| `[warn] help does not list <flags>` | a flag of the built-in launch, resume, autonomy, soul, fresh-session or profile-mode form is missing from the text the agent's help arguments print: the built-in form may have changed |
| `custom invocation; output read as plain text ...` | `agents.<id>.invocation` replaces the built-in form; its help is not checked |

The help runs the way detection probes run: sanitized environment, the
filesystem root as the working directory, a timeout. Its stdout and stderr
are both read. A version that leaves out trailing segments counts them as 0
(`2.1` is `2.1.0`).

`apb doctor --probe-agent <id> [--model <m>]` is the paid check: it prints a
cost notice, sends one short prompt through the agent's form without
autonomy, from an empty temporary directory, with a timeout, and checks the
output against `promises()` (reply, session id, usage, closing record) and
`agents::conformance`. The form and program come from the config under the
name given (an alias reads its own entry), and the spawn is a run step's:
connector-referenced variables are withheld, codex gets an isolated home and
zcode its provider config inside the probe's directory, and claude its
minimal-environment settings. It names the autonomy flags it left out,
prints the usage and cost the agent reported, and exits 1 when the format
drifted. Ctrl-C (or SIGTERM) stops the agent and removes the directory. A
run step is refused (`APB_RUN_ID` or `APB_HOST_TASK` set): only a person
spends that call.

## Checking a profile's models before a run

`apb validate` (whole project), `apb doctor` and the adoption report
(`apb adopt`, MCP `playbook_adopt_report`) judge every model of a profile's
executor chain with one function, `apb_core::model_check`:

| Finding | Means | validate | doctor | adopt |
|---|---|---|---|---|
| `model_not_allowed` | a zcode model outside `GLM-5.3` / `GLM-5.3-Flash` | error (`zcode_model_not_allowed`) | fail | finding |
| `model_policy_violation` | the config's `model_policy` does not allow it | error | fail | finding |
| `model_unknown` | outside apb's closed list for claude, codex or zcode (a typo, a retired id); claude's own aliases (`opus`, `sonnet`, `haiku`, `fable`, `opusplan`, `default`) and `[1m]` variants count as known | warning | warn | finding |
| `model_new_in_family` | not on apb's claude list, but a well-formed id of a known Claude family: `claude-<family>-<major>-<minor>` or `claude-<family>-<major>` with family `opus`, `sonnet`, `haiku` or `fable`, optionally a `-YYYYMMDD` date or `[1m]` (`claude-sonnet-6-0`, `claude-opus-6`); most likely a model newer than this apb, so it is accepted (a claude that is not installed still reads `agent_not_installed`) | info | ok | finding |
| `model_not_available` | the installed agent lists its models with Full authority (`opencode models`) and this one is missing | warning | warn | finding |
| `agent_not_installed`, `model_unverifiable` | nothing on this machine can confirm it | - | agent check | finding |

apb's claude list (`claude_static_models` in the models table) names the
current models first, Opus 5.5 (`claude-opus-5-5`, the profile editor's
default), Sonnet 5.5 (`claude-sonnet-5-5`), Haiku 5.5 (`claude-haiku-5-5`) and
Fable 5.1 (`claude-fable-5-1`), then the predecessors still served
(`claude-sonnet-5`, `claude-haiku-4-5-20251001`). A user overlay
(`<config_dir>/models.yaml`) can replace the list.

### Model policy

The global `config.yaml` can restrict which models profiles may use, for
for example to keep routine work on a smaller model:

```yaml
model_policy:
  - agent: claude
    allow: ["*sonnet*", "*haiku*"]
    reason: keep routine work on a smaller model
  - agent: opencode
    when: "anthropic/*"       # only the Anthropic models of opencode
    allow: ["*haiku*"]
```

A rule covers one agent (`claude-code` counts as `claude`) and, with `when`,
only the models matching that glob. A covered model must match one of the
`allow` globs. Globs match case-insensitively, against the model as written and,
for zcode, its canonical bare id, with any `@effort` suffix ignored, so
`claude-sonnet-5-5` passes the claude rule above and `claude-opus-5-5` does not,
and a zcode rule `allow: [GLM-5.3]` accepts `zai-individual/GLM-5.3@low` but
not `GLM-5.3-Flash`. A glob that does not compile makes the config invalid. The policy is
enforced where profiles are checked: `apb validate` fails on a violation,
`apb doctor` reports it as a failure, and the adoption report lists it. It is
the user's own rule, so it lives only in the global config, not in a project's
`.apb/config.yaml`.

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
  `@high` or `@max`. Omitted, apb fills in `low`. ZCode's own default is the
  model's highest level, `max`, which made medium tasks take many minutes; on
  a realistic review task `low` found the same bug as `high` and `max` in well
  under a minute instead of seven to nine, with a third to a half of the
  tokens. Write `@high` or `@max` in a profile that needs deeper reasoning.
  There is no profile-level effort field; the suffix is zcode-only.
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
commands are refused: effectively read-only), and for an authorized effectful
run the profile's `zcode_mode` (appended last, the last value wins): `yolo` by
default (files, shell, network), or `edit` when the profile sets
`zcode_mode: edit` (file edits only; shell commands stay refused, so the
playbook runs builds and tests in its own script nodes). `zcode_mode` is set
in `profile.yaml`, by MCP `profile_write` (`zcode_mode: "edit"`) or
`apb profile write --zcode-mode edit`; an update that does not name it keeps the
stored value. Other agents ignore it. The CLI has no `--model`
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

Desktop history (`ui_sync`, opt-in). Sessions the headless CLI runs are not
listed in the ZCode desktop app: the desktop lists sessions from its own task
index (`~/.zcode/v2/tasks-index.sqlite`), which the CLI does not write. To see
apb's zcode sessions there, turn the sync on in the global config:

```yaml
agents:
  zcode:
    ui_sync: true
```

After every successful zcode attempt apb then inserts one row for the session
into that index, shaped like the rows the desktop writes (provider `glm`, the
model as `<plan provider id>/<model>`, the effort, the `--mode` the session ran
with, title `apb: <node> (<run id>)`). The row is filed under the workspace the
way the desktop files it:

- under WSL, `remote:wsl:<distro>:<user>:<path>`, the identity of a project the
  desktop opened through its WSL remote connection; the distro and user are
  read at runtime (`$WSL_DISTRO_NAME`, else `wslpath -w /`; the process user).
  A project the desktop opened by its `\\wsl.localhost\...` path is a local
  Windows workspace with its own index on the Windows side, which apb does not
  write, so the session shows only under a project opened as a WSL remote;
- anywhere else, the plain working directory path, the key of a local project.

The write is best effort: apb opens the existing index only (it never creates
one or changes its schema), takes SQLite's write lock the way the desktop does,
retries a few times while the desktop holds it, and uses `INSERT OR IGNORE`,
so a row the desktop already has (a resumed session, a renamed title) is never
changed. Any failure (no index, a busy or changed database, a WSL host whose
distro cannot be told) is a warning on the run's stderr; the step's result is
never affected. Other agents ignore `ui_sync`.

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
