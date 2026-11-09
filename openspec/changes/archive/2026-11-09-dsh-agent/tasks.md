# Tasks: DeepSeek Harness (`dsh`) as a built-in agent

Tick each box as soon as it is done. Tests first for every behaviour.

## Environment

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-dsh-target TMPDIR=$HOME/.cache/apb-dsh-tmp
```

## 1. Agent module

- [x] 1.1 `agents/dsh/model.rs`: model string parse, canonical spelling, the patch body, unit tests.
- [x] 1.2 `agents/dsh/output.rs`: the `--json` reader, unit tests on trimmed captures.
- [x] 1.3 `agents/dsh/actions.rs`: tool calls as actions, unit tests.
- [x] 1.4 `agents/dsh/mod.rs`: descriptor, forms, setup, model policy; registry entry.
- [x] 1.5 `agents/dsh/mcp.rs`: `apb setup` registration in the home patch layer, round-trip tests.

## 2. Spawn path

- [x] 2.1 `HostSetup::scoped_args` and its application in `adapter::command::spawn_agent`, with an engine test.
- [x] 2.2 Decision record 0013.

## 3. Transcript

- [x] 3.1 `Transcript::DshEvents` and the engine's parser over the shared JSON-lines view.

## 4. Tests across apb

- [x] 4.1 `agents_test` golden tables, detect tests.
- [x] 4.2 Agent contract fixture and suite entry (stdin prompt support in the stub).
- [x] 4.3 Doctor check test for dsh's help.

## 5. Docs

- [x] 5.1 `docs/AGENT-SETUP.md` section and agent lists.
- [x] 5.2 CLAUDE.md and AGENTS.md module map, identical text.
- [x] 5.3 Dashboard hint naming the session-continuing agents.

## 6. Gates

- [x] 6.1 fmt, clippy, tests, doc tests, ts_contract, web checks, code-ranker, release clippy.
- [x] 6.2 Live checks with the installed dsh (owner approved): format, resume, MCP attachment, one tiny playbook end to end.

## 7. Review findings

- [x] 7.1 Content-addressed, never rewritten model patches; overlapping launches on different routes keep their own.
- [x] 7.2 The supervisor spawn applies `scoped_args`; ADR 0013 updated; supervisor launch test.
- [x] 7.3 A failed attempt's printed session id goes into retry state; retry-after-failure test.
- [x] 7.4 `model_env` covers every provider of dsh's verified `llm-pi-ai` catalog; allowlist plan test on `cerebras/`.
- [x] 7.5 `apply_patch` paths from its add, update, move and delete headers, shared with codex's decoder.
- [x] 7.6 Failed-attempt session recovery only from a declared, verified opening record (`Capabilities::printed_session`), validated, never over a continued or assigned id; negative tests.
- [x] 7.7 `amazon-bedrock` keeps `AWS_SESSION_TOKEN`; the full temporary-credential set tested.
