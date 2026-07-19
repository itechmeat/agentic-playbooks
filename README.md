# Agentic Playbooks - `apb`

Repeatable multi-step workflows for coding agents, defined in YAML and executed
locally. One binary: the execution engine, a visual web editor, and an MCP
server your agent can drive.

You already ask Claude Code (or another agent) to "plan, implement, lint, fix,
repeat" by hand. A playbook turns that into a versioned artifact: agent tasks,
scripts, conditions, review gates, and parallel branches wired into a graph
that runs the same way every time, survives restarts, and leaves a full event
log behind.

## Why `apb`

- **Playbooks as YAML.** Node types: `agent_task`, `script`, `condition`,
  `human_review`, `wait` (timer/webhook), parallel branches with `join`.
  Templating over params and node outputs, retries, fallbacks, timeouts.
- **Any coding agent, via profiles.** A profile binds agent + model + ordered
  fallbacks + role prompt + skills. Nodes reference profiles by name; swapping
  models never means editing the playbook.
- **Deterministic and resumable runs.** Append-only event log, immutable
  per-run snapshot of the playbook and profiles, `apb resume` after a crash.
- **Visual editor and live monitor.** `apb serve` opens a svelte-flow web UI:
  edit YAML and graph side by side, watch runs light up node by node.
- **Built for agents, not just humans.** `apb mcp` exposes the whole surface
  as MCP tools, so an agent can create, run, and supervise playbooks itself.
- **Self-improving playbooks.** A supervisor agent can watch a run, retry or
  reroute nodes, patch the playbook mid-run; successful patches are promoted
  into a new version.
- **Local-first.** No cloud, no accounts. State lives in `.apb/` next to your
  code; versions are immutable and diffable.

## Quick start

```sh
apb init          # create the .apb/ structure in your project
apb detect        # find installed coding agents, set up profiles
apb serve         # web UI at http://127.0.0.1:7321 - create a playbook
apb run <id>      # run it from the terminal
```

A playbook that plans, lints, and fixes until the linter passes:

```yaml
schema: 2
id: implement-task
name: Implement Task
version: 1.0.0

params:
  - { name: task, type: text, label: "Task" }

defaults:
  profile: architect

nodes:
  - { id: start, type: start, title: Start }
  - id: plan
    type: agent_task
    title: Plan
    prompt: |
      Write a plan: {{params.task}}
  - { id: lint, type: script, title: Lint, script: scripts/lint.sh, runner: sh }
  - { id: check, type: condition, title: Passed?, max_loops: 3 }
  - id: fix
    type: agent_task
    title: Fix
    prompt: |
      Fix: {{nodes.lint.output}}
  - { id: done, type: finish, outcome: success }

edges:
  - { from: start, to: plan }
  - { from: plan, to: lint }
  - { from: lint, to: check }
  - {
      from: check,
      to: done,
      condition: { type: node_status, node: lint, equals: success },
    }
  - {
      from: check,
      to: fix,
      condition: { type: node_status, node: lint, equals: failure },
    }
  - { from: fix, to: lint }
```

## Install

**The simplest path - let your agent install it.** Paste this into Claude Code,
Codex, OpenCode, or any AI agent with shell access:

> Install agentic-playbooks for me by following the instructions at
> <https://github.com/itechmeat/agentic-playbooks/blob/main/llms.txt>

The agent reads the `llms.txt`, clones the repo, builds the web frontend,
installs the binary, and verifies with `apb doctor`.

If you prefer running the steps yourself, the project is currently pre-release
and requires Rust and Bun (the web UI is embedded into the binary at build time):

```sh
git clone https://github.com/itechmeat/agentic-playbooks && cd agentic-playbooks
(cd web && bun install && bun run build)
cargo install --path crates/apb-cli
```

Note: `cargo install --git` is not supported; install from a local clone.

To update: `git pull`, rebuild `web/`, re-run `cargo install --path crates/apb-cli`.
To uninstall: `cargo uninstall apb-cli`. Project state in `.apb/` and global
config in `~/.config/apb/` are never touched by uninstall.

### Planned for v0.1.0

