# Interactive nodes: agent capability matrix and upstream contribution plan

What apb needs from a CLI coding agent to support interactive nodes (nodes
that ask the user questions mid-run, spec:
`docs/superpowers/specs/2026-07-20-interactive-nodes-design.md`), where each
agent stands today, and which upstream contributions we plan so more agents
can reach the best tier.

Status: research snapshot 2026-07-20, pre-implementation. UPDATE THIS
DOCUMENT after the interactive-nodes feature ships: re-verify the "needs
verification" rows against the real binaries, flip `interaction` defaults
that verification confirms, and turn the "planned" contribution rows into
links to filed issues and PRs.

## What an agent needs

apb spawns node agents headless and one-shot. To let such an agent ask the
user a question, apb uses one of three transports, best available first:

1. **live**: apb injects a one-tool stdio MCP server (`ask_user`) into the
   spawned agent; the tool call blocks until a human answers. Requires all
   three: (a) per-invocation MCP server injection in headless mode, (b) a
   tool-call timeout that is absent, hours-long, or configurable per run,
   and (c) survival of long-idle stdio calls (or progress-notification
   support that resets idle timers).
2. **resume**: the agent prints a question marker and exits; apb re-invokes
   it with the answer once a human replies. Requires headless session resume
   with full state, and the session id must be obtainable from a headless
   run's output.
3. **reprompt**: the floor. Fresh invocation carrying the full Q&A
   transcript in the prompt. Works with any agent, loses in-flight state.

MCP elicitation (spec 2025-06) would be the protocol-native fourth option,
but as of 2026-07 no surveyed agent ships it verified in headless mode.

## Capability matrix (2026-07 research, doc-based)

Agents apb supports today:

