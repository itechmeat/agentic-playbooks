# Buzz apb-operator Persona Pack Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship phase 1 of the Buzz integration (spec: `docs/superpowers/specs/2026-07-30-buzz-integration-design.md`): an example Buzz persona pack that gives a Buzz-managed agent the apb MCP server, plus a recipe doc. Zero changes to apb code.

**Architecture:** A persona pack under `examples/buzz/apb-operator-pack/` following Buzz's `PERSONA_PACK_SPEC.md`: `.plugin/plugin.json` manifest, one persona (`apb-operator`) whose system prompt encodes the operating rules, a pack-level `.mcp.json` that launches `apb mcp` through a `sh -c` working-directory wrapper, and pack instructions. A recipe at `docs/integrations/buzz.md` covers install, project pinning, the review-gate flow, and field-observed pitfalls. Validation is `buzz pack validate` / `buzz pack inspect` (both local-only, no relay).

**Tech Stack:** JSON + Markdown/YAML only. Local `buzz` CLI (Buzz.app 0.4.22, `~/.local/bin/buzz`) for validation. Installed `apb` 0.12.0 for the MCP smoke check.

## Global Constraints

- No em-dashes (U+2014) and no exclamation marks anywhere in the new files. No CJK. All new files are English.
- Work on a feature branch (suggested: `feat/buzz-operator-pack`), never on `main`.
- Commits use `--signoff` and end with `Co-Authored-By: <acting model> <noreply@anthropic.com>`.
- Never push, publish, or upload anything; the owner pushes and opens the PR.
- Do not touch the unrelated untracked dirs `.apb/playbooks/branch-quality-review/`, `.apb/profiles/branch-reviewer/`, `docs/reviews/`.
- Persona frontmatter may contain ONLY keys from the spec (`name`, `display_name`, `avatar`, `description`, `version`, `author`, `skills`, `mcp_servers`, `subscribe`, `triggers`, `model`, `temperature`, `max_context_tokens`, `thread_replies`, `broadcast_replies`, `hooks`); unknown keys are hard validation errors.
- Do not use `${VAR}` interpolation inside `.mcp.json` values; Buzz documents it but has not implemented it. Literal values only.
- Omit `engines.buzz` from the manifest (local CLI reports no version; the constraint would guess). The recipe documents version expectations in prose instead.
- After every change to the pack, `buzz pack validate examples/buzz/apb-operator-pack` must print `Valid.` and exit 0.
- No Rust code changes, so the code-ranker and clippy gates do not apply; the fmt gate does not apply to JSON/Markdown.

## File Structure

- Create: `examples/buzz/apb-operator-pack/.plugin/plugin.json` (pack manifest)
- Create: `examples/buzz/apb-operator-pack/agents/apb-operator.persona.md` (persona: frontmatter + system prompt)
- Create: `examples/buzz/apb-operator-pack/.mcp.json` (pack-level MCP servers: apb)
- Create: `examples/buzz/apb-operator-pack/instructions.md` (pack instructions)
- Create: `examples/buzz/apb-operator-pack/README.md` (30-second orientation, points to the recipe)
- Create: `docs/integrations/buzz.md` (the recipe; new `docs/integrations/` directory)

---

### Task 1: Pack skeleton, manifest, persona

**Files:**
- Create: `examples/buzz/apb-operator-pack/.plugin/plugin.json`
- Create: `examples/buzz/apb-operator-pack/agents/apb-operator.persona.md`

**Interfaces:**
- Consumes: nothing (first task).
- Produces: a pack directory that `buzz pack validate` accepts; manifest fields `id: io.github.itechmeat.apb-operator`, `mcp_config: .mcp.json` (file added in Task 2), `pack_instructions: instructions.md` (file added in Task 3). Later tasks must create exactly those filenames.

- [ ] **Step 1: Create the manifest**

Write `examples/buzz/apb-operator-pack/.plugin/plugin.json`:

```json
{
  "$schema": "https://open-plugin-spec.org/schema/v1/plugin.json",
  "id": "io.github.itechmeat.apb-operator",
  "name": "APB Operator",
  "version": "0.1.0",
  "description": "A single-agent pack that operates apb (agentic-playbooks) from a Buzz channel: starts approved playbook runs, supervises them, relays human-review gates into the channel.",
  "license": "MIT",
  "homepage": "https://github.com/itechmeat/agentic-playbooks",
  "keywords": ["apb", "playbooks", "mcp", "automation"],
  "personas": [
    "agents/apb-operator.persona.md"
  ],
  "pack_instructions": "instructions.md",
  "mcp_config": ".mcp.json",
  "defaults": {
    "model": "anthropic:claude-sonnet-5",
    "temperature": 0.3,
    "triggers": { "mentions": true, "keywords": [], "all_messages": false },
    "thread_replies": true,
    "broadcast_replies": false
  }
}
```

