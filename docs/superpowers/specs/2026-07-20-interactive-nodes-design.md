# Interactive Nodes: agent_task nodes that ask the user questions mid-run

Date: 2026-07-20
Status: approved design, pre-implementation
Related: docs/superpowers/specs/2026-07-08-workflows-cli-design.md (run engine),
docs/superpowers/specs/2026-07-20-run-reliability-design.md (control channel,
detached driver, stop/resume semantics), docs/INTERACTIVE-AGENTS.md (agent
capability matrix and upstream contribution plan).

## Problem

During a supervised run, the user talks to the supervisor agent, not to the
node agents. Today the only way to influence a running node is to append a
note to the run context. Nodes whose value comes from dialogue (a brainstorm
node refining requirements, a design node resolving an ambiguity) cannot ask
the user anything: the node agent is a headless one-shot process with no
channel back to a human.

The feature: an `agent_task` node may be marked interactive. Its agent can
ask a question mid-execution; the run parks until an answer arrives; the
answer reaches the agent and execution continues. The user answers through
whichever facade they are actually at: the supervisor's chat, the CLI, or the
web UI.

## Research summary

Eleven CLI agents were surveyed (2026-07, web research with doc citations,
full matrix in docs/INTERACTIVE-AGENTS.md). Findings that shape the design:

- A long-blocking MCP tool call ("wait for a human for hours") is reliably
  supported only by Claude Code: per-run injection via `--mcp-config`, tool
  timeout default about 28 hours, plus a 30-minute stdio idle timer that
  progress notifications reset. Codex caps tool calls at 60 s by default and
  is configurable only through a config file; OpenCode hard-caps tool
  execution at roughly 30-120 s regardless of config; Hermes and Antigravity
  do not document timeouts at all.
- Headless session resume is the broadly available capability: claude
  (`--resume <id>`), codex (`exec resume <id>`), opencode (`--session <id>`,
  with open reliability bugs), hermes (`--resume`, interplay with `-z`
  unverified). Antigravity currently surfaces no conversation id from `-p`
  (open upstream issue), so it cannot resume at all.
- MCP elicitation, the spec-native mechanism for exactly this use case, is
  not shipped or not verified in headless mode in any surveyed agent.

Conclusion: no single transport works everywhere. The design separates an
invariant Q&A core from per-agent transports selected by capability.

## Design overview

One core, two transports, one answer path.

- The **core** is agent-agnostic: question and answer are journal events plus
  file channels in the run directory, modeled on the proven human_review
  machinery (`reviews.jsonl`, park and poll, decisions consumed by count).
- The **ask transport** is how the node agent's question physically reaches
  the engine, chosen per agent from a capability field: `live` (blocking MCP
  tool inside the running agent) or `resume` / `reprompt` (marker protocol
  plus re-invocation).
- The **answer path** is shared and unique: a question raises a wake; the
  supervisor, the CLI, and the web UI all post answers into the same channel.

The playbook author sees none of this. A node opts in with `interactive:
true`; the engine picks the best transport the bound agent supports.

## Schema changes (apb-core)

`agent_task` nodes gain:

- `interactive: bool` (default false). Only meaningful on `agent_task`.
- `answer_by: human | supervisor` (default `human`). `human`: the supervisor
  must relay the question to the user and relay the answer back verbatim.
  `supervisor`: the supervisor is allowed to answer on its own judgment.
- `question_timeout_seconds: Option<u64>` (default none = wait forever, like
  human_review) with `default_answer: Option<String>`. When the timeout
  elapses, the engine answers with `default_answer` (`answered_by:
  "timeout"`); a timeout without `default_answer` fails the node attempt.

Validation (V30 is the last code in use, so these take V31 and V32):

- V31: interactive fields (`answer_by`, `question_timeout_seconds`,
  `default_answer`) on a non-interactive or non-agent_task node.
- V32: `default_answer` without `question_timeout_seconds`.

All schema fields participate in the playbook content digest automatically,
so the trust gate covers interactivity: a playbook cannot gain the ability to
question the user without its digest changing and trust being re-established.

## Core machinery (apb-engine)

### Channels and events

Two append-only channels in `runs/<id>/`, mirroring `reviews.jsonl`:

- `questions.jsonl`: entries `{seq, node, attempt, question, options?}`.
  Written by the ask transports (the MCP sidecar or the adapter's marker
  parser). Options are an optional list of suggested answers; free-text
  answers are always allowed.
- `answers.jsonl`: entries `{seq, node, answer, answered_by}`. Written by the
  answer facades (`post_answer`, exposed like `post_review`).

Only `drive` writes journal events (same invariant as review):

- `QuestionAsked { node, question, options }` when drive observes a new
  question entry.
- `QuestionAnswered { node, answer, answered_by }` when drive matches the
  N-th answer for a node against the N-th asked question (count-based
  consumption, exactly like ReviewDecided).

Both new `EventPayload` variants follow the append-only rule; any new fields
later must carry `#[serde(default)]`.

