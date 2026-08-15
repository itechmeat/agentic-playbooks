# Qoder CLI Agent and Models Table Refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the Qoder CLI (qoder.com/cli) as a ninth built-in coding agent, following the cursor aggregator pattern, and refresh the curated models table against 2026-08-15 vendor pricing.

**Architecture:** Qoder is an aggregator-category agent (one subscription serving Qwen, DeepSeek, GLM, Kimi and MiniMax models), so it gets a detection probe, an invocation form, a session-capture arm and docs — no `agent_vendor` mapping and no curated rows of its own. The models refresh is a pure data change to `assets/models.yaml` plus its pinned tests.

**Tech Stack:** Rust workspace (apb-core, apb-engine), YAML data asset, existing test suites.

## Global Constraints

- No em-dashes (U+2014), no exclamation marks in docs or user-facing strings; no CJK anywhere.
- Commits: `git commit --signoff` plus `Co-Authored-By: <acting model> <noreply@anthropic.com>` trailer.
- Never commit `.apb/profiles/developer/profile.yaml` (pre-existing local drift, not part of this work).
- Gates before done: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `cargo metadata --format-version 1 >/dev/null` then `code-ranker check .`.
- All facts below were verified 2026-08-15 against the real CLI help output (`@qoder-ai/qodercli` 1.1.22) and official vendor pricing pages; do not "improve" them from memory.

---

### Task 1: Qoder built-in agent

**Files:**
- Modify: `crates/apb-core/src/detect.rs` (builtin_probes entry, custom_probes shadow-guard set, doc comment count)
- Modify: `crates/apb-engine/src/invocation.rs` (builtin arm, resume-argv arm, inline tests)
- Modify: `crates/apb-engine/src/adapter.rs` (capture_session arm)
- Modify: `docs/PROFILES.md` (known-agents comment line, autonomous-flag prose)
- Test: `crates/apb-core/tests/suite/detect_test.rs`, `crates/apb-server/tests/suite/meta_api_test.rs`, `crates/apb-engine/tests/suite/resume_capture_test.rs`, inline tests in invocation.rs, `agent_vendor` inline test in `crates/apb-core/src/models_table.rs`, `web/src/lib/profileedit.test.ts`

**Interfaces:**
- Consumes: existing `Probe`, `mk`, `v`, `SoulDelivery`, `Interaction`, `capture_json_string_field` helpers exactly as grok/cursor use them.
- Produces: agent id `qoder` visible in `detect::detect()`, `invocation::builtin("qoder")`, `adapter::capture_session("qoder", ...)`.

**Verified CLI facts (source: real `--help` of @qoder-ai/qodercli 1.1.22):**
- Binary: `qoder` (npm also installs alias `qodercli`; id equals binary name, so no `program_for`/`default_program` arm is needed).
- Print mode: `-p, --print` is a BOOLEAN flag; the prompt is a positional `query` argument (cursor-style grammar, not grok-style).
- Model: `-m, --model <model>`.
- System prompt: `--system-prompt <text>` and `--append-system-prompt <text>` both exist; use `--append-system-prompt` so Qoder's own agentic system prompt survives (mirror how apb treats claude if claude uses the append flag; the SOUL is a role prompt, not a replacement).
- Non-interactive approval: `--permission-mode <mode>` with snake_case choices `default, accept_edits, bypass_permissions, dont_ask, auto`. Use `--permission-mode bypass_permissions` (NOT camelCase; that is claude/grok spelling).
- Output: `-o/--output-format` accepts `text`, `json`, `stream-json`; pin `--output-format text` like cursor.
- Resume: `-r, --resume [id]` exists; json/stream-json init event carries `session_id`. Interaction ceiling: `Resume`.

- [ ] **Step 1: detect.rs probe.** Add after the cursor probe, following the same comment style:

```rust
// Qoder CLI. An aggregator: one subscription serving Qwen, DeepSeek,
// GLM, Kimi and MiniMax models, so it contributes no curated rows of
// its own. npm installs both `qoder` and a `qodercli` alias for the
// same binary; the shorter `qoder` is unambiguous and is the one
// probed. Model enumeration via `--list-models` requires
// authentication, so it is deferred like grok's.
Probe {
    id: "qoder".into(),
    bins: v("qoder"),
    category: AgentCategory::Aggregator,
    version_args: v("--version"),
    models_source: ModelsSource::None,
    auth_source: AuthSource::None,
},
```

