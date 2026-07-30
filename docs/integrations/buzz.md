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
