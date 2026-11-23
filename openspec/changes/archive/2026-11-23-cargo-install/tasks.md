# Tasks: cargo install

## Before anything

This branch was written on a main without one-point-oh-freeze (its tree
says `1.0.0-beta.7` and has no `docs/COMPATIBILITY.md`). The orchestrator
rebases it onto main after one-point-oh-freeze has merged, before the
implementer starts. The implementer checks precondition 1 first and
stops with a report if it does not hold; never work around it.

Work in order, one task at a time. Before a section, read its part of
`design.md` (named in the section title) and the spec deltas under
`specs/`. Tick each box (`- [x]`) as soon as the task is done, so an
interrupted run can resume from the first open box. Partial edits from an
earlier attempt may already be in the tree: review them against
`design.md`, keep what is right, finish the rest.

Every design choice is made in `design.md`. If something there does not
fit the code (a function was renamed, a file moved), find the site by the
name `design.md` gives, keep the decision, and note the difference in the
final report. Never invent a different behaviour.

For every behaviour: write the failing test first, run it and see it
fail, then write the code. A compile error is an acceptable first failure
only for a test that names a type or function that does not exist yet.

## Preconditions

1. one-point-oh-freeze is merged and this branch is rebased onto it:
   `Cargo.toml` `[workspace.package] version` is `1.0.0-rc.1`,
   `docs/COMPATIBILITY.md` and `docs/release-notes/v1.0.0-rc.1.md` exist,
   and `git merge-base --is-ancestor origin/main HEAD` succeeds. If any of
   it fails, stop and report it. `<V>` below is that version.
2. Record the release plan of the starting main before any edit:
   `dist plan --output-format=json > $TMPDIR/dist-plan-before.json`.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-cargo-install-target TMPDIR=$HOME/.cache/apb-cargo-install-tmp
mkdir -p "$TMPDIR"
```

Never use `/dev/shm`, and never put a target dir, cache or temp data
inside the worktree. Before the first cargo command build the dashboard
(after section 2 it lands in `crates/apb-server/dashboard/`, before it in
`web/dist/`): `(cd web && bun install --frozen-lockfile && bun run build)`;
it needs network access for the install. Never touch the real
`~/.config/apb`. Never stop a process you did not start. Do not commit,
push, open a pull request, publish a crate or call any crates.io write
endpoint. Do not start subagents. Search the code with
`zg --rg -F <symbol> <path>` first.

Inside a task run only the tests it names:

1. A unit test module of a library: `cargo test -p <package> --lib <module path>`.
   The CLI package `apb` has a binary target only and no library, so
   `--lib` fails for it; its unit tests run through the binary target:
   `cargo test -p apb --bin apb <module path>` (for example
   `cargo test -p apb --bin apb selfupdate`).
2. An integration test file: `cargo test -p <package> --test main <file
   module>`. The CLI package stays `apb`: `cargo test -p apb --test main
   crate_manifests_test`. After section 1 the libraries are
   `-p agentic-playbooks-core` and so on.

## 1. Package names (design D2)

- [x] 1.1 Write `crates/apb-cli/tests/suite/crate_manifests_test.rs` with
  `every_internal_dependency_is_exact`, `library_names_stay` and
  `the_cli_package_stays_apb` (design D5, test list items 1 to 3), declare
  it in `crates/apb-cli/tests/main.rs`; run it and see the first two fail.
- [x] 1.2 Add the six internal entries to `[workspace.dependencies]` of
  `Cargo.toml` with `version = "=<V>"` and the comment of design D2; add
  `homepage` and `rust-version` to `[workspace.package]` (record the CI
  stable version you used in the final report).
- [x] 1.3 In the six library manifests: the new package name, `[lib] name`,
  `homepage.workspace = true`, `rust-version.workspace = true`. In all
  seven manifests: every normal internal dependency becomes
  `<key>.workspace = true` (features kept); every internal dev-dependency
  stays path-only with `package = ...` added (self entries included). The
  path dependency versions the freeze wrote in `crates/apb-cli/Cargo.toml`
  and `crates/apb-server/Cargo.toml` disappear with this.
- [x] 1.4 `cargo check --workspace --all-targets`; `Cargo.lock` changes
  only by the renames (compare the sorted package entries as design
  Evidence item 3 describes; no version, source or checksum may change).
- [x] 1.5 Run the tests of 1.1 and see them pass.
- [x] 1.6 The `-p apb-<library>` mentions: `ts_contract.rs` (`HEADER`, the
  module doc and the drift message), then regenerate with
  `APB_WRITE_TS=1 cargo test -p agentic-playbooks-server --lib ts_contract`
  (only the header line of the two generated files may change);
  `crates/apb-engine/src/decision/report/tests.rs`,
  `crates/apb-engine/tests/suite/agent_env_live_test.rs`. Check with
  `git grep -nE -- '-p apb-(core|engine|server|mcp|mcp-client|decide)\b' -- ':(exclude)openspec' ':(exclude)docs/superpowers' ':(exclude)docs/release-notes'`:
  only the docs of section 7 may remain.
- [x] 1.7 `dist plan --output-format=json > $TMPDIR/dist-plan-after.json`
  and compare with the file of precondition 2: the same app, artifact
  names and install hints. A difference is a stop.

## 2. The dashboard path (design D3)

- [x] 2.1 `web/vite.config.ts`: `build.outDir` and `build.emptyOutDir`;
  `.gitignore`: the new path. Rebuild the dashboard and check that
  `crates/apb-server/dashboard/index.html` exists and `git status` shows
  nothing new.
- [x] 2.2 `crates/apb-server/build.rs` and `#[folder = "dashboard"]` in
  `assets.rs` with its comment. Check by hand: move
  `crates/apb-server/dashboard` aside, `cargo check -p agentic-playbooks-server`
  fails with the design D3 message; an empty directory fails the same
  way; move it back and the check passes. Record the message in the final
  report.