Prebuilt binaries (aarch64/x86_64 Apple darwin, x86_64 Linux GNU), a Homebrew
tap, and SHA256 checksums will be published alongside the first tagged release.
The release archive will include the `apb` binary and a copy of `LICENSE`.

```sh
# Planned (not yet available):
tar -xzf apb-aarch64-apple-darwin.tar.gz
mv apb /usr/local/bin/
apb --version

# Planned (not yet available):
brew install itechmeat/tap/apb
```

## Everyday commands

```text
apb init            create the .apb structure
apb list            playbooks and versions
apb validate        validate playbook schema
apb run <id>        run a playbook (--overrides, --supervise, params)
apb runs            list runs
apb resume <run>    resume a paused or interrupted run
apb review          decide a pending human_review node
apb serve           web UI (port 7321)
apb mcp             stdio MCP server for coding agents
```

Profiles and environment:

```text
apb detect          detect installed coding agents (local checks only)
apb profile         list / show / write / edit agent profiles
apb connector       list / show / call / approve / doctor / env / init - connectors to external services
apb migrate         migrate schema 1 playbooks (executors) to schema 2 (profiles)
apb adopt           adoption readiness report for a playbook
apb doctor          diagnose agents, profiles, runners, playbooks
apb export/import   move a playbook as a single bundle file
```

## Use from a coding agent (MCP)

`apb mcp` serves the current project's playbooks over stdio MCP. The server
identifies itself as `agentic-playbooks` to MCP clients. Claude Code:

```sh
claude mcp add agentic-playbooks -- apb mcp
```

or in the project's `.mcp.json`:

```json
{ "mcpServers": { "agentic-playbooks": { "command": "apb", "args": ["mcp"] } } }
```

For global scope (available in all projects), add `--scope user`:

```sh
claude mcp add --scope user agentic-playbooks -- apb mcp
```

Other MCP-compatible hosts (Codex, OpenCode, etc.): add a stdio MCP server
with command `apb` and args `["mcp"]`, using the name `agentic-playbooks`.

Tool families: playbook CRUD and validation, run control (`playbook_run`,
`run_status`, `run_events`, `run_report`, `run_resume`), profile and advisory
tools, and supervisor tools.

**Supervised runs.** `playbook_run` with `supervise: "self"` starts the run in
the background and hands the calling agent a supervisor token. The agent then
loops: `supervisor_wait_event` -> `supervisor_run_inspect` -> intervene if
needed (`supervisor_node_retry`, `supervisor_run_continue_from`,
`supervisor_patch_playbook`, `supervisor_run_abort`, ...) -> `supervisor_report`.
Alternatively, `apb run <id> --supervise` spawns a separate supervisor agent
with heartbeat monitoring and a fallback respawn. Capabilities are gated per
playbook policy. Details: [docs/MCP.md](docs/MCP.md).

## Status

Pre-release (`0.1.0`). The engine, web editor, versioning, MCP surface, and
supervisor are implemented and tested; the schema and CLI may still change
before the first tagged release. Design specs live in
[docs/superpowers/specs/](docs/superpowers/specs/).

## Development

```sh
cargo test --workspace        # engine, core, server, CLI tests
cd web && bun run test        # frontend tests (vitest)
cd web && bun run dev         # dev frontend with a proxy to :7321
apb dev                       # Vite HMR + API from the source tree
```

More: [docs/INSTALL.md](docs/INSTALL.md), [docs/MCP.md](docs/MCP.md),
[docs/PROFILES.md](docs/PROFILES.md), [docs/CONNECTORS.md](docs/CONNECTORS.md),
[docs/HOST-INTEGRATION.md](docs/HOST-INTEGRATION.md).

## Security

> [!WARNING]
> Playbooks can execute local scripts and invoke coding agents. Treat third-party
> playbooks and imported bundles as executable code. Review them before running,
> and do not expose `apb serve` or `apb mcp` to untrusted users or networks.

Please report suspected vulnerabilities privately as described in
[SECURITY.md](SECURITY.md).

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md).
All commits must be signed off under the [Developer Certificate of Origin](DCO).

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
