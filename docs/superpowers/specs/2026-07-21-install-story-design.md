# Install Story Design (v0.9.0)

Status: approved design, pending implementation.

## Goal

Give apb a modern, low-maintenance install story: a one-line shell installer, a classic Homebrew install, and a built-in self-update command, all driven by the existing tag-push release flow. Turn `apb init` into a polished interactive onboarding questionnaire. Refresh the stale install documentation as a headline deliverable of the same release.

## Background

The release workflow (`.github/workflows/release.yml`) already builds self-contained `apb` binaries (web UI embedded via rust-embed) for four targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, and publishes tarballs plus `.sha256` files on every `v*` tag. What is missing is the install layer on top of those artifacts:

- `docs/INSTALL.md` and the README Install section still describe prebuilt binaries and Homebrew as "planned for v0.1.0" while releases v0.1.0 through v0.8.0 already ship them. The only documented path is clone plus `cargo install --path`.
- `packaging/apb.rb` is an unfilled Homebrew formula template pinned to 0.1.0.
- There is no install script, no tap, no self-update, and the crates are not publishable to crates.io (no `description` fields; `cargo install` from a registry would also fail because rust-embed needs `web/dist` at compile time).

Research (2026-07, web survey of community recommendations and of what uv, ruff, atuin, starship, zoxide, jj and just actually ship) shows the community standard for Rust CLIs is a dist-generated stack: uv, ruff, and atuin all ship a curl one-liner, Homebrew, and self-update from a single dist config. dist (formerly cargo-dist, repo `axodotdev/cargo-dist`) is actively maintained (0.32.0, 2026-05); the Astral fork was merged back in 0.29.0. The axo company wound down, leaving single-maintainer risk, but everything dist generates is plain YAML and shell committed to our own repo and running on GitHub Actions, so the output remains ours to freeze or fork.

## Decision

Adopt dist as the release and installer generator. Ship three user-facing install paths in v0.9.0:

1. Shell one-liner (macOS and Linux): `curl --proto '=https' --tlsv1.2 -LsSf https://github.com/itechmeat/agentic-playbooks/releases/latest/download/apb-installer.sh | sh`. The installer is a dist-generated release asset: it detects OS and arch, downloads the matching tarball, verifies the sha256 checksum, installs the binary, and writes an install receipt.
2. Homebrew: `brew install itechmeat/agentic-playbooks/apb` via a new public tap repository `itechmeat/homebrew-agentic-playbooks` whose formula dist updates automatically on every release. The name follows the GitHub owner of the main repository; the local folder name omniteamhq is unrelated and must not appear in any user-facing name.
3. Self-update: `apb self-update` as a first-class subcommand built on the axoupdater library (the same mechanism as `uv self update`). It reads the dist install receipt, checks GitHub Releases, verifies checksums, and replaces the binary in place. When no receipt exists (Homebrew or source installs) it exits with a clear message pointing to the right update path instead of touching the binary.

Source builds stay documented as the contributor path (clone, build web, `cargo install --path crates/apb-cli`).

## Release pipeline changes

