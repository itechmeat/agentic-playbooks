# Install Story Design (v0.9.0)

Status: approved design, pending implementation.

## Goal

Give apb a modern, low-maintenance install story: a one-line shell installer, a classic Homebrew install, and a built-in self-update command, all driven by the existing tag-push release flow. Refresh the stale install documentation as a headline deliverable of the same release.

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

- `dist init` adds the dist workspace config (`dist-workspace.toml` or `[workspace.metadata.dist]`, whichever current dist writes) and regenerates `.github/workflows/release.yml`. The generated workflow replaces the hand-written one; the exact dist version is pinned in the config and verified as the latest stable at implementation time.
- Same four targets as today. No Windows builds in this release; dist makes adding them later a config change.
- Web frontend build is injected into the generated workflow through dist's `github-build-setup` extension point: install bun, `bun install --frozen-lockfile`, `bun run build` in `web/` before cargo builds, because `apb-server` embeds `web/dist` at compile time.
- The current pre-build quality gate (fmt, clippy, nextest, doc tests) must still block publishing on tag pushes. Preferred mechanism: a gate expressed through dist's supported extension points (custom or plan-stage jobs); if dist cannot express a blocking pre-build gate, the gate runs from `github-build-setup` at the start of every build job. Which mechanism dist 0.3x actually supports is a verification task for the implementation plan.
- Release notes convention is preserved: `docs/release-notes/vX.Y.Z.md` must exist for the tag and must end up as the GitHub Release body. If dist's own changelog/body options cannot consume it directly, a post-announce job applies it with `gh release edit --notes-file`. The fail-fast check that the file exists stays in the pipeline.
- Artifact names may change to dist conventions (archive format and naming are dist's choice, checksums included). That is acceptable; all docs that mention artifact names are updated to match, and the old hand-rolled naming is not preserved artificially.
- `dist plan` runs in PR CI as a dry-run so config drift between the dist config and the committed workflow fails a pull request instead of a release.

## New external assets (owner approval required per action)

- Public repo `itechmeat/homebrew-agentic-playbooks` for the tap.
- A fine-grained personal access token limited to that tap repo, stored in the main repo as an Actions secret (name per dist docs) so the release workflow can push formula updates.
- Both are created during implementation, each with an explicit per-action approval; nothing is pushed or created without it.

## Self-update details

- New dependency: axoupdater (library mode) in `apb-cli`. Version verified as latest stable at implementation time.
- `apb self-update` checks for a newer release, prints current and available versions, and updates in place; `apb self-update --check` only reports. Non-TTY use is supported (exit codes: 0 up to date or updated, distinct nonzero for update available in check mode and for failures).
- Checksum verification of downloaded artifacts is mandatory; a failed verification aborts with no change to the installed binary.
- User-facing messages follow the repo conventions: no exclamation marks, no em-dashes, English machine fields.

## Documentation deliverables (headline scope)

- `docs/INSTALL.md`: full rewrite around the three install paths, a manual download-and-verify path for the security conscious (tarball plus `.sha256`), the contributor source build, uninstall (including removing the receipt), and update guidance per install method.
- `README.md` Install section: condensed version of the same, one-liner first; stale "planned for v0.1.0" claims removed.
- `packaging/apb.rb`: deleted (replaced by the dist-managed tap). The `packaging/` directory goes away if nothing else remains.
- `llms.txt` agent-install path updated to prefer the one-liner.
- `docs/release-notes/v0.9.0.md` written per the existing convention.
- CLAUDE.md and AGENTS.md release-command sections updated in sync if the release process description changes (mirror rule).

## Deferred (recorded, not lost)

- crates.io publishing and cargo-binstall support: deferred; must be revisited within 2 to 5 releases (by v0.13.0). Reminders exist in the assistant's persistent project memory and in this section; when the PR for this release is opened, a tracking GitHub issue is proposed to the owner as a visible in-repo marker. Prerequisites recorded: `description` fields for all crates, packaging built `web/dist` into the published `apb-server` crate (include list overriding gitignore).
- Windows builds plus PowerShell installer, MSI, winget, scoop: when Windows targets are added.
- npm shim, homebrew-core migration: only if demand appears.

## Testing

- Unit and integration tests for `apb self-update` argument handling, `--check` exit codes, and the no-receipt refusal path (network calls mocked or gated; no live GitHub calls in the test suite).
- CI: `dist plan` dry-run on PRs; the standard workspace gates (fmt, clippy dev and release, nextest, doc tests, web vitest and check, code-ranker) all green before merge.
- Post-release manual verification checklist (performed on the v0.9.0 release): one-liner install on macOS, `brew install` from the tap, `apb self-update` from a v0.8.0-installed binary, checksum mismatch simulation is not required live but the abort path is covered by tests.

## Risks

- dist is single-maintainer OSS. Hedge: generated workflow and installers are committed plain YAML and shell; if dist stalls, freeze the working version and maintain the output by hand.
- Replacing a proven release.yml is the riskiest step; mitigations are the PR dry-run, keeping the old workflow in git history, and the manual post-release checklist above.
- Self-update executes downloaded code by design; checksum verification and HTTPS-only download are non-negotiable, and the command never runs implicitly.