### Parking and timeouts

A node with an unanswered question is parked: drive spins on the answer
channel with `AWAIT_CONTROL_POLL`, exactly like an undecided human_review.
The run status surfaces `waiting_kind: "question"` plus the question text and
the node id (`pending_question` in `run_status`). Interactive `agent_task`
nodes are always executed sequentially, never inside the concurrent batch, so
at most one question is pending per run at a time and `pending_question` is a
single object by design, not an array.

While a question is pending, the node's own `timeout_seconds` clock is
paused: the engine records the asked-at and answered-at instants from the
journal and excludes pending intervals from the elapsed time handed to
`check_cancel_timeout`. A node that budgets 300 s of agent work must not be
killed because a human took an hour to answer.

Stop, abort, and resume compose with parking through the existing paths: a
parked node behaves like a waiting human_review for `stop_run` and
`plan_resume`. On resume after a crash, an asked-but-unanswered question is
re-surfaced from the journal without re-invoking the agent; an answered
question replays deterministically.

### Transport selection

`InvocationDef` gains `interaction: live | resume | reprompt` (serde default:
`reprompt`). Built-in defaults:

| agent | interaction | rationale |
|---|---|---|
| claude / claude-code | `live` | `--mcp-config` injection plus about 28 h tool timeout, both confirmed in docs |
| codex | `resume` | `codex exec resume <id>` confirmed; blocking capped at 60 s by config-file-only setting |
| opencode | `resume` | tool calls hard-capped around 30-120 s; `--session <id>` documented (reliability bugs noted, fallback below) |
| hermes | `resume` | `--resume` documented; combination with `-z` requires empirical verification before enabling, until then behaves as `reprompt` |
| agy | `reprompt` | no conversation id is surfaced from `-p` (open upstream issue), resume impossible today |

Config-defined agents may override `interaction` in
`agents.<id>.invocation`. The engine treats the field as a ceiling, not a
promise: `live` falls back to `resume`, and `resume` falls back to
`reprompt`, whenever the preferred transport fails to initialize (missing
session id, sidecar spawn failure). Every fallback is journaled as a
`SupervisorAction { action: "interaction_downgraded" }` detail so the run
report shows what actually happened.

## Transport: live (claude first)

A hidden subcommand `apb __ask-server --run <id> --node <node> --attempt <n>`
serves a single-tool stdio MCP server from the apb binary itself (pattern
matches the existing hidden `apb __drive-run`). The tool:

- `ask_user(question: string, options?: string[]) -> string`. Appends to
  `questions.jsonl`, then polls `answers.jsonl` for the matching answer,
  sending an MCP progress notification every 60 s so Claude Code's 30-minute
  stdio idle timer never fires. Returns the answer text.

The adapter, when the node is interactive and the agent's `interaction` is
`live`, injects the sidecar into the spawn:

- claude: `--mcp-config '{"mcpServers":{"apb":{"command":"<current-exe>",
  "args":["__ask-server", ...], "timeout": <question_timeout or a very large
  value>}}}'`. The apb executable path is the same re-exec path
  `__drive-run` already resolves. No `--strict-mcp-config`: the agent keeps
  its own configured servers.
- The node prompt gains one appended paragraph: the tool exists, when to use
  it, and that free-form questions to the user should go through it rather
  than being answered by assumption.

The sidecar is a child of the agent process and dies with the agent's
process group; it holds no state beyond the run-dir paths and writes through
the same atomic fsutil primitives. It never reads secrets and never sees the
prompt. The engine side needs no special handling beyond the parking logic:
drive notices the question entry exactly as it notices a review request. The
adapter must not treat the blocked agent as hung: the pending-question
interval is excluded from the node timeout (core machinery above), and the
EOF/exit budgets from the run-reliability work remain in force unchanged
(the agent process is alive and its pipes are open while blocked).

## Transport: resume / reprompt (everyone else)

For interactive nodes on `resume`/`reprompt` agents, the engine appends a
marker contract to the node prompt:

> If you need input from the user before you can proceed, print a line
> containing exactly `<<<apb:question>>>` followed by a JSON object
> `{"question": "...", "options": ["...", ...]}` on the next line, then stop
> without doing further work.

The adapter scans agent stdout for the marker (same place the stream parser
already runs). On a marker:

1. The attempt finishes as `Suspended` (internal outcome, not a node
   status): no `NodeFinished` is written; the question enters
   `questions.jsonl`; the node parks.
2. For `resume` agents the adapter captures the session id from the finished
   run (claude: `session_id` from `--output-format json` / stream-json;
   codex: session JSONL id; opencode: session id; hermes: session name) and
   stores it in the attempt's journal entry (`AttemptFinished` gains
   `session: Option<String>`, `#[serde(default)]`).
3. On answer, drive re-invokes the node: `resume` agents get their session
   resumed with the answer as the follow-up prompt; `reprompt` agents (and
   any resume failure) get a fresh invocation whose prompt carries the full
   Q&A transcript of this node visit appended after the original prompt.
