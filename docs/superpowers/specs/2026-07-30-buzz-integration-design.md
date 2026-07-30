# apb x Buzz Integration Design

Status: draft, phase 2 decision deferred. Date: 2026-07-30.

## Context

[Buzz](https://github.com/block/buzz) is Block's Nostr-relay-based workspace
where humans and AI agents collaborate in shared channels as first-class
peers. Each participant, human or agent, holds its own Nostr keypair,
memberships, and audit trail. It ships as a desktop app (Tauri) plus a relay
server and a family of Rust crates (`buzz-relay`, `buzz-cli`, `buzz-acp`,
`buzz-agent`, `buzz-persona`, `buzz-dev-mcp`). Apache 2.0, launched
2026-07-21, currently v0.5.x and moving fast (three releases in the launch
week, including a breaking change to harness registration).

Facts verified on 2026-07-30 (repo source, local binary inspection, docs):

- **Buzz does not expose an MCP server.** Nothing can connect to Buzz over
  MCP. Its own protocol is Nostr: REST endpoints (`/events`, `/query`,
  `/count`, `/hooks/{id}`) plus a WebSocket feed, every request signed with
  the caller's Nostr key (NIP-98, Schnorr). There is no API-key scheme.
- **The `buzz` CLI is the supported programmatic surface.** Agent-first
  design: `--format json|compact`, documented exit codes (0 ok, 1 bad input,
  2 relay/network, 3 auth, 4 other, 5 write conflict), JSON errors on stderr.
  Configured via `BUZZ_RELAY_URL`, `BUZZ_PRIVATE_KEY`, optional
  `BUZZ_AUTH_TAG`. Installed locally as part of Buzz.app.
- **Agents attach over ACP, not MCP.** The `buzz-acp` bridge turns channel
  @mentions into `session/prompt` calls to an agent subprocess over the Agent
  Client Protocol (JSON-RPC over stdio). Supported harnesses today: goose,
  Claude Code, codex; since v0.5.0 a generic "bring your own harness" (BYOH)
  catalog replaces per-harness registrations.
- **MCP enters only on the agent side, via persona packs.** A persona pack
  declares MCP servers in a pack-level `.mcp.json` (Claude-Code-style
  `mcpServers` map) merged with per-persona frontmatter `mcp_servers`
  entries; the merged config is delivered to the agent runtime through ACP
  `NewSessionRequest.mcp_servers`. No file is written to the agent's working
  directory. Transports: stdio and streamable_http only (SSE rejected).
- **Ecosystem practice confirms the pattern.** Every concrete integration
  found from launch week (Hermes agent, community bridge daemons, the
  OpenClaw/Claude-Agent-SDK write-up) is either an ACP subprocess or a direct
  relay participant using the CLI/REST surface. Nobody built an MCP-shaped
  integration with Buzz itself, because there is nothing to build it on.

The Hermes integration (hermes-agent.nousresearch.com/docs/integrations/buzz)
is the closest prior art: it offers three parallel paths, (1) Buzz Desktop
spawning Hermes as a managed ACP subprocess, (2) `buzz-acp` bridging a
channel to `hermes acp` on a server, and (3) a native gateway plugin where
Hermes connects outbound to the relay over an authenticated Nostr WebSocket.
Path 3 is their "deepest" integration and required a dedicated Nostr keypair
and custom Nostr client code.

## The question this doc answers

Should apb integrate with Buzz through the apb connector layer or through a
direct integration, and in which direction: Buzz driving apb, apb reporting
into Buzz, or both.

Answer: phased. Phase 1 needs no apb code at all and covers both directions
at once. The connector-versus-notifier fork only matters for phase 2, and the
decision is deliberately deferred until phase 1 has produced real usage data
and the Buzz API has stabilized.

## Goals

- Let a Buzz-resident agent start, supervise, and review apb playbook runs
  from a channel conversation.
- Let run progress, human-review gates, and final reports surface in a Buzz
  channel where humans see them.
- Start local (own relay, Buzz Desktop, single user), keep the hosted
  community scenario (`*.communities.buzz.xyz` or self-hosted relay for a
  team) reachable without redesign.

## Non-goals (for now)

- Buzz as an executor of `agent_task` nodes (an apb-engine adapter speaking
  ACP/BYOH). Recorded under Future work.
- Any change to apb's trust, policy-gate, or manifest model.
- Supporting hosted relays as the primary target in phase 1. A hosted-relay
  auth regression was observed in the wild during launch week (NIP-42
  `restricted: not a relay member` appearing without client-side changes,
  block/buzz#2663), so hosted reliability is not yet a foundation to build
  on.

## Phase 1: apb-operator persona pack (zero apb code)

A Buzz persona pack that gives a Buzz-managed agent the apb MCP server. The
persona becomes an apb operator living in a channel: it discovers playbooks,
starts runs on request, supervises them with the `supervisor_*` tools, and
narrates progress in the channel itself. In this shape "apb reports to Buzz"
needs no engine work, because the reporter is the agent, and the channel is
already its mouth.

### Shape

```
apb-operator-pack/
  .plugin/plugin.json        # pack manifest (id, version, personas, mcp_config)
  .mcp.json                  # {"mcpServers": {"apb": {"command": "apb", "args": ["mcp", ...]}}}
  agents/apb-operator.persona.md   # frontmatter + SOUL-like system prompt
  instructions.md
```

- The `.mcp.json` entry points at the installed `apb` binary's MCP mode over
  stdio. Buzz delivers it to the harness via ACP `session/new.mcp_servers`;
  apb-mcp already speaks stdio and negotiates old-revision hosts, so nothing
  changes on the apb side.
- Harness choice: Claude Code or goose, whichever is configured in Buzz
  Desktop. Both are confirmed MCP-capable ACP harnesses.
- The persona prompt encodes the operating rules: call `playbook_catalog`
  before acting, never start a run without an explicit go-ahead from the
  channel owner, relay every `pending_review` into the channel in the user's
  language and record the answer with `review_decide`, post progress at
  meaningful transitions rather than every event.

### Human-review gates over chat

This is the natural fit of the whole design. The apb MCP contract already
requires the supervising agent to relay a `pending_review` instruction with
its options and record the decision via `review_decide`. In a Buzz channel
that contract becomes: the persona posts the gate as a channel message, the
human replies in the channel, the persona calls `review_decide`. No new
mechanism on either side.

### Known constraints to handle in the pack, not in code

- **Project scoping.** The Buzz agent workspace lives under `~/.buzz/...`,
  while apb-mcp resolves playbooks per project. The persona must either pass
  an explicit project path or use `projects_list` to pick the workspace; the
  pack's instructions pin the default project.
- **Trust policy.** apb refuses drafts and untrusted playbooks. The persona
  does not get a bypass; trust is established out of band (by the owner, in
  the project) before channel-driven runs work. This is a feature, not a
  limitation.
- **Secrets.** `BUZZ_PRIVATE_KEY` belongs to the Buzz side of the fence and
  never appears in apb config, prompts, or logs. The apb side has its own
  rule already: auth files are never returned, logged, or cached.
- **Version drift.** The locally installed Buzz is 0.4.22 while upstream is
  0.5.2; persona packs declare `engines.buzz` constraints. The pack should
  pin the engine floor it was tested against, and the recipe should say to
  update Buzz first.

### Deliverables

- An example pack under `examples/buzz/apb-operator-pack/` (or a docs-only
  listing if examples/ is not the right home, to be decided at review).
- A recipe page `docs/integrations/buzz.md`: prerequisites, pack
  installation, harness selection, project pinning, the review-gate flow,
  and a troubleshooting section seeded with the launch-week field pitfalls.

### Exit criteria for phase 1

Phase 1 is done when a channel conversation can, on the local relay: list
playbooks, start an approved run, watch progress messages appear, answer a
human-review gate in the channel, and receive the final report, all without
touching a terminal.

## Phase 2: native outbound reporting (decision deferred)

Phase 1 reporting depends on the persona being awake and attentive. A native
outbound path makes run events reach a channel even when no agent is
supervising, or when the run is driven from a terminal or CI. Two candidate
shapes, decision after phase 1:

### Option A: Buzz connector in apb-engine

Buzz becomes a regular apb connector bindable to nodes (send message, create
issue, update canvas). Architecturally consistent: accounts, grants, policy
warnings, and the manifest snapshot all apply unchanged.

Cost: the connector layer authenticates HTTP calls with account credentials;
Buzz requires a Schnorr signature over every request (NIP-98, a signed
`kind:27235` event in a header). That is a new auth kind in
`apb-engine/connector/call/`, a secp256k1 dependency, and a signing path
that must never log the private key. Nontrivial, and it hard-binds apb to a
pre-1.0 wire protocol.

### Option B: CLI notifier

The engine (or a small sidecar) shells out to the `buzz` CLI to post run
lifecycle events to a configured channel. The CLI is designed for exactly
this (JSON in/out, stable exit codes) and absorbs all Nostr signing.

Cost: a new mechanism in the engine that is neither a connector nor an
adapter, with weaker policy integration (no accounts/grants model), plus a
runtime dependency on an installed, compatible `buzz` binary.

### Decision criteria (evaluate after phase 1)

- Did phase 1 usage actually need unattended reporting, and from which
  driver (MCP-supervised, CLI foreground, background supervisor)?
- Has the Buzz API stabilized (1.0 or a stated compatibility promise)? A
  native connector before that is churn.
- Does apb want NIP-98 as a general connector auth kind (other Nostr-based
  services would inherit it), or is Buzz a one-off? A general auth kind
  strengthens the case for Option A.
- Weight of the extra dependency: secp256k1 in-tree (A) versus a required
  external binary (B).

## Future work (out of scope)

- **Buzz as executor.** An apb-engine adapter that drives a Buzz-registered
  harness over ACP/BYOH, making a Buzz agent an executor binding for
  `agent_task` nodes like claude/cursor today. Deepest and most expensive
  path; revisit only with a concrete need.
- **Engram-backed cross-run state.** `buzz mem` (NIP-AE engrams) offers
  persistent slug-keyed memory on the relay; a possible shared-state surface
  for multi-run workflows once phase 2 exists.
- **Hosted deployment guide.** Identity management (dedicated keypair per
  agent, `BUZZ_AUTH_TAG` owner attestation), uptime (the "agent that only
  exists while one laptop is awake" problem), and the hosted-relay auth
  behavior, once it is stable.

## Risks

- **API instability.** Pre-1.0, breaking changes weekly (BYOH replaced
  per-harness registration within seven days of launch). Mitigation: phase 1
  touches only the persona-pack contract, which is spec-documented
  (`PERSONA_PACK_SPEC.md`) and cheap to re-pin.
- **Hosted relay reliability.** The launch-week NIP-42 regression on
  `*.communities.buzz.xyz` hit exactly the agent-attestation path.
  Mitigation: local relay first; hosted is a later, separate milestone.
- **Field-observed gaps** (from launch-week integration reports, relevant
  mainly if apb ever becomes a direct relay participant): no whoami-style
  CLI call (an agent must derive its own pubkey), `--format compact` drops
  `pubkey` and `tags` (unusable for mention loops), no headless path to
  publish the agent-directory entry, external agents excluded from @-mention
  autocomplete.
- **Silent-failure mode of ACP agents.** Buzz agents reply by shelling out
  to `buzz messages send`; a harness without shell access, or with permission
  prompts enabled, fails silently (typing indicator, no message). The recipe
  must call this out for the chosen harness.

## References

- https://github.com/block/buzz (README, ARCHITECTURE.md,
  crates/buzz-persona/PERSONA_PACK_SPEC.md, crates/buzz-acp/README.md,
  crates/buzz-agent/src/mcp.rs, docs/MCP_DRIVEN_HOOKS.md)
- https://hermes-agent.nousresearch.com/docs/integrations/buzz
- block/buzz#2663 (external-agent field reports and hosted-relay auth
  regression), block/buzz#2535/#2536/#2546/#2914 (per-harness registrations
  superseded by BYOH in v0.5.0)
- https://blog.darrenjrobinson.com/block-buzz-agent-integration-with-openclaw-claude-code-agents/
  and https://github.com/darrenjrobinson/buzz-acp
