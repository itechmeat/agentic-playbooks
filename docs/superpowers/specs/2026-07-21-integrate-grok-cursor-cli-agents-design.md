# Integrate Grok CLI and Cursor CLI as built-in agents

Status: design (brainstorm output for issue #34)
Date: 2026-07-21
Related: docs/superpowers/specs/2026-07-12-agent-profiles-design.md (agent profiles, sections 6.2-6.3, 7.1, 8.2-8.4)

## Problem

apb drives a fixed set of six built-in CLI agents: claude, codex, agy, opencode, pi, and hermes.
Each is wired through three coordinated places: a detection probe (apb-core/src/detect.rs), an invocation form (apb-engine/src/invocation.rs), and, where relevant, curated pricing rows (assets/models.yaml).
Issue #34 asks to add two more modern coding CLIs so users can bind them in profiles and run them in playbooks exactly like the existing six:

- Grok CLI (xAI), referenced in the issue by the binary name `agent`, docs https://docs.x.ai/build/overview
- Cursor CLI, referenced in the issue by the binary name `cursor-agent`, docs https://cursor.com/docs/cli/overview

apb already supports custom agents through the global config `agents:` block, but a config recipe does not give first-class detection, curated models, or a first-class slot in the profile UI.
The goal here is full built-in parity, not a config workaround.

## Goals

- Detect both agents as installed CLIs, with canonical ids `grok` and `cursor`, like the current built-ins.
- Provide a built-in invocation form for each so a profile-bound node runs headless with the correct argv.
- Make both selectable in the profile UI when installed, with sensible model suggestions.
- Add curated, sourced pricing rows for xAI (Grok) models to the curated models table.
- Cover the new wiring with unit tests and keep all format, clippy, and code-ranker gates clean.

## Non-goals

- No live blocking `ask_user` (MCP sidecar) transport for either agent in this iteration.
- No new machine-readable model enumeration (no `models` probe command) for either agent in this iteration.
- No auth-hint detection for either agent in this iteration (they start at auth None, like agy and pi).
- No changes to the six existing agents beyond widening the "known" enumerations to eight.
- No Cursor-specific curated model rows (Cursor is an aggregator that runs existing vendor models).

## Canonical ids and binaries

Per the decision on issue #34, the binary names are taken verbatim from the issue text:

- Canonical id `grok`, binary `agent`, category Vendor (xAI models only).
- Canonical id `cursor`, binary `cursor-agent`, category Aggregator (runs gpt, claude, gemini, and other vendor models).

The id and the binary are deliberately distinct fields. The canonical id is what a profile references and what `invocation::builtin` matches on; the binary is only what detection looks for in PATH and what the launcher executes.
This split is already how `Probe { id, bins, ... }` is modeled, so no schema change is required to have id `grok` resolve to binary `agent`.

## Chosen approach and rationale

Bake both agents in as built-ins, mirroring the existing six across detect.rs, invocation.rs, and models.yaml.

Rationale: the user asked for full parity, and parity means a user picks `grok` or `cursor` in a profile and runs a playbook with no extra configuration, the same as `claude` or `codex`.
A built-in also centralizes the invocation form and the collision-sensitive binary probing in one reviewed place rather than in every user's config.

### Interaction ceilings

apb ranks interactivity Headless < Reprompt < Resume < Live.
The first integration is conservative and matches documented capabilities:

- `cursor` targets `Interaction::Resume`. The Cursor CLI documents resumable sessions (`--resume`, `--continue`, `ls`), which maps to the same resume transport used by codex, opencode, and hermes.
- `grok` targets `Interaction::Reprompt`. Grok Build documents headless `-p` and MCP/ACP, but its session-resume story is not clearly documented yet, so it starts at reprompt (like agy) and can be upgraded to Resume or Live in a follow-up once verified.

### Models strategy

- `grok` is a Vendor agent. Detection uses `models_source = None` (no live enumeration); model suggestions come from curated xAI rows added to assets/models.yaml.
- `cursor` is an Aggregator that runs other vendors' models. Detection uses `models_source = None`; it reuses the existing gpt, claude, and gemini rows and otherwise accepts free-text model entry. No Cursor-specific rows are added.

### Auth strategy

Both start at `auth_source = None` for this iteration, which is within parity (agy and pi are also None).
Best-effort auth hints (for example Grok reading `XAI_API_KEY`, Cursor reading its login state) are deferred to a follow-up and would only add a display hint, never read or store any secret value.

## Components

### 1. apb-core/src/detect.rs

Add two entries to `builtin_probes()`:

- `grok`: `bins = ["agent"]`, `category = Vendor`, `version_args = ["--version"]`, `models_source = None`, `auth_source = None`.
- `cursor`: `bins = ["cursor-agent"]`, `category = Aggregator`, `version_args = ["--version"]`, `models_source = None`, `auth_source = None`.

Widen the hardcoded built-in id set `["claude","codex","agy","opencode","pi","hermes"]` (used in `custom_probes()` and `detect`) to include `grok` and `cursor`, so a user config cannot shadow or duplicate them.
Update the "six agents" doc comments to reflect eight.
No new `ModelsSource` or `AuthSource` variant is needed, since both use the existing `None` variants.

### 2. apb-engine/src/invocation.rs

Add two arms to `builtin(agent_id)`:

- `cursor`: argv `["-p","{prompt}","--model","{model}"]`, `PromptVia::Argv`, `SoulDelivery::Prefix`, no soul flag, `autonomous_args` best-effort `["--output-format","text","--force"]`, `Interaction::Resume`.
  Provide the resume-round argv (the declarative resume form used by other Resume agents) that substitutes the prior session id and the follow-up prompt, based on `cursor-agent --resume`.
- `grok`: argv `["-p","{prompt}","-m","{model}"]`, `PromptVia::Argv`, `SoulDelivery::Prefix`, no soul flag, `autonomous_args = []`, `Interaction::Reprompt`.

Neither CLI documents a system-prompt flag, so the SOUL travels as a prompt prefix (`SoulDelivery::Prefix`), like the aggregators.
The autonomous and resume flags are best-effort and are called out as verify-before-merge items below.
Update the "known six" comment on `builtin`.

### 3. assets/models.yaml

Add curated xAI rows for Grok models (for example `grok-4.5`, `grok-4`, `grok-code-fast`) with `vendor: xai`, USD-per-million in and out costs, reasoning tier, context window, `source_url`, `checked_at`, and `price_basis`.
Bump the top-level `as_of`.
The table is advisory only; nothing here binds to detection or launch.
Add no Cursor rows.

### 4. Web UI (web/src/pages/ProfileEdit.svelte and web/src/lib/profileedit.ts)

Confirm both agents appear in `agentOptions` when `installed` is true (this is data-driven off `/api/agents`, so likely needs no change).
Confirm model suggestions: `grok` should surface the curated xAI rows through `modelIdsForAgent`; `cursor` should fall back to existing provider rows or free-text.
Add or adjust the agent-to-model mapping in profileedit.ts only if `grok` does not already resolve its vendor rows.

### 5. Tests

- Unit: `builtin_probes()` contains `grok` and `cursor` with the exact bins, category, and source fields above.
- Unit: `invocation::builtin("grok")` and `invocation::builtin("cursor")` return `Some(..)` and each `validate()` passes; assert the argv, soul delivery, and interaction ceiling.
- Unit: assets/models.yaml parses and contains the new xAI rows with valid `price_basis` and required provenance fields.
- Frontend (if a mapping change is made): `modelIdsForAgent("grok", ...)` returns the curated xAI ids.

## Considered alternatives

- Documentation-only recipe using the existing custom `agents:` config block. Zero Rust change, but no curated models, no first-class UI slot, and every user re-derives the invocation form. Rejected because the ask is parity.
- Hybrid: bake in detection but leave the invocation form to user config. Half-integrated and inconsistent with the six; rejected.
- Target Live (MCP ask-server sidecar) for both immediately, since both document MCP. Highest effort and risk, and unnecessary for a first integration; deferred.
- Add a machine-readable models probe (a `models` subcommand) for one or both. Neither CLI exposes a reliable one yet; deferred in favor of curated rows plus free-text.

## Risks and verify-before-merge items

- Binary names conflict with current upstream docs. Current xAI material indicates the Grok binary is `grok`, and the generic `agent` binary appears to belong to Cursor, which means the issue's parentheticals may be swapped or version-stale. This design honors the issue text (`grok` -> `agent`, `cursor` -> `cursor-agent`) by decision, and the implementer must re-verify the actual installed binary names before merge and correct the probes if reality differs.
- The generic binary name `agent` is PATH-collision-prone. Detection already ignores project-local binaries, but a globally installed unrelated `agent` could match. Consider a version-string sanity check during implementation if the risk proves real.
- Best-effort flags. Cursor's `--force` and `--output-format text` autonomous flags, Cursor's resume-round argv, and Grok's headless flag set must be validated against the installed CLIs; adjust the invocation forms to match.
- Model naming and pricing for xAI can change. The curated rows carry `source_url` and `checked_at`; verify against xAI's published pricing at implementation time.

## Acceptance criteria

- With the Grok CLI installed, `grok` is detected as installed and is selectable as an executor in a profile; a node bound to `grok` runs headless with argv `agent -p <prompt> -m <model>`.
- With the Cursor CLI installed, `cursor` is detected as installed and is selectable; a node bound to `cursor` runs headless with argv `cursor-agent -p <prompt> --model <model>` and can resume a session.
- Grok shows curated xAI model suggestions in the profile UI; Cursor accepts existing provider models or free-text.
- assets/models.yaml parses with the new xAI rows and an updated `as_of`.
- New unit tests pass, and `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `code-ranker check .` are clean.
