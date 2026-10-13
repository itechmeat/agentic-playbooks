# Release checklist

Maintainer steps for every tagged release `vX.Y.Z`. The release pipeline is
described in `CLAUDE.md` / `AGENTS.md` ("Release pipeline (dist)").

## In the release pull request

- Bump `version` in the root `Cargo.toml` (`[workspace.package]`) and in the
  `version = ` of the path dependencies in `crates/apb-cli/Cargo.toml` and
  `crates/apb-server/Cargo.toml`, then refresh `Cargo.lock`
  (`cargo check --workspace`).
- Add `docs/release-notes/vX.Y.Z.md`. The release test gate fails without it,
  and its text becomes the GitHub release body.
- Update the "latest release" line in `README.md`.
- When Rust or web dependencies changed, regenerate the third-party license
  notices (`scripts/third-party-notices.sh`, see "Third-party notices" in
  `CONTRIBUTING.md`) and commit the result. CI fails when the committed file is stale.
- When `dist-workspace.toml` or a file it references changed, re-run
  `dist generate` and commit the regenerated `.github/workflows/release.yml`.
  Never edit `release.yml` by hand.
- When `docs/HOWTO-interview.md` changed since the last release, run the
  emulated scripted-interview acceptance from
  `docs/superpowers/specs/2026-08-15-playbook-interview-design.md` (Testing
  section).
- Gates are green: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo clippy --release`, `cargo test --workspace`,
  `cd web && bun run check && bun run test`, `cargo deny --all-features check`,
  `code-ranker check .`.

## After the merge

- Tag the merge commit `vX.Y.Z` and push the tag.
- Watch the release workflow: test gate legs, build legs, attestations, the
  Homebrew formula publish and the release-notes job.
- Check the published release: archives for every target with `.sha256`
  files, `apb-installer.sh`, and `gh attestation verify` on one archive.
- Install from scratch with the one-liner into an empty `CARGO_HOME` and run
  `apb --version`, `apb init`, `apb doctor`.
- Confirm that the archives hold `LICENSE` and the third-party notices, and
  no runtime state, logs, tokens or credentials.
- Confirm that `apb dashboard` still binds to loopback by default.

## Repository settings (verify occasionally)

- The `main` ruleset requires pull requests and the checks
  `build-and-test`, `test-macos`, `DCO` and `dependency-review`; force pushes
  and branch deletion are blocked.
- Dependabot alerts and security updates, secret scanning with push
  protection, and private vulnerability reporting are enabled.
- Every commit carries a DCO `Signed-off-by` line.
