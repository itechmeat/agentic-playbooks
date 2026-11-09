# Design: DeepSeek Harness (`dsh`) as a built-in agent

Facts below were read in the dsh source at 0.2.2-rc.1 (commit `fcc0ad46`,
paths relative to that tree) and confirmed with a few live runs of the
installed `dsh` 0.2.2-rc.1 on 2026-11-09.

## 1. How dsh is launched

- `dsh <name>` abbreviates `dsh --profile <name>`, but only when the first
  argument does not start with `-` (`apps/cli/src/args.ts`,
  `parseDshArgs`). The launcher parses its own flags (`--profile`,
  `--patch`, the dumps) up to the first token it does not know and hands the
  rest to the booted app verbatim (`passThroughOptions`). apb therefore
  always spells `--profile headless`, so a leading `--patch` keeps working.
- The headless app (`packages/bundle/headless/src/startup.ts`) takes the
  task as positional words, `-` for stdin, `--json`, `--session-id <id>`,
  `--permission-mode <preset>`, `--approval-policy ask|never`, `--add-dir`.
  Commander keeps the last value of a repeated option, so the autonomy flags
  appended after the base form win.
- Permission presets (`packages/bundle/base/cordis.patch.yml`, row
  `permission-presets`): `read-only` (sandbox read-only, approval ask),
  `workspace-write` (ask), `danger-full-access` (no confinement, approval
  never). With `ask` and no answerer an approval fails closed
  (`packages/interaction/user-approval/src/index.ts`), `never` rejects every
  ask. Base form: `--permission-mode read-only --approval-policy never`;
  autonomy: `--permission-mode danger-full-access`, the one-shot equivalent
  of codex's `--dangerously-bypass-approvals-and-sandbox`.
- The prompt goes on stdin (`-`): no argument length limit and never read as
  an option. A resume form always carries its prompt as argv in apb
  (`apb_engine::invocation::resume_spec`), so the resume form ends with
  `-- {prompt}`; the headless parser honours `--` (`jsonRequested`).
- There is no system-prompt flag: the SOUL is a prompt prefix.

## 2. How the model is chosen

dsh has no model flag. The model is the `agent-default-model` row
(`packages/core/agent-default-model/src/index.ts`: `provider`, `model`,
`reasoningEffort`) of the composed profile; `--patch <file>` overlays are
applied last (`apps/cli/src/profile-boot.ts`, `composeProfile`), and a patch
replaces the targeted row's whole `config`
(`packages/bundle/base/cordis.patch.yml`, header).

Decision: apb writes a patch file with exactly that row and passes `--patch
<file>` in front of the form. The env-only `scoped_env` hook cannot do it
(dsh reads no variable for an overlay path), so `HostSetup` gains
`scoped_args(model, scoped_dir)`, applied by the one spawn path
(`adapter::command::spawn_agent`) for every form and transport, and by the
supervisor spawn, and recorded as decision record 0013. The file lives in
the node's agent home, named after its content
(`agent-home/dsh/<node>/model-<digest>.patch.yml`), so items of one list
node that run at once on different fallback models never overwrite each
other's patch; outside a run it is the same content-addressed file under
apb's state directory, as for zcode's provider config. An empty model writes nothing and the profile's own default runs.

Model strings: `[<route>/]<model>[@<effort>]`. A bare model is on the route
`deepseek-official` (`packages/llm/llm-deepseek-api-key/src/index.ts`,
`PROVIDER`), whose key is `DEEPSEEK_API_KEY`. Other routes are the
`llm-pi-ai` provider profiles the person configured (`providers.<route>`),
whose key names come from `packages/llm/llm-pi-ai/src/provider-data.generated.ts`.
Every part is checked against a small character set before it is written,
and values are written JSON-quoted, so no model string can add a YAML tag or
another row.

