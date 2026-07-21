# AGENTS.md

This file provides guidance to coding agents working in this repository.

> Mirror rule: AGENTS.md and CLAUDE.md must stay in sync. Any change to the
> shared guidance in one file must be mirrored in the other in the same change.
> CLAUDE.md additionally carries a small "Claude-specific" section with no twin
> here.

## What this is

`apb` - a local runner for agentic playbooks: YAML playbook definitions, a
svelte-flow web visualizer, semver versioning, and an MCP server so coding
agents can drive playbooks. Rust workspace, edition 2024. Full design spec:
`docs/superpowers/specs/2026-07-08-workflows-cli-design.md` and the agent-profiles
spec `docs/superpowers/specs/2026-07-12-agent-profiles-design.md`.

## Crates (big picture)

Five crates under `crates/`; the dependency direction is core <- engine <- mcp,
with cli and server on top. Do not introduce import cycles (enforced by
code-ranker, see below).

- `apb-core` - domain layer, no async. Playbook schema (`schema.rs`,
  `Playbook::from_yaml`), validator (`validate.rs`, codes V01+), profiles
  (`profile.rs` types incl. `ProfileError`, `profile_store.rs` scope resolution
  and bundle trust, `skills.rs`), registry and versioning, schema 1->2 migrator
  (`schema_migrate.rs`), free agent detection (`detect.rs`), curated models table
  (`models_table.rs` + `assets/models.yaml`), content/bundle digests
  (`content.rs`), trust store (`trust.rs`), atomic state IO and dir locks
  (`fsutil.rs`).
- `apb-engine` - execution. The drive loop and node execution (`scheduler.rs`),
  the immutable write-once run manifest (`manifest.rs`), invocation resolution
  (`invocation.rs`), agent adapters (`adapter.rs`), the append-only event log
  (`event.rs`), background supervisor spawn, legacy run-resume shim
  (`legacy_snapshot.rs`).
- `apb-mcp` - rmcp stdio MCP server (`server.rs`), the server-side run policy gate
  (`policy.rs`), and profile/advisory/supervisor tools.
- `apb-cli` - package name `apb` (bin `apb`, `main.rs`); thin dispatch over
  core/engine/mcp. `apb init` runs a short interactive questionnaire in a
  terminal (feedback-loop consent into CLAUDE.md/AGENTS.md, agent
  subscriptions survey); non-TTY runs skip it. `apb self-update` updates
  installer-based installs in place.
- `apb-server` - axum web API (`lib.rs`) with the svelte frontend from `web/`
  embedded via rust-embed (`web/dist`).

### Concepts that span files

- A **profile** is the single executor binding for an `agent_task` node: agent +
  model + ordered fallbacks + SOUL (role prompt) + skills, with a scope
  (Project | Global | Auto). Nodes reference it by name or `{ name, scope }`.
- `profile_digest` = hash of profile.yaml + SOUL.md; `bundle_digest` = profile
  digest plus the actual content digests of its skills. Editing a skill changes
  the bundle and drops trust.
- A run snapshots its resolved profiles, skills, and invocations into an
  immutable, write-once `runs/<id>/manifest.yaml`. All post-start reads (retry,
  fallback, resume) come from that snapshot, not live files.
- Anti-TOCTOU: the MCP policy gate (`policy::check_run`) returns a `RunPermit`
  (playbook digest + the exact verified profile-bundle map) in one pass; the
  engine is handed that map verbatim as `expected_*` and rejects any drift.
  Never recompute that map separately between gate and run.
- schema 2 removed the old `executors` block; playbooks bind executors only
  through profiles. `apb migrate` converts legacy schema 1 playbooks.
- The supervisor is an optional agent that watches a run and intervenes via the
  `supervisor_*` MCP tools; its binding exists in the manifest only when the run
  is actually externally supervised.

## Commands

Build (debug): `cargo build`. Release (bakes the frontend first):
`cd web && bun install && bun run build` then `cargo build --release`.

Test everything: `cargo test --workspace`. A single test:
`cargo test -p <crate> --test <file> <test_name>` (e.g.
`cargo test -p apb-engine --test profile_run_test legacy_run_resume_via_ephemeral_snapshot`).

Frontend (`web/`, bun + vite + vitest): `bun run test`, `bun run build`,
`bun run check`.

Format and lint gates (must be clean):
`cargo fmt --all -- --check` and
`cargo clippy --workspace --all-targets -- -D warnings`.

### Release pipeline (dist)

Releases are built by [cargo-dist](https://opensource.axo.dev/cargo-dist)
0.32.0, configured in `dist-workspace.toml`. `dist generate` writes
`.github/workflows/release.yml` from that config; the file is GENERATED and
must never be hand-edited directly. To change release CI, edit
`dist-workspace.toml` (and the referenced `.github/build-setup.yml`,
`.github/workflows/test-gate.yml`, `.github/workflows/release-notes.yml`),
then re-run `dist generate` and commit the regenerated workflow alongside the
config change.

Pushing a tag `vX.Y.Z` triggers the release workflow: it builds the shell
installer, the Homebrew formula (tap `itechmeat/homebrew-agentic-playbooks`),
and archives for the configured targets. A test gate (fmt, clippy, nextest,
doctests, and a check that `docs/release-notes/<tag>.md` exists) runs both as
a fast plan-stage job and again inside every build leg, so a failing gate
physically blocks publishing rather than just warning. `dist plan` also runs
on every PR (`pr-run-mode = "plan"`) to catch config drift before a tag is
pushed. `docs/release-notes/<tag>.md` is required per tag and becomes the
published release body via a post-announce job.

## Required gates

- **Before code is ready to commit:** run code-ranker and fix any violation.
  First warm the cargo cache (the rust plugin needs it, else an offline error):
  `cargo metadata --format-version 1 >/dev/null`, then `code-ranker check .`
  (exit != 0 on a violation). For a violation, read `code-ranker docs base <ID>`
  before fixing, fix, and re-run until clean. It guards dependency cycles (ADP),
  cohesion, complexity, and SOLID/DRY/KISS.
- **Before code is considered ready for release:** run `cargo clippy --release`.
  Fix every error in the logs; also clean up warnings as far as practical.
- Commit only after the owner approves. Do not commit or push on your own
  initiative.
- **Never upload, publish, or send any file to any server or external service
  without the owner's explicit per-action approval** (git push, artifacts,
  deploys, package registries, file sharing - any network destination). Local
  writes are fine; anything leaving the machine needs an approval first.

## Conventions

- No em-dashes (U+2014) and no exclamation marks in docs or user-facing strings.
  No CJK anywhere in code or prose. Machine-facing fields are English;
  user-facing chat messages are written in the user's chat language.
- New `EventPayload` fields are added only with `#[serde(default)]`.
- State files are written atomically (temp + rename, 0600 on unix) via
  `apb_core::fsutil`.
- Secret values (auth files) are never returned, logged, or cached; skill
  content is never embedded into a prompt (skills are delivered by name, or as
  materialized snapshot copies for isolated nodes).
- Profile and skill names: `[a-z0-9][a-z0-9-]*`, at most 64 chars; path segments
  are validated with `is_safe_segment` / `validate_profile_name`.
- Navigate the code through a symbol/edge index rather than ad-hoc grep where
  possible.