4. The loop may repeat: an agent may ask several questions across several
   suspensions. Each round is a new attempt row in the journal with the same
   spawn-time `AttemptStarted` discipline the reliability work introduced.

The marker is parsed only from interactive nodes; on non-interactive nodes
the literal text has no effect. A malformed JSON payload after the marker
fails the attempt with a clear error naming the node (never a silent parse
skip), because a half-parsed question would park the run on a question
nobody can read.

## Answer path (shared)

- **MCP** (apb-mcp): new tool `run_answer { run_id, node?, answer }`, the
  analog of `review_decide`, posting `{answered_by: "supervisor"}` when the
  caller authenticates with a supervisor token and `{answered_by: "human"}`
  for the plain run-tool path. `run_status` gains `pending_question: { node,
  question, options, answer_by, asked_at }` and the question raises a wake so
  `supervisor_wait_event` returns immediately.
- **Policy**: when the node declares `answer_by: human`, `post_answer`
  rejects supervisor-token answers with an error instructing the supervisor
  to relay the question to the user. `answer_by: supervisor` accepts both.
- **CLI** (apb-cli): `apb answer <run> <text>` (with `--node` when several
  questions could be pending across parallel branches). `apb runs` and `apb
  doctor --run` display waiting-on-question state.
- **Web UI** (apb-server + web/): the run view shows a question panel with
  the question text, options as buttons, and a free-text field, exactly the
  interaction pattern of the existing review panel. Server surface: pending
  question in the run status payload plus a POST answer endpoint delegating
  to `post_answer`.
- **Supervisor contract**: the supervisor SOUL/prompt guidance (docs/MCP.md
  supervised-run section) gains the standing instruction: relay `answer_by:
  human` questions to the user verbatim in the user's chat language, return
  the answer verbatim, never answer such questions yourself; `answer_by:
  supervisor` questions may be answered directly, escalate when unsure.

## Security and trust

- `interactive`, `answer_by`, and the timeout fields are schema fields,
  hence inside the playbook content digest and behind the existing trust
  gate. An untrusted or drifted playbook cannot interrogate the user.
- Question text is agent-generated and untrusted: every facade renders it as
  plain text (no markdown execution in the web UI, no shell interpolation in
  the CLI), and the supervisor instruction says to relay, not to obey,
  question content.
- Answers are user input embedded into prompts: they are journaled verbatim
  and rendered into the re-invocation prompt as a quoted Q&A block. No
  template expansion runs over answer text (V13 namespaces do not apply
  inside answers).
- The sidecar MCP server exposes exactly one tool and only writes to the two
  channel files of its own run; it validates `run_id`/`node` as safe
  segments like every other entry point.

## Testing strategy

Per docs/TESTING-GUIDELINES.md (bounded waits, one integration binary per
crate, stub agents):

- Core: unit tests for count-based question/answer consumption, parking,
  timeout-pause arithmetic, and the answer-rejection policy; integration
  tests driving a stub agent that emits the marker, answering via
  `post_answer`, asserting the journal sequence and the re-invocation
  prompt content (reprompt) and session flag (resume).
- Live transport: the sidecar's channel IO tested directly (spawn the
  subcommand, call the tool over stdio MCP, answer, assert return), plus an
  injection-shaped test for the progress-notification cadence. No test may
  wait unbounded on the sidecar; every wait carries a deadline naming the
  question id.
- Resume capture: per-agent session-id extraction tested against recorded
  fixture outputs, not live agents. Live verification for hermes and
  opencode happens in the phase 3 field checks and is recorded in
  docs/INTERACTIVE-AGENTS.md.
- MCP/CLI/web facades: one test per facade posting an answer end to end.

## Phasing

1. **Core plus resume/reprompt transport**: schema, validation, channels,
   events, parking, timeout pause, marker contract, re-invocation, MCP
   `run_answer` and `run_status`, CLI `apb answer`, supervisor wake and
   docs. Works with every agent from day one (reprompt as the floor).
2. **Live transport for claude**: `__ask-server` sidecar, `--mcp-config`
   injection, progress notifications, prompt paragraph, downgrade path.
3. **Field verification and matrix updates**: empirically verify hermes
   `-z` with `--resume` and opencode session reliability, flip their
   `interaction` defaults if verified, update docs/INTERACTIVE-AGENTS.md,
   and open the upstream contributions listed there.

## Out of scope

- MCP elicitation as a transport (not shipped or unverified headless in
  every surveyed agent; revisit when the ecosystem catches up).
- Live transport for codex, copilot, goose, amp until their timeout
  behavior is verified empirically (codex 60 s config-file default, copilot
  open timeout bugs, amp undocumented).
- Interactivity for `script` and `condition` nodes.
- Web UI chat-style threading of the Q&A history (the run event log already
  shows it; a dedicated thread view can come later).
- Answer routing to external channels (Telegram, Slack): the supervisor
  relay already covers "wherever the user actually is" for MCP-driven runs.