- [x] 2.3 The comments and docs of the design D3 table that are not in
  section 7: `ci.yml`, `test-gate.yml`, `connector/official.rs`,
  `scripts/third-party-notices.sh`, `web/scripts/third-party-notices.ts`,
  `scripts/daily-cleanup.sh` `regenerable_paths`.
- [x] 2.4 `cargo test -p agentic-playbooks-server` (the assets and build id
  tests) and `cd web && bun run check && bun run test`.

## 3. Self-contained packages (design D4)

- [x] 3.1 The seven `LICENSE` links and the nine `embedded/` links, created
  with `ln -s` exactly as design D4 names them; `git add` them and check
  `git ls-files -s` shows mode `120000` for each.
- [x] 3.2 Point the embeds of `apb-core` and `apb-mcp` at the links
  (design D4 items 2 and 3); `cargo test -p agentic-playbooks-core --test main templates_test`,
  `cargo test -p agentic-playbooks-core --test main bundled_content_lint_test`,
  `cargo test -p agentic-playbooks-core --lib models_table`,
  `cargo test -p agentic-playbooks-core --lib cloud_models`,
  `cargo test -p agentic-playbooks-mcp --lib setup` pass.
- [x] 3.3 Add the test `libraries_say_they_are_not_an_api` to
  `crate_manifests_test.rs`; see it fail.
- [x] 3.4 `include`, descriptions, keywords, categories and `readme` of
  design D4 in the seven manifests; the paragraph at the top of the six
  `src/lib.rs`; `crates/apb-cli/README.md`. See 3.3 pass.
- [x] 3.5 `cargo package --list -p <package> --allow-dirty` for each of the
  six libraries and `-p apb`: each list holds `LICENSE`; the server's
  holds `dashboard/index.html`; the core's holds `embedded/bundled/...`;
  no list holds `tests/`.

## 4. The publishability gate (design D5)

- [x] 4.1 Add `the_publish_order_is_a_dependency_order`,
  `the_script_refuses_a_token_where_code_runs` and
  `an_all_skipped_plan_runs_no_cargo_command` (design D5, test list items
  5 to 7) to `crate_manifests_test.rs`; see them fail (no script yet).