- `dist init` adds the dist workspace config (`dist-workspace.toml` at the repo root, the current format since dist 0.23) and regenerates `.github/workflows/release.yml`. The generated workflow replaces the hand-written one; the dist version (0.32.0 at research time, re-verified as latest stable at implementation) is pinned in the config.
- Same four targets as today. No Windows builds in this release; dist makes adding them later a config change.
- Web frontend build is injected into the generated workflow through dist's `github-build-setup` extension point: install bun, `bun install --frozen-lockfile`, `bun run build` in `web/` before cargo builds, because `apb-server` embeds `web/dist` at compile time.
- The current pre-build quality gate (fmt, clippy, nextest, doc tests) must still block publishing on tag pushes. Mechanism: `plan-jobs = ["./test-gate"]` in the dist config, pointing at a reusable workflow committed in `.github/workflows/`; it runs at the plan stage and failing it stops the release before any build. This config-driven job survives `dist generate` regeneration; the generated workflow itself stays unedited (no `allow-dirty`).
- Release notes convention is preserved: `docs/release-notes/vX.Y.Z.md` must exist for the tag and must end up as the GitHub Release body. dist only reads a root changelog, not per-tag files, so dist creates the release and a `post-announce-jobs` workflow applies the body with `gh release edit <tag> --notes-file docs/release-notes/<tag>.md`. The fail-fast check that the notes file exists moves into the test-gate workflow so a missing file stops the release before any build.
- Artifact names may change to dist conventions (archive format and naming are dist's choice, checksums included). That is acceptable; all docs that mention artifact names are updated to match, and the old hand-rolled naming is not preserved artificially.
- `dist plan` runs in PR CI as a dry-run so config drift between the dist config and the committed workflow fails a pull request instead of a release.

## New external assets (owner approval required per action)

- Public repo `itechmeat/homebrew-agentic-playbooks` for the tap.
- A personal access token with write access limited to that tap repo, stored in the main repo as the Actions secret `HOMEBREW_TAP_TOKEN` (the name dist's generated publish job expects) so the release workflow can push formula updates.
- Both are created during implementation, each with an explicit per-action approval; nothing is pushed or created without it.

## Self-update details

- New dependency: axoupdater (library mode, `blocking` feature so no tokio runtime is added) in `apb-cli`. Version verified as latest stable at implementation time (0.10.x at research time). The dist config sets `install-updater = true` so installers write the install receipt (under the XDG config dir for apb) that axoupdater reads.
- When no install receipt exists (Homebrew or source installs), axoupdater returns its no-receipt error; `apb self-update` maps it to a clear message naming the right update path (brew upgrade or rebuild from source) and exits nonzero without touching the binary.
- `apb self-update` checks for a newer release, prints current and available versions, and updates in place; `apb self-update --check` only reports. Non-TTY use is supported (exit codes: 0 up to date or updated, distinct nonzero for update available in check mode and for failures).
- Checksum verification of downloaded artifacts is mandatory; a failed verification aborts with no change to the installed binary.
- User-facing messages follow the repo conventions: no exclamation marks, no em-dashes, English machine fields.

## Interactive apb init

`apb init` becomes a clack-style questionnaire (crate: cliclack, the Rust port of the library behind the skills.sh aesthetic; version verified as latest stable at implementation time). The existing behavior (create `.apb/{playbooks,profiles,runs}` and `.apb/config.yaml` if absent) stays first and unchanged; the questionnaire runs after it, only when stdin and stdout are terminals. In non-TTY contexts (CI, agent-driven runs) the command behaves exactly as today: same output line, same exit codes, no prompts.

Questionnaire steps:

1. Feedback-loop consent, default Yes. The question explains transparently: after supervised playbook runs, the coding agent will search existing issues and file anonymized apb error reports as consolidated issues at https://github.com/itechmeat/agentic-playbooks, never including secrets or private prompt content. On consent, for each of `CLAUDE.md` and `AGENTS.md` in the current directory independently: if the file exists, append the canonical feedback-loop block (the inner markdown block from the README section "Help apb improve: the feedback loop", starting at heading `## apb feedback loop (standing instruction)`) to the bottom separated by one blank line; if it does not exist, create the file with exactly that block as content. On decline, touch nothing.
2. Agent subscriptions survey: the existing hand-rolled survey logic (detected agents via `agents_detect`, `agent[:plan[:coverage]]` semantics from `subscriptions_cmd`) re-skinned as cliclack prompts inside the same questionnaire. Semantics and stored state format do not change; only the presentation does. The step is offered when onboarding is uninitialized, mirroring today's gating, and is skippable.

Idempotency is a hard requirement: repeated `apb init` must never break or duplicate anything. Concretely: `.apb` dirs and `config.yaml` are already re-run safe (create if absent only); the feedback-loop block is appended only when the file does not already contain the `## apb feedback loop` heading, and an already-configured file is reported as such instead of asked about again; re-answering the subscriptions step overwrites the subscription state the same way `apb subscriptions set` does today, which is safe. Cancelling the questionnaire (Ctrl+C or ESC) leaves everything already written on disk valid and exits cleanly without an error stack.

The canonical feedback-loop block lives once in the codebase as an asset file included at compile time (`include_str!`) by `apb-cli`; a unit test asserts the README still contains the asset text verbatim so the two cannot drift. Prompt copy follows repo conventions: English, no exclamation marks, no em-dashes.

## Documentation deliverables (headline scope)

- `docs/INSTALL.md`: full rewrite around the three install paths, a manual download-and-verify path for the security conscious (tarball plus `.sha256`), the contributor source build, uninstall (including removing the receipt), and update guidance per install method.
- `README.md` Install section: condensed version of the same, one-liner first; stale "planned for v0.1.0" claims removed.
- `packaging/apb.rb`: deleted (replaced by the dist-managed tap). The `packaging/` directory goes away if nothing else remains.
- `llms.txt` agent-install path updated to prefer the one-liner.
- `docs/release-notes/v0.9.0.md` written per the existing convention.
- README and `docs/HOWTO-authoring.md` (or the closest getting-started doc) mention the interactive `apb init` questionnaire and its non-TTY fallback.
- CLAUDE.md and AGENTS.md release-command sections updated in sync if the release process description changes (mirror rule).

## Deferred (recorded, not lost)

- crates.io publishing and cargo-binstall support: deferred; must be revisited within 2 to 5 releases (by v0.13.0). Reminders exist in the assistant's persistent project memory and in this section; when the PR for this release is opened, a tracking GitHub issue is proposed to the owner as a visible in-repo marker. Prerequisites recorded: `description` fields for all crates, packaging built `web/dist` into the published `apb-server` crate (include list overriding gitignore).
- Windows builds plus PowerShell installer, MSI, winget, scoop: when Windows targets are added.
- npm shim, homebrew-core migration: only if demand appears.

## Testing

- Unit and integration tests for `apb self-update` argument handling, `--check` exit codes, and the no-receipt refusal path (network calls mocked or gated; no live GitHub calls in the test suite).
- Unit tests for the feedback-loop file logic in a tempdir: create-when-missing, append-when-present, no duplicate on re-run, decline touches nothing; plus the README-vs-asset drift test. The prompt layer stays a thin untested shell over tested logic; non-TTY skip is covered by an integration test running `apb init` with piped stdio.
- CI: `dist plan` dry-run on PRs; the standard workspace gates (fmt, clippy dev and release, nextest, doc tests, web vitest and check, code-ranker) all green before merge.
- Post-release manual verification checklist (performed on the v0.9.0 release): one-liner install on macOS, `brew install` from the tap, `apb self-update` from a v0.8.0-installed binary, checksum mismatch simulation is not required live but the abort path is covered by tests.

## Risks

- dist is single-maintainer OSS. Hedge: generated workflow and installers are committed plain YAML and shell; if dist stalls, freeze the working version and maintain the output by hand.
- Replacing a proven release.yml is the riskiest step; mitigations are the PR dry-run, keeping the old workflow in git history, and the manual post-release checklist above.
- Self-update executes downloaded code by design; checksum verification and HTTPS-only download are non-negotiable, and the command never runs implicitly.