The official API serves exactly `deepseek-flash` and `deepseek-v4-pro`
(`packages/llm/llm-deepseek/src/models.ts`; a live run with another name
failed with "The supported API model names are deepseek-flash,
deepseek-v4-pro"). That pair is apb's closed list for dsh, DeepSeek first;
an id on another route passes the list check, since apb cannot list a
person's routes. Efforts on the DeepSeek route: `off`, `low`, `high`, `max`
(`packages/llm/llm-deepseek/src/config.ts`). A billing account is one route
(`dsh:<route>`), so a quota on DeepSeek does not block a fallback to another
route on dsh.

## 3. How the output reads

`--json` (`packages/bundle/headless/src/json-stream.ts`) writes one JSON
object per line: `session {sessionId, cwd}` first, `status` with `phase`
`turn_start`, `step_start`, `step_end` (with `usage` when every attempt of
the step reported one) and `turn_end` (with `reason`), `thinking`, `text`,
`tool_call {callId, tool, input}`, `tool_result {callId, status, result,
error?}`, and last `final {sessionId, reason, text}` (the runner,
`packages/bundle/headless/src/index.ts`), or `error {message}` for a driver
failure. The exit code is 0 only for reason `completed`. Older builds wrote
`final {text}` alone (the recorded streams); the reader takes both.

Usage counts are disjoint (`packages/llm/llm/src/types.ts`, `TokenUsage`):
`inputTokens` is uncached input, cache reads and writes are separate. The
reader sums every `step_end`; no cost is printed.

Failure words: the reason's `error.code` is a stable class
(`packages/llm/llm/src/error.ts`, `retry-policy.ts`). The reader appends the
words apb's classifier already reads: `RATE_LIMIT` "rate limit", `QUOTA` and
`ACCOUNT_QUOTA` "quota exceeded", `AUTH`, `MISSING_CREDENTIAL`,
`INVALID_CREDENTIAL` "authentication failed", `SERVER` "server error",
`TIMEOUT` "request timed out", `TRANSPORT` "connection error", plus the
HTTP `status` when present. A resume refused because the session does not
exist or was recorded in another directory reads "session not found", so
the engine starts fresh instead of retrying.

## 4. Sessions

Sessions persist under `$DSH_HOME/sessions` and `--session-id` adopts one;
an unknown id is an error and so is a session recorded in another working
directory (`resolveAgent`, `assertAdoptable`). dsh cannot be handed an id
for a new session, so the fresh form is `Printed`: the `session` event is
the first line, written before any model call, so even a killed attempt
names its session.

That record is a declared capability (`Capabilities::printed_session`). For
such an agent alone, a failed or timed-out attempt's session is read from
the head of its stdout log by the agent's own record parser
(`OutputFormat::opening_session`, the first line only), validated like
every captured id (no dash-led or malformed id, codex's stderr header
included), and used for the retry only when apb handed the attempt no
session of its own (a continued or an assigned one). A plain-text reader's
answer that holds a session field is never taken.

## 5. Host facts

- Login: `$DSH_HOME/.credentials.yaml` (presence only) or
  `DEEPSEEK_API_KEY` in the environment.
- Environment set under `process_env: allowlist`: `DSH_*` and
  `DEEPSEEK_*`; a model on another route named after a provider of dsh's
  `llm-pi-ai` catalog adds that provider's variables, and `amazon-bedrock`
  also `AWS_SESSION_TOKEN` for temporary credentials.
- Cost basis: an api key route bills; apb reads no subscription fact, so
  it says nothing beyond what the person declared.
- Sandbox: not verified, so `isolation: full` refuses dsh and
  `best_effort` runs it without a sandbox.
- Skills: dsh reads `.agents/skills` of the project and `~/.agents/skills`
  (`packages/skill/skill-filesystem/src/index.ts`, `roots`), apb's own skill
  canon, so skills are delivered by name.
- MCP: an MCP server is a `@deepseek-ai/dsh-mcp-client` row
  (`docs/user/guide/mcp-memory.md`). `apb setup` appends one `insert` patch
  to `$DSH_HOME/cordis.patch.yml`, the home layer every profile reads, and
  removes exactly those bytes.
- Not supported, with the reason in the module doc and
  `docs/AGENT-SETUP.md`: the live ask transport (claude's `--mcp-config`
  injection; dsh takes MCP servers only as rows), the tool exposure check
  (the stream reports no tool list at start), an end-of-turn hook (dsh's
  hook rows are experimental), the hermetic minimal environment, desktop
  history.