- [x] 4.2 Write `scripts/crates.sh` with the shared preparation (the token
  refusal first, the `.cargo/config` refusal, the exact two-line rename
  with `cargo metadata --locked` and the restore trap, the dependency
  read, the dashboard check), the plan check of `repack` and `publish`
  that takes the all-skipped path before the preparation, the guard
  against an empty package selection, and the five commands `check`,
  `preflight`, `repack`, `publish` and `installed` of design D5; make it
  executable. See 4.1 pass.
- [x] 4.3 The script refuses a modified CLI manifest or lockfile, and the
  worktree holds this change uncommitted, so run it in a throwaway copy:
  `git add -N .` (marks new files, links included, so the diff carries
  them), `git clone --local . $TMPDIR/crates-check`,
  `git diff --binary HEAD | git -C $TMPDIR/crates-check apply --index`,
  a commit inside the copy only (`git -C $TMPDIR/crates-check commit -qm wip`),
  then build the dashboard there and run `scripts/crates.sh check` there
  with `CARGO_HOME` set to a new empty directory under `$TMPDIR` (the
  cold cache of design evidence 4). It must pass; record the seven crate
  sizes and `SHA256SUMS` in the final report. Then check that the copy's
  `crates/apb-cli/Cargo.toml` and `Cargo.lock` are unchanged
  (`git -C $TMPDIR/crates-check status --short` shows only ignored
  output). Nothing is committed in the worktree.
- [x] 4.4 In the same copy, without any token in the environment:
  `scripts/crates.sh preflight <V> <target dir>/package/SHA256SUMS` runs
  no cargo command, finds the seven names absent from crates.io (they are
  new) and writes `publish-plan` with seven `publish` lines; with
  `GITHUB_OUTPUT` set to a file under `$TMPDIR` that file gains
  `nothing_to_publish=false`. Then `scripts/crates.sh repack <V> <target dir>/package`
  packs again with `--no-verify` and reports every checksum equal; a sums
  file with one checksum changed makes it stop naming that crate and both
  checksums. A plan edited by hand to seven `skip` lines makes `repack`
  print its all-skipped line and exit 0 without running cargo (check
  with `strace -f -e trace=execve` or a `cargo` stub first on `PATH`);
  a plan with one `publish` line packs that one crate only. Never run
  `publish`.
- [x] 4.5 Prove the gate fails: in the throwaway copy, add to `apb-core`
  a production `include_str!` of a root file outside the crate (for
  example `README.md`) and see `check` fail in the verify build; remove
  the dashboard and see it fail on the missing dashboard; change one
  character of the copy's `crates/apb-cli/Cargo.toml` without committing
  and see every command refuse to start and leave the file as it was.
  Discard the copy.
- [x] 4.6 Write `the_required_check_fails_unless_every_linux_job_succeeded`
  in `release_workflow_test.rs` (design D5, the aggregate job) and see it
  fail. Then `ci.yml`: the job `crates`, `crates` in the `needs` of
  `build-and-test`, the env variable `CRATES: ${{ needs.crates.result }}`
  of its step and the comparison `[ "$CRATES" = "success" ]` in its
  script, exactly as design D5 writes the script. See the test pass:
  with `CRATES` set to `failure`, `cancelled` or `skipped` the step exits
  non-zero.

## 5. Publication (design D6)

- [x] 5.1 Write the eight new tests of design D6 (items 1 to 8 of its
  test list) in `release_workflow_test.rs` (the main check and the
  release check run through `run_with_gh` with every stub answer the
  design lists; the inline artifact check and the tree step run with bash
  over the temporary directories the design describes); see them fail.