Add `"qoder"` to the `custom_probes` shadow-guard BTreeSet and update the "eight agents" doc comment to nine.

- [ ] **Step 2: invocation.rs builtin arm.** Cursor-style boolean `-p` with the prompt positional and last:

```rust
// qoder's `-p/--print` is a BOOLEAN flag and the prompt is a
// positional argument, so the prompt slot goes last, after the
// options, like cursor. Unlike cursor it has a real system-prompt
// flag; `--append-system-prompt` is used (not `--system-prompt`) so
// the SOUL rides alongside qoder's own agentic system prompt instead
// of replacing it. `--permission-mode bypass_permissions` (snake_case,
// unlike claude's camelCase) is the non-interactive approval mode and
// `--output-format text` pins plain stdout.
"qoder" => Some(mk(
    &["-p", "--output-format", "text", "--model", "{model}", "{prompt}"],
    SoulDelivery::Native,
    Some("--append-system-prompt"),
    &["--permission-mode", "bypass_permissions"],
    Interaction::Resume,
)),
```

Adjust to the exact `mk` signature in the file (the argv/pinned-args split above follows the cursor arm's split; keep whichever split the cursor arm actually uses so autonomous args stay in `autonomous_args`). Resume-argv arm:

```rust
"qoder" => Some(v(&[
    "--resume", "{session}", "-p", "--output-format", "text", "--model", "{model}", "{prompt}",
])),
```

No `program_for` change (id == binary).

- [ ] **Step 3: adapter.rs capture_session arm.**

```rust
"qoder" => capture_json_string_field(raw, &["session_id"]),
```

No `default_program` change (id == binary).

- [ ] **Step 4: tests.** Extend, following the existing per-agent shapes exactly:
  - `detect_test.rs`: probe assertions for qoder (bins `qoder`, category Aggregator, version_args `--version`), alongside `builtin_probes_include_grok_and_cursor`.
  - `meta_api_test.rs`: bump `agents.len() == 8` to 9, update the "eight built-in probes" comment, add `agents.iter().any(|a| a["agent"] == "qoder")`.
  - invocation.rs inline: add `"qoder"` to the `builtin_agents_present_and_valid` loop list; add `builtin_qoder_form` pinning the exact argv, soul delivery, soul flag, autonomous args, interaction and resume argv.
  - adapter.rs inline: extend the binary-name test only if it loops over all ids.
  - `resume_capture_test.rs`: qoder block asserting `capture_session("qoder", <json with session_id>)` extracts it and plain text yields `None`.
  - models_table.rs inline `agent_vendor_ties_known_vendor_agents_only`: assert `agent_vendor("qoder") == None`.
  - `web/src/lib/profileedit.test.ts`: add qoder to the fixture asserting an aggregator keeps the full table (mirror the cursor case). Run `cd web && bun run test`.

- [ ] **Step 5: docs/PROFILES.md.** Add `qoder` to the known-agents comment list and extend the autonomous-flag prose: `--permission-mode bypass_permissions` for qoder. Also grep `docs/INTERACTIVE-AGENTS.md` and `README.md` for agent enumerations and extend if any list the eight ids.

- [ ] **Step 6: gates and commit.** fmt, clippy, `cargo test --workspace`, `bun run test` in web/, code-ranker; commit `feat(core,engine): qoder CLI as a built-in agent`.

### Task 2: Models table refresh (2026-08-15)

**Files:**
- Modify: `assets/models.yaml`
- Test: `crates/apb-core/tests/suite/models_table_test.rs` (and any inline models_table tests pinning row ids)

**Interfaces:**
- Consumes: existing row schema (id, vendor, cost_in_usd_mtok, cost_out_usd_mtok, reasoning, context_tokens, vision/stt/tts, source_url, checked_at, price_basis).
- Produces: refreshed table, `as_of: "2026-08-15"`.

All values below were verified against official vendor pages on 2026-08-15; `checked_at: "2026-08-15"` on every touched row.

- [ ] **Step 1: updates to existing rows.**
  - `claude-sonnet-5`: `price_basis` from `launch-until-2026-08-31` to `list` (the vendor made the launch price permanent; the scheduled Sep 1 increase was cancelled).
  - `gpt-5.6-terra`: costs 2.5/15.0 to **2.0 / 12.0**.
  - `gpt-5.6-luna`: costs 1.0/6.0 to **0.2 / 1.2**.
  - `o4-mini`: costs 4.0/16.0 to **1.10 / 4.40** (the stored value was wrong from the start; the vendor page lists 1.10/4.40).
  - DeepSeek rows: prices unchanged, but add a section comment: DeepSeek switches to peak/off-peak billing on 2026-08-16 (off-peak flash 0.22/0.66, pro 0.66/1.98; peak doubles); the flat rows below need a follow-up refresh once the scheme is live.

- [ ] **Step 2: new rows.**
  - `claude-opus-5` (anthropic): 5.0 / 25.0, reasoning high, context_tokens 1000000, vision true, source_url `https://platform.claude.com/docs/en/about-claude/pricing`, price_basis list.
  - `gemini-3.7-flash` (google): 0.75 / 3.75, reasoning high, vision true, stt true, source_url `https://ai.google.dev/gemini-api/docs/pricing`, price_basis `launch-until-2026-12-31` (rises to 1.5/7.5 after; note in a comment).
  - `gemini-3.5-flash-lite` (google): 0.3 / 2.5, reasoning medium, vision true, stt true, same source, price_basis list.
  - `grok-4.6` (xai): 2.0 / 6.0, reasoning high, context_tokens 500000, source_url `https://docs.x.ai/docs/models`, price_basis list (base tier; the existing two-tier comment covers the doubling; vision unverified, omit the flag).
  - `qwen3.8-max` (alibaba): 1.65 / 4.951, reasoning high, context_tokens 1000000, vision true, source_url `https://www.alibabacloud.com/help/en/model-studio/qwen3-8-max`, price_basis list; comment: Singapore region is 2.0/6.0.
  - `kimi-k3` (moonshot, new vendor string): 3.0 / 15.0, reasoning high, context_tokens 1048576, source_url `https://platform.kimi.ai/docs/pricing/chat-k3`, price_basis list.
  - `glm-5.2` (zhipu, new vendor string): 1.4 / 4.4, reasoning high, source_url `https://docs.z.ai/guides/overview/pricing`, price_basis list.
  - Group moonshot/zhipu/new-alibaba rows under a comment noting these are the model families served by the qoder agent.
  - Deliberately NOT added (record in the yaml comment only if a natural place exists, otherwise skip): Claude Mythos 5 (gated availability), gpt-5.6-cyber/5.5-cyber (specialized red-team line), qwen3.7-max/plus, kimi-k2.7-code, glm-5.3, Cantus (no official metered pricing published as of 2026-08-15).

- [ ] **Step 3: drops.**
  - Drop the `grok-4` row (no longer listed on docs.x.ai at all; superseded by 4.3/4.5/4.6).
  - Drop the `llama-4-maverick` row and the Meta section (Meta wound down its first-party Llama API on 2026-07-06; the source_url is dead and no vendor pricing exists).
  - In `purposes`, replace the `grok-4` reference in `brainstorming` scores with `grok-4.6` (same score). Verify no other purpose references a dropped id.

- [ ] **Step 4: metadata.** Bump top-level `as_of` to `"2026-08-15"`. Add `claude-opus-5` to `claude_static_models`.

- [ ] **Step 5: tests.** Extend `models_table_test.rs` (and inline tests) so the new vendor rows are pinned the way xAI rows are: assert `kimi-k3`, `glm-5.2`, `qwen3.8-max`, `claude-opus-5`, `grok-4.6` parse with their vendors, and remove/adjust any assertion referencing `grok-4` or `llama-4-maverick`. Confirm nothing else in the workspace references the dropped ids (`git grep -n "grok-4\"" ; git grep -n "llama-4-maverick"`).

- [ ] **Step 6: gates and commit.** fmt, clippy, `cargo test --workspace`, code-ranker; commit `chore(core): refresh curated models table for 2026-08-15`.

---

## Release (coordinator-owned, after both tasks review clean)

Final whole-branch review, then: `docs/release-notes/v0.17.0.md`, version bump PR flow, tag `v0.17.0`, pipeline watch, local `apb self-update`.