- [ ] **Step 2: Run validation, expect failure**

Run: `buzz pack validate examples/buzz/apb-operator-pack`
Expected: non-zero exit, error about the missing persona file `agents/apb-operator.persona.md` (and possibly the missing `instructions.md` / `.mcp.json` pointers; note what it reports, the pointers may be lazily resolved).

- [ ] **Step 3: Write the persona**

Write `examples/buzz/apb-operator-pack/agents/apb-operator.persona.md`:

```markdown
---
name: apb-operator
display_name: "APB Operator"
description: "Operates the apb playbook runner over MCP: starts approved runs, supervises them, relays review gates into the channel."
triggers:
  mentions: true
  keywords:
    - playbook
    - apb
temperature: 0.3
---

You are the APB Operator, the bridge between this Buzz workspace and the apb playbook runner connected over MCP (server name: apb).

## What you do

- Discover and describe available playbooks on request.
- Start playbook runs after an explicit go-ahead, and supervise them to the end.
- Report run progress in the channel and relay every human-review gate.

## Operating rules

1. Call playbook_catalog once per task that names a doable action, before acting.
2. Never start a playbook run without an explicit go-ahead in the channel. First describe the playbook (name, version, declared effects), then wait for a clear yes before calling playbook_run.
3. Supervise every run you start: request supervision when starting the run and keep following it with supervisor_wait_event until the run reaches a terminal state.
4. The moment run_status, supervisor_wait_event, or supervisor_run_inspect returns pending_review, relay the review instruction into the channel in the owner's language, together with the options. Record the owner's answer with review_decide. The run stays frozen until then; repeat the relay while the gate stays pending.
5. Post progress when a node starts, finishes, or fails, and when the run finishes. Do not post every event.
6. Never paste secrets, tokens, auth file contents, or private prompt content into the channel. apb never returns secret values; do not try to work around that.
7. When a run fails, report the failing node and a short error summary, then ask whether to retry the node, resume the run, or abort. Call the matching supervisor tool only after the owner answers.
8. If apb refuses a playbook as a draft or as untrusted, report that verbatim and stop. Trust is granted by the owner outside this channel; never acknowledge trust on the owner's behalf.
9. If playbook_list comes back empty, the MCP server was probably launched with the wrong working directory. Say so and point the owner at the APB_PROJECT_DIR value in this pack's .mcp.json. Do not attempt to fix it yourself.

## Style

Machine-facing tool arguments are English. Channel messages are written in the language the owner writes in. Keep channel messages short; the run report carries the detail.
```

- [ ] **Step 4: Run validation, expect the persona to resolve**

Run: `buzz pack validate examples/buzz/apb-operator-pack`
Expected: either `Valid.` with exit 0, or errors only about the still-missing `instructions.md` / `.mcp.json` pointers. If the pointers are eagerly checked, create both as empty stubs now (`{ "mcpServers": {} }` and an empty heading `# APB Operator pack instructions`) so validation passes; Tasks 2 and 3 replace the stubs with real content.

- [ ] **Step 5: Inspect**

Run: `buzz pack inspect examples/buzz/apb-operator-pack`
Expected: exit 0; output shows pack id `io.github.itechmeat.apb-operator`, one persona `apb-operator`, resolved model `anthropic:claude-sonnet-5`, resolved temperature `0.3`, and a system-prompt snippet.

- [ ] **Step 6: Commit**

```bash
git add examples/buzz/apb-operator-pack
git commit --signoff -m "feat(examples): buzz apb-operator persona pack skeleton"
```

(Append the Co-Authored-By trailer per Global Constraints.)

---

### Task 2: MCP wiring

**Files:**
- Create or replace: `examples/buzz/apb-operator-pack/.mcp.json`

**Interfaces:**
- Consumes: manifest pointer `mcp_config: .mcp.json` from Task 1.
- Produces: an MCP server entry named `apb` that Buzz delivers to the harness via ACP `session/new.mcp_servers`. The recipe (Task 4) documents editing `APB_PROJECT_DIR`.

**Background for the implementer:** `apb mcp` takes no flags; the served project is the process working directory at launch, verified in `crates/apb-cli/src/main.rs` (`std::env::current_dir()`) and `docs/MCP.md`. Buzz's pack-level `.mcp.json` entries carry only `command`, `args`, `env` (no `cwd` key), so the working directory is set by a `sh -c` wrapper reading a literal env var.

- [ ] **Step 1: Write the MCP config**

Write `examples/buzz/apb-operator-pack/.mcp.json`:

```json
{
  "mcpServers": {
    "apb": {
      "command": "sh",
      "args": ["-c", "cd \"$APB_PROJECT_DIR\" && exec apb mcp"],
      "env": {
        "APB_PROJECT_DIR": "/absolute/path/to/your/apb/project"
      }
    }
  }
}
```