- [x] 5.2 Write `.github/workflows/publish-crates.yml` with the jobs
  `prepare`, `publish` and `installed` exactly as design D6 orders their
  steps, with the permissions, environment, conditions and job env of
  its table and items 2, 4, 5 and 6 (only `publish` holds the
  environment `crates-io` and its token, no job has `id-token: write`,
  the "Publish" step fails closed without the token; no cache anywhere),
  every action pinned to a full commit SHA with its tag in a comment (the
  same SHAs ci.yml uses for checkout, bun and Rust, and the SHAs the
  generated `release.yml` uses for `actions/upload-artifact` and
  `actions/download-artifact`). See 5.1 and `every_third_party_action_is_pinned_to_a_full_commit_sha`
  pass.
- [x] 5.3 Check the remaining paths of the script without publishing:
  `scripts/crates.sh preflight 0.0.0 <sums>` refuses (not the workspace
  version) before any network call; `scripts/crates.sh repack 0.0.0 <dir>`
  refuses (the plan does not name that version) before any cargo call;
  `scripts/crates.sh publish <V> <dir>` without `CARGO_REGISTRY_TOKEN`
  refuses; run the step script of "Check the handed-over packages" by
  hand over the copy's `target/package` files, the plan and the dashboard
  gathered as `$RUNNER_TEMP/verified` (with `TAG=v<V>` and `RUNNER_TEMP`
  under `$TMPDIR`) and see it pass; read the sparse index of an
  existing crate (`curl -fsS https://index.crates.io/se/rd/serde | head -c 200`)
  to confirm the prefix rule, and
  `https://crates.io/api/v1/crates/serde` and `.../serde/owners` with the
  script's `User-Agent` to confirm the fields its ownership check reads
  (`crate.name`, `users[].login`, `users[].kind`);
  `scripts/crates.sh installed 0.0.0-none`
  fails on the install with a clear message and leaves no directory
  behind. Never run `publish` with a token.

## 6. Self-update on a cargo install (design D7)

- [x] 6.1 Write the tests of `cargo_install.rs` `mod tests` and the five
  tests of design D7 item 6 in `selfupdate.rs` (both replacement orders,
  equal and older versions, `--check`, no receipt); see them fail.
- [x] 6.2 Write `cargo_install.rs` (`PUBLISHED_PACKAGE`,
  `built_for_cargo_install`, `guidance`), the `self_update_from` seam
  with `env!("CARGO_PKG_NAME")` passed by `self_update_as` (the freeze's
  tests of `self_update_as` keep passing unchanged) and the
  `NO_RECEIPT_GUIDANCE` sentence. Nothing reads `.crates2.json` or the
  binary's path (`git grep -n crates2 -- crates` finds only the test
  `an_installer_build_follows_its_receipt_next_to_a_cargo_record`). `cargo test -p apb --bin apb selfupdate` and
  `cargo test -p apb --bin apb cargo_install` pass (the CLI has a binary
  target only; `--lib` does not apply to it).
- [x] 6.3 In the throwaway copy of 4.3, after the rename (`scripts/crates.sh`
  leaves it restored, so apply it by hand there), build the CLI and check
  that `apb self-update --check` exits 2 with the cargo guidance; without
  the rename the same build follows today's path.

## 7. Docs (design D8)

- [x] 7.1 `docs/INSTALL.md` (the new section 3 with the build tools: the
  Xcode Command Line Tools on macOS, `build-essential` on Debian or
  Ubuntu; the renumbering and every reference to a section number; the
  switch-channels sentence), `llms.txt`. Then
  `git grep -nE 'only Rust|Rust toolchain only|Rust alone' -- docs llms.txt crates/apb-cli/README.md`
  finds nothing.
- [x] 7.2 `docs/COMPATIBILITY.md` and `docs/release-notes/v1.0.0-rc.1.md`
  (the cargo paragraph with the build tools, the section, soak item 7 in
  the `ubuntu:26.04` container with exactly `build-essential`,
  `ca-certificates`, `curl` and rustup).
