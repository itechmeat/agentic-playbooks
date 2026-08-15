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
    agent: claude            # one of the known agents (claude, codex, agy, opencode, pi, hermes, grok, cursor, qoder) or a configured one
    model: claude-opus-4-8   # exactly the string that agent's --model expects
    fallbacks:               # optional ordered chain; same role, different executor
      - { agent: codex, model: gpt-5.2-codex }
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
`--dangerously-skip-permissions` for agy,
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