The path value is deliberate user configuration: the recipe instructs the operator to replace it with their project directory before loading the pack. Do not substitute a machine-specific path in the committed example.

- [ ] **Step 2: Validate**

Run: `buzz pack validate examples/buzz/apb-operator-pack`
Expected: `Valid.`, exit 0.

- [ ] **Step 3: Smoke-check the wrapper shape against a real project**

Run (from the repo root, which is a real apb project):

```bash
APB_PROJECT_DIR="$PWD" sh -c 'cd "$APB_PROJECT_DIR" && exec apb mcp' <<'EOF'
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"pack-smoke","version":"0.0.0"}}}
EOF
```

Expected: a single JSON-RPC response on stdout containing `"serverInfo"` and a non-empty `"instructions"` string; the process exits when stdin closes. This proves the exact command line the pack ships actually boots apb's MCP server in the right directory.

- [ ] **Step 4: Commit**

```bash
git add examples/buzz/apb-operator-pack/.mcp.json
git commit --signoff -m "feat(examples): wire apb mcp server into the buzz pack"
```

---

### Task 3: Pack instructions and README

**Files:**
- Create or replace: `examples/buzz/apb-operator-pack/instructions.md`
- Create: `examples/buzz/apb-operator-pack/README.md`

**Interfaces:**
- Consumes: manifest pointer `pack_instructions: instructions.md` from Task 1.
- Produces: pack-level instructions appended to every persona in the pack; README points readers to `docs/integrations/buzz.md` (created in Task 4, link is forward-referencing and expected).

- [ ] **Step 1: Write the pack instructions**

Write `examples/buzz/apb-operator-pack/instructions.md`:

```markdown
# APB Operator pack instructions

This pack connects Buzz personas to one local apb (agentic-playbooks) project over MCP.

- The apb MCP server serves exactly one project: the directory named by APB_PROJECT_DIR in this pack's .mcp.json. All playbook, profile, and run tools operate on that project. Reads across other registered workspaces are possible through projects_list plus the workspace parameter, but runs belong to the pinned project.
- Trust is established outside this pack. When apb refuses a draft or untrusted playbook, that refusal is correct behavior; report it and stop.
- Runs survive the conversation. Run state lives in the project's runs/ directory; an interrupted run can be resumed with run_resume once the interruption is resolved.
- Never place secret values in channel messages, run instructions, or tool arguments.
```

- [ ] **Step 2: Write the README**

Write `examples/buzz/apb-operator-pack/README.md`:

```markdown
# APB Operator pack for Buzz

A single-persona Buzz pack that turns a channel into an apb control room: ask the agent to list playbooks, approve a run, watch progress, and answer human-review gates without leaving the chat.

Setup, project pinning, and troubleshooting live in the recipe: [docs/integrations/buzz.md](../../../docs/integrations/buzz.md).

Quick check after editing the pack:

    buzz pack validate examples/buzz/apb-operator-pack
    buzz pack inspect examples/buzz/apb-operator-pack

Both commands are local-only and need no relay.
```

- [ ] **Step 3: Validate and inspect**

Run: `buzz pack validate examples/buzz/apb-operator-pack` then `buzz pack inspect examples/buzz/apb-operator-pack`
Expected: `Valid.` exit 0; inspect exit 0 with the same resolved persona as Task 1 Step 5.

- [ ] **Step 4: Commit**

```bash
git add examples/buzz/apb-operator-pack/instructions.md examples/buzz/apb-operator-pack/README.md
git commit --signoff -m "docs(examples): pack instructions and readme for buzz apb-operator"
```

---

### Task 4: Recipe doc

**Files:**
- Create: `docs/integrations/buzz.md`

**Interfaces:**
- Consumes: the pack path `examples/buzz/apb-operator-pack/` and the `APB_PROJECT_DIR` contract from Task 2.
- Produces: the user-facing recipe. The spec's phase 1 deliverable list names exactly this file.

- [ ] **Step 1: Write the recipe**

Write `docs/integrations/buzz.md`:

```markdown
# Using apb from Buzz

[Buzz](https://github.com/block/buzz) is Block's Nostr-based workspace where humans and AI agents share channels. This recipe installs the apb-operator persona pack (`examples/buzz/apb-operator-pack/`), which gives a Buzz-managed agent the apb MCP server so playbook runs can be started, supervised, and reviewed from a channel.

Design background and the longer-term plan live in `docs/superpowers/specs/2026-07-30-buzz-integration-design.md`.

## What you get

- A persona, APB Operator, that lists playbooks, starts runs only after your explicit go-ahead, and posts progress in the channel.
- Human-review gates delivered as channel messages: the persona relays the gate, you answer in the channel, it records the decision.
- apb's policy gate stays intact: drafts and untrusted playbooks are refused, and the persona will not work around that.

## Prerequisites

- Buzz Desktop installed, with an agent harness configured that supports MCP over ACP (Claude Code or goose). This pack was authored against the persona pack spec as of Buzz v0.5.x; Buzz is pre-1.0 and moves fast, so re-validate the pack after Buzz upgrades.
- apb installed and on PATH (`apb --version` should print 0.12.0 or newer).
- An apb project with at least one trusted playbook. Trust is granted in the project (by you), not from the channel.

## Setup

1. Copy `examples/buzz/apb-operator-pack/` somewhere stable, or use it in place.
2. Edit `.mcp.json` in the pack: replace `/absolute/path/to/your/apb/project` with the absolute path of your apb project (the directory that contains `.apb/`).
3. Validate: `buzz pack validate <path-to-pack>` should print `Valid.`
4. Optionally inspect the resolved persona: `buzz pack inspect <path-to-pack>`.
5. Load the pack in Buzz Desktop. As of the version this recipe was written against, the CLI has no `pack install` subcommand; pack loading happens through the desktop app. If your Buzz version differs, check `buzz pack --help` and the Buzz README for the current mechanism.
6. In a channel, mention the persona and ask it to list playbooks. The first successful `playbook_list` proves the whole chain: pack, ACP delivery, MCP server, project pinning.

## The review-gate flow

When a run hits a `human_review` node, the persona posts the gate's instruction and options into the channel and waits. Reply in the channel; the persona records your decision with `review_decide` and the run continues. Until you answer, the run stays frozen and the persona re-posts the gate if asked about status.

## Troubleshooting

- **The persona says the project is empty.** The MCP server is running in the wrong directory. Fix `APB_PROJECT_DIR` in the pack's `.mcp.json`; it must be the project root containing `.apb/`.
- **The apb server does not start.** GUI-spawned processes on macOS often have a minimal PATH that misses `~/.cargo/bin`. Replace `"exec apb mcp"` in `.mcp.json` with the absolute binary path, for example `"exec /Users/you/.cargo/bin/apb mcp"`.
- **The agent replies with a typing indicator but no message appears.** A known Buzz-side failure mode: harnesses post channel replies by shelling out to the `buzz` CLI, so a harness without shell access, or with permission prompts enabled, fails silently. Check the harness settings in Buzz Desktop.
- **Runs refuse to start with a trust error.** Expected: the playbook is a draft or untrusted in the pinned project. Trust it from the project side (owner action), then ask again.
- **Hosted relays.** This recipe targets a local relay. Hosted communities showed an agent-auth regression during Buzz's launch week; treat hosted deployment as a separate, later step.

## Scope and secrets

- The pack pins one project. Cross-project reads exist (`projects_list` plus the `workspace` parameter on read-only tools), but runs are scoped to the pinned project.
- `BUZZ_PRIVATE_KEY` and other Buzz credentials belong to Buzz's side of the fence: they never appear in apb configuration, prompts, or logs. apb's own rule is symmetric: secret values are never returned by its tools.
```

- [ ] **Step 2: Hygiene gate over all new files**

Run: `grep -rnP '\x{2014}|!' examples/buzz docs/integrations/buzz.md; echo "exit=$?"`
Expected: `exit=1` (no matches). If grep lacks `-P` on this machine, run the two checks separately with `grep -rn $'—'` and `grep -rn '!'`.

- [ ] **Step 3: Final validation pass**

Run: `buzz pack validate examples/buzz/apb-operator-pack`
Expected: `Valid.`, exit 0.

- [ ] **Step 4: Commit**

```bash
git add docs/integrations/buzz.md
git commit --signoff -m "docs: buzz integration recipe for the apb-operator pack"
```

---

### Task 5: End-to-end dogfooding gate (owner-assisted, do not automate)

**Files:** none (verification only).

**Interfaces:**
- Consumes: everything above.
- Produces: the spec's phase 1 exit-criteria checklist, verified or annotated.

This task needs the Buzz Desktop GUI and per-run approvals, so the executing agent STOPS here and hands the checklist to the owner instead of driving it:

- [ ] Pack loads in Buzz Desktop without errors.
- [ ] In a channel, the persona lists playbooks from the pinned project.
- [ ] An approved run starts from the channel and progress messages appear.
- [ ] A human-review gate arrives as a channel message and a channel reply resolves it via `review_decide`.
- [ ] The final run report reaches the channel.
- [ ] Any Buzz-version mismatch or broken assumption found here goes back into `docs/integrations/buzz.md` (troubleshooting) before the branch is offered for merge.

Findings that belong to apb itself (not to this pack) are collected for the standing feedback loop (`docs/FEEDBACK-LOOP.md`), as usual.