- [x] 7.3 `CLAUDE.md` and `AGENTS.md` with the same shared text (the mirror
  rule), `CONTRIBUTING.md`, `docs/TESTING-GUIDELINES.md`,
  `docs/BUILD-OPTIMIZATION.md`, `RELEASE-CHECKLIST.md`. Then the check of
  1.6 finds nothing outside history, and
  `git grep -n 'web/dist' -- ':(exclude)openspec' ':(exclude)docs/superpowers' ':(exclude)docs/release-notes' ':(exclude).apb' ':(exclude)README.md' ':(exclude)scripts/old-apb.sh' ':(exclude)scripts/daily-cleanup.sh' ':(exclude)web/.gitignore'`
  finds nothing.
- [x] 7.4 Text rules over every changed file: no U+2013 or U+2014, no
  exclamation mark in docs or user-facing strings, no CJK.

## 8. Gates

- [x] 8.1 `cargo fmt --all -- --check`;
  `cargo clippy --workspace --all-targets -- -D warnings`;
  `cargo clippy --release`.
- [x] 8.2 `cargo nextest run --workspace` (or `cargo test --workspace`) and
  `cargo test --workspace --doc`.
- [x] 8.3 `scripts/third-party-notices.sh --check`; when it reports a
  difference, regenerate and include the file (design "Files",
  conditional).
- [x] 8.4 `cargo metadata --format-version 1 >/dev/null`, then
  `code-ranker check .` until clean (read `code-ranker docs base <ID>`
  before fixing a violation).
- [x] 8.5 `dist plan` once more against precondition 2, and
  `git diff --stat main -- .github/workflows/release.yml dist-workspace.toml .github/build-setup.yml .github/workflows/release-notes.yml`
  is empty.
- [x] 8.6 `scripts/crates.sh check` once more on the final tree, in a
  throwaway copy as in 4.3, with an empty `CARGO_HOME`.

## 8a. The minimum Rust version and unused dependencies (design D2, D5, D8)

- [x] 8a.1 `rust-version = "1.99"` in `[workspace.package]` of
  `Cargo.toml`; `RUSTUP_TOOLCHAIN=1.99.1 cargo check --workspace
  --all-targets --locked` passes (design Evidence item 18). Had it failed,
  the lowest of 1.99 and 1.100 that builds would have been used, with no
  code change for an older Rust.
- [x] 8a.2 `ci.yml`: the job `msrv` (design D5) and `msrv` in the
  `needs`, env and comparison of `build-and-test`;
  `release_workflow_test.rs`: `msrv` in the expected `needs` of
  `the_required_check_fails_unless_every_linux_job_succeeded` and the new
  `the_msrv_job_checks_the_rust_version_of_cargo_toml`. See both fail on a
  workflow without the `msrv` dependency and on a literal toolchain
  version, then pass.
- [x] 8a.3 `docs/INSTALL.md` and `crates/apb-cli/README.md` say "Rust
  1.99 or newer"; the compatibility wording of design D8 item 3 says the
  minimum Rust version is not part of the 1.x promise and may rise in a
  minor release.
- [x] 8a.4 Remove `tower-http` from `apb-server` and `imap-proto` from
  `apb-engine` and `[workspace.dependencies]` after `rg` shows neither in
  its crate's code, tests or features; `Cargo.lock` loses exactly those
  two entries; `scripts/third-party-notices.sh --check`, regenerated when
  it reports a difference.
- [x] 8a.5 The gates of section 8 again on the final tree, and
  `openspec validate cargo-install --strict`.

## 9. Record and archive

- [x] 9.1 Write `docs/superpowers/specs/2026-11-22-cargo-install-design.md`
  (design D8 item 8).
- [x] 9.2 Count the changed files against design "Files" and report any
  difference.
- [x] 9.3 `openspec validate cargo-install --strict`, then
  `openspec archive cargo-install --yes`, and check that
  `openspec/specs/crate-packaging/spec.md`,
  `openspec/specs/crate-publication/spec.md` and
  `openspec/specs/cargo-install-channel/spec.md` exist.
- [x] 9.4 Final report: the moved sites, the recorded values (Rust
  version, crate sizes and checksums, the build error message), every
  gate's result, and the owner steps of
  `proposal.md` that remain.
