# Hermes Agent integration

Date: 2026-07-19
Status: approved design, pending implementation plan
Depends on: 2026-07-12-agent-profiles-design.md

## 1. Purpose and scope

apb executes `agent_task` nodes through profiles bound to a known set of
executor agents (claude, codex, agy, opencode, pi). This story adds Hermes
Agent (Nous Research, `hermes` CLI) as the sixth known agent, on exactly the
same rails: a detection probe, a built-in invocation form, tests, and docs.
No new adapter code is required; the engine's generic headless adapter
already serves any CLI that takes a prompt in argv and prints the final
answer to stdout.

Facts established against the live installation on this machine (Hermes
Agent v0.18.2, install method git, `~/.hermes/hermes-agent`) and its
source:

- `hermes -z PROMPT` is a first-class one-shot mode designed for scripts
  and pipes: it prints ONLY the final response text to stdout (no banner,
  no spinner, no session id line), loads tools, rules, memory, and
  `AGENTS.md` from the CWD as in a normal turn, and auto-bypasses
  approvals (the implementation sets `HERMES_YOLO_MODE=1` internally).
  Verified live: `hermes -z 'Reply with exactly the text APB_OK...'`
  printed `APB_OK` and exited 0.
- Model selection mirrors `hermes chat`: `-m MODEL` optional (defaults to
  the user's configured model), provider auto-detected from the model id
  when `-m` is given alone; `--provider` without `-m` is an error.
- `--safe-mode` is a troubleshooting flag (disables user config, rules,
  plugins, MCP servers), not a permission guard; irrelevant here.
- `hermes acp` implements the Zed/JetBrains Agent Client Protocol. apb's
  `acp` transport is a different thing (Claude Code stream-json), so the
  ACP route is out of scope (section 6).

Out of scope: Hermes-specific model inventory parsing, a Hermes entry in
the curated models table, session resume through `--resume`, the Hermes
gateway/portal surface, and any web UI change (the dashboard's agent list
is fed dynamically by detection and picks the new agent up on its own).

## 2. Decisions and their rationale

| Decision | Choice | Why |
|---|---|---|
| Integration form | A `Probe` in `detect.rs` plus a `builtin()` arm in `invocation.rs`, like codex and opencode | The generic headless adapter is parameterized by the invocation form; hermes' `-z` contract (final text on stdout, exit code semantics) is exactly what the adapter expects, so new adapter code would be duplication |
| Invocation | `hermes -z {prompt} -m {model}` with `SoulDelivery::Prefix`, no soul flag, empty `autonomous_args` | `-z` is the documented script mode. Hermes has no per-invocation system-prompt flag, so the SOUL is prefixed into the prompt like codex and opencode. `-z` auto-bypasses approvals by hermes' own design, which matches the empty-autonomous-args posture of the other aggregator arms (codex `exec`, opencode `run`) |
| Category | `AgentCategory::Aggregator` | Hermes routes to many providers (OpenRouter, Z.AI, NVIDIA, Nous Portal, vendor keys); it is not a single-vendor CLI |
| Models source | `ModelsSource::None` | The CLI has no machine-readable model list (`hermes model` is an interactive picker). Suggestions fall back to the curated table, whose cross-vendor ids hermes can actually serve through its providers; hermes also auto-pairs the provider from the model id |
| Model placeholder | `-m {model}` always passed | apb profiles always carry a model; passing it explicitly keeps the run reproducible instead of silently following the user's hermes default |
| Auth hint | New `AuthSource::Hermes`: presence of `~/.hermes/.env` maps to `AuthKind::ApiKey` | Matches the existing pattern (presence only, values never read). The `.env` file is where hermes keeps provider keys |
| Version probe | `--version`, first line | `hermes --version` prints `Hermes Agent v0.18.2 (2026.7.7.2) · upstream e361c5e2` fast enough for the detect path; store the first line like the other probes |
| Curated models table | Unchanged | Table rows require verifiable pricing provenance; hermes serves models that are already rows (cross-vendor). Nothing to add without inventing prices |
| Program resolution | Default binary name equals the agent id (`hermes`) | `program_for`'s existing fallback already does this; no change needed |

## 3. Detection

`builtin_probes()` gains:

```rust
Probe {
    id: "hermes".into(),
    bins: v("hermes"),
    category: AgentCategory::Aggregator,
    version_args: v("--version"),
    models_source: ModelsSource::None,
    auth_source: AuthSource::Hermes,
},
```

`AuthSource::Hermes` checks `home/.hermes/.env` exists and reports
`AuthKind::ApiKey`; absent file means no auth hint. The builtin agent-id
set used for config merging (currently
`["claude", "codex", "agy", "opencode", "pi"]`) gains `"hermes"`. Doc
comments that say "the known five" / "the five agents" are updated to six
wherever they appear.

## 4. Invocation

`builtin()` in `apb-engine/src/invocation.rs` gains:

```rust
"hermes" => Some(mk(
    &["-z", "{prompt}", "-m", "{model}"],
    SoulDelivery::Prefix,
    None,
    &[],
)),
```

Transport stays `Headless`. Everything downstream (spawn, timeout,
cancellation, ETXTBSY retry, output capture) is the existing generic
adapter. A node bound to a hermes profile therefore runs
`hermes -z "<soul + prompt>" -m <model>` in the node workdir and the
node output is hermes' final response text.

Known behavioral note, documented rather than fought: `-z` loads
`AGENTS.md`, rules, memory, and the user's configured toolsets from the
CWD and user config, and auto-approves tool use inside that run. This is
the same trust posture as codex `exec` and opencode `run`; users who need
a stripped hermes for apb runs can bind a profile to a wrapper or use
hermes' own config to constrain toolsets.

## 5. Testing

- Unit, `apb-core` (`detect_test.rs` pattern): a stub `hermes` executable
  on a temp PATH that prints the real version line; assert the probe
  reports installed, version first line, Aggregator category, no models
  inventory; auth hint present when a fake `~/.hermes/.env` exists under
  the test HOME and absent otherwise.
- Unit, `apb-engine`: `builtin("hermes")` returns the exact form above
  (argv, prefix soul, headless transport, empty autonomous args);
  `spec_for`/`program_for` resolve `hermes` without config entries.
- Integration, `apb-engine` (`adapter_test.rs` pattern): a stub `hermes`
  script (written with the synced helper) that echoes its arguments,
  proving the adapter passes `-z <prompt with SOUL prefix> -m <model>`
  and captures stdout as the node output.
- Live smoke, `#[ignore]` env-gated `APB_LIVE_TEST_HERMES=1` alongside
  the existing live tests: runs the real local hermes through a minimal
  profile-bound node (`-z`, configured default model via an explicit
  `-m`), asserts exit 0 and non-empty output. Never runs in CI.

## 6. Out of scope, follow-up candidates

- `hermes acp` as a true ACP transport for apb (apb's `acp` transport is
  Claude Code stream-json today; adopting real ACP is its own story).
- Hermes model inventory (`ModelsSource`) if a machine-readable list
  command appears.
- Session resume (`--resume`/`--continue`) mapping onto apb run resume.
- `--usage-file` cost capture into run events.
- Curated table rows for Nous-hosted Hermes models once verifiable
  pricing is available.

## 7. Docs and release

- `docs/PROFILES.md`, `docs/MCP.md`, `docs/HOST-INTEGRATION.md`: add
  hermes wherever the known agents are enumerated.
- Workspace version bumps to 0.6.0 with `docs/release-notes/v0.6.0.md`
  (title: `apb 0.6.0: hermes agent support`).