| agent | per-run MCP injection | tool-call timeout | headless resume | transport today |
|---|---|---|---|---|
| Claude Code | yes, `--mcp-config` (file or inline JSON) | default about 28 h (`MCP_TOOL_TIMEOUT`, per-server `timeout`); 30 min stdio idle timer, reset by progress notifications | yes, `--resume <session-id>` with `-p` | **live** |
| Codex CLI | config file only (`.codex/config.toml`); inline `-c` override for `mcp_servers.*` unconfirmed | `tool_timeout_sec`, default 60 s, config file only | yes, `codex exec resume <id>` | **resume** |
| OpenCode | no flag; `opencode.json` only, with an open project-scope detection bug | effectively hard-capped around 30-120 s regardless of config (open issues) | `--session <id>` / `--continue`, open "Session not found" headless bug | **resume** (needs verification) |
| Hermes Agent | not documented; `config.yaml` / `hermes mcp add` only | not documented | `--resume` / `--continue` documented; combination with `-z` one-shot unverified | **resume** (needs verification, until then reprompt) |
| Antigravity CLI | no; persistent config files only | not documented | no: `-p` never surfaces a conversation id (open upstream issue #7) | **reprompt** |

Popular agents apb does not support yet (candidates):

| agent | per-run MCP injection | tool-call timeout | headless resume | notes |
|---|---|---|---|---|
| GitHub Copilot CLI | yes, `--additional-mcp-config` | per-server `timeout` (ms), open bugs: ignored (#172), reverts to 180 s after list_changed (#1378) | yes, `--resume` / `--continue` | ships a native ask-the-human pause (`--no-ask-user` disables it) |
| Goose | yes, `--with-extension` on `goose run` | per-extension `timeout` in seconds, default 300 s, documented as raisable | recipes/sessions only, not plain `goose run -t` | positions itself as an MCP reference client; builds draft MCP features (elicitation) first |
| Amp | yes, `--mcp-config` (also skips workspace approval gate) | undocumented | yes, `amp threads continue/fork` | timeout ceiling needs empirical test |
| Factory Droid | config files only (`.factory/mcp.json`) | `timeoutMs` per server plus global `mcp.callTimeoutMs`, default about 60 s | yes, `droid exec -s <id>` / `--fork`, cleanest headless resume of the set | |
| Cursor CLI | config files only; per-run injection reported broken | undocumented; open reports of MCP hanging or not working non-interactively | `--resume=<chatId>` / `--continue` | not viable for live until headless MCP bugs are resolved |
| Gemini CLI | config files only | per-server `timeout`, default 600 s | not found for `-p` | Google announced retirement in favor of Antigravity CLI |
| Aider | no MCP support at all (open RFC) | n/a | n/a | out of consideration until MCP lands |

## Upstream contribution plan

The goal: contribute the missing pieces where repositories accept them, so
more agents can run interactive nodes at the `live` or `resume` tier. One
item per agent, ordered by expected impact for apb users. Each row moves to
"filed" with a link once we act on it, after the feature ships.

| # | project | contribution | kind | status |
|---|---|---|---|---|
| 1 | Antigravity CLI (google-antigravity/antigravity-cli) | Surface the conversation id from `--print` runs so headless callers can resume; issue #7 already tracks it and matches our need exactly. Add our use case and, if the CLI source is open to contributions, a PR implementing caller-supplied or emitted ids. | comment plus possible PR | planned |
| 2 | Hermes Agent (NousResearch/hermes-agent) | Document and, if missing, implement per-invocation MCP server injection for `-z` one-shot mode (a `--mcp-config`-style flag), document the MCP tool-call timeout, and document `-z` combined with `--resume`. Open source; PRs are realistic for all three. | PR | planned |
| 3 | OpenCode (anomalyco/opencode) | Make the MCP tool-execution timeout honestly configurable (issues #8701, #23096, #11584 describe the hard cap) and support per-run MCP injection on `opencode run` (feature request #10527). Fix or triage the headless `--session` "Session not found" bug (#28407). | PR plus issue comments | planned |
| 4 | Codex CLI (openai/codex) | Verify whether `-c mcp_servers.*` inline overrides work for a single `codex exec` run; if yes, contribute a docs example, if no, propose the capability. Propose an env-var override for `tool_timeout_sec` so a spawner can raise it without rewriting config.toml. | docs PR or feature issue | planned |
| 5 | GitHub Copilot CLI (github/copilot-cli) | Add our reproduction and use case to the MCP timeout bugs (#172, #1378); a reliable per-server timeout is the only thing between Copilot CLI and the `live` tier, since `--additional-mcp-config` already exists. Closed source: issues only. | issue comments | planned |
| 6 | Goose (block/goose) | Propose session resume for plain `goose run -t` one-shot runs (today only recipes/sessions resume). Goose already has the cleanest per-run injection and timeout story; this closes its only gap. | feature issue, then PR | planned |
| 7 | MCP ecosystem | When elicitation gains a headless story in any client, evaluate replacing the marker protocol with `elicitation/create` and report implementation experience upstream to the spec discussions. | tracking | watching |

Non-actions, recorded so we do not re-litigate them: Cursor CLI and Amp are
closed source with community forums as the only channel; we file nothing
until their headless MCP paths stabilize. Gemini CLI is being retired; no
investment. Aider needs MCP itself first; out of scope.

## Verification checklist (run after implementation)

- [ ] hermes: `-z` plus `--resume <session>` restores state; flip
      `interaction` to `resume` if confirmed.
- [ ] hermes: MCP tool-call timeout behavior under a blocked `ask_user`
      (empirical; docs are silent).
- [ ] opencode: headless `--session` resume reliability on macOS and Linux;
      keep `resume` or drop to `reprompt` accordingly.
- [ ] codex: `-c` inline override reaching `mcp_servers.*` tables for a
      single `exec` run.
- [ ] claude: 30-minute stdio idle timer actually reset by our progress
      notification cadence over a multi-hour block.
- [ ] Update the matrix and the contribution table statuses; convert
      "planned" rows to links.
