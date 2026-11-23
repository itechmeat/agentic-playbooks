# Proposal: cargo install

## Why

The owner decided that apb is published to crates.io so that a machine
with a Rust toolchain and the usual build tools (a C compiler and linker)
installs it with `cargo install agentic-playbooks`. The binary stays `apb`. The channel
ships with 1.0.0 and is first exercised by 1.0.0-rc.1.

Today that command cannot work, for four reasons found on main 121e5c060
(Start rules, #299; apb 1.0.0-beta.7):

1. The names. The CLI package is named `apb`, and the crates.io names
   `apb`, `apb-core` and `apb-cli` belong to unrelated projects. The names
   `agentic-playbooks`, `agentic-playbooks-core`, `-engine`, `-server`,
   `-mcp`, `-mcp-client` and `-decide` were free when this was written.
2. The release pipeline derives its file names from the CLI package name.
   A dry run (`dist plan` on a local copy with the CLI package renamed to
   `agentic-playbooks`) renames the shell installer to
   `agentic-playbooks-installer.sh`, every archive to
   `agentic-playbooks-<target>.tar.xz` and every updater to
   `agentic-playbooks-<target>-update`; only the Homebrew formula can be
   pinned back (`formula = "apb"`). An existing beta install updates
   through `apb-installer.sh` of the newest release, and the docs name
   `.../releases/latest/download/apb-installer.sh`, so that rename breaks
   both.
3. The packages are not self-contained. `cargo package --list` of every
   crate on main lists no `LICENSE`, and four crates embed files from
   outside their own directory, which a crate package cannot hold:
   `apb-server` the built dashboard (`web/dist`), `apb-core` the shipped
   playbooks, the official connectors, the template gallery, the models
   table and the cloud catalog, `apb-mcp` three documents and the standing
   playbook block. A user running `cargo install` has no bun, so the
   dashboard has to be in the published package already built.
4. Nothing publishes, and nothing notices when the workspace stops being
   publishable.

## What changes

1. Package names (design D1, D2). The six library packages are renamed
   `agentic-playbooks-core`, `-engine`, `-server`, `-mcp`, `-mcp-client`
   and `-decide`; each keeps its library name (`[lib] name = "apb_core"`
   and so on) and every dependent names it through one
   `[workspace.dependencies]` entry with `package = ...` and an exact
   version, so no Rust import changes. The CLI package stays `apb` in the
   repository, so `dist` derives exactly the names it derives today; the
   publication step renames it to `agentic-playbooks` in its own checkout
   only, right before packaging.
2. Self-contained packages (design D3, D4). The dashboard is built
   straight into `crates/apb-server/dashboard/` (git-ignored), the one
   path the workspace build, CI, the release build and the published
   package read; a build without it stops with a message that names the
   build command. The other embedded trees are reached through committed
   symbolic links inside each crate (`crates/apb-core/embedded/`,
   `crates/apb-mcp/embedded/`), which `cargo package` resolves into plain
   files. Every crate ships `LICENSE`.
3. A publishability gate in CI (design D5): `scripts/crates.sh check`
   applies the publication rename (an exact edit of two lines, checked by
   `cargo metadata --locked`, no resolver run), runs
   `cargo package --workspace` (which builds every package against the
   others through a temporary local registry, so unpublished
   interdependent crates verify), checks the file lists, the license, the
   dashboard and the sizes, and records the checksum of every package.
4. Publication (design D6): a hand-written workflow,
   `.github/workflows/publish-crates.yml`, follows every successful
   Release run of a version tag, like `release-notes.yml`. Before any
   checkout and any credential it checks that the tag names the commit the
   run built, that this commit is an ancestor of main, and that the GitHub
   release exists and is not a draft. Three jobs on fresh runners: the job
   `prepare`, with no credential, builds the
   dashboard, runs the same `scripts/crates.sh check` on the tagged
   commit, refuses a name someone else owns and a version already on
   crates.io with other bytes, and hands the verified packages, their
   checksums and the plan of what to publish over as an artifact. The job
   `publish`, the only one with the `crates-io` environment and its token,
   runs no build and no code of
   the tagged checkout except the publish script from its own fresh
   checkout: it checks the artifact inline, packs the publishing crates
   again without building, requires the recorded checksums, and only then
   takes the token and publishes with verification off (it fails closed
   with a clear message when the environment holds no token); afterwards
   the index checksums must equal the recorded ones. The job `installed`,
   again without any credential, installs the published version from the
   registry into an empty directory and runs `apb --version`. A rerun
   after a failure half way skips what is already published with the
   verified bytes; when everything is already published, `publish` is
   skipped and `installed` runs at once. Pre-releases are published too.
5. `apb self-update` in a binary compiled from the published package
   (`agentic-playbooks`) names the cargo command and exits 2 without
   touching anything; a binary built as `apb` (installer, source build)
   behaves as today. The kind of install comes from the package name the
   binary was compiled as, never from a record on disk (design D7).
6. Docs (design D8): `docs/INSTALL.md` gains the cargo section,
   `llms.txt` keeps a corrected warning about the unrelated `apb` crate,
   `docs/COMPATIBILITY.md` states what 1.x promises about this channel and
   that the library crates are not a public API, and the rc.1 release
   notes name the command.

All of it is capabilities `crate-packaging`, `crate-publication` and
`cargo-install-channel`.

## What the owner does by hand, once

1. Sign in to crates.io with the GitHub account that owns the repository
   (`itechmeat`, the owner login `scripts/crates.sh` expects) and verify
   the account's email address (crates.io refuses a publish without one).
2. Create a crates.io API token with the scopes `publish-new` and
   `publish-update`, limited to the crate pattern `agentic-playbooks*`,
   valid for one year.
3. In the repository settings create the environment `crates-io`, with
   deployment branches limited to `main` (a `workflow_run` job runs on
   the default branch) and no required reviewers, and store the token as
   its secret `CARGO_REGISTRY_TOKEN`.
4. In the repository settings add a tag ruleset for the pattern `v*`
   that restricts creating, updating and deleting matching tags, with
   only the owner (the repository admin role) in its bypass list, so only
   the owner creates or moves a release tag. The workflow also refuses a
   tag whose commit is not on main.
5. Tag `v1.0.0-rc.1` only after this change is merged (Decision 1), then
   watch the three jobs of `publish-crates`. crates.io limits how fast
   new crates may be published; if the run stops on that limit, use
   "Re-run all jobs" later: crates already published with the verified
   bytes are skipped. If the last job (`installed`) fails, yank the seven
   versions (`cargo yank --version 1.0.0-rc.1 <crate>`) and report it.
6. In a container `ubuntu:26.04` with exactly `build-essential`,
   `ca-certificates` and `curl` from apt and rustup's minimal stable
   toolchain, run
   `cargo install --locked agentic-playbooks --version 1.0.0-rc.1`, then
   `apb --version`, `apb doctor` and `apb dashboard` (its page answers
   HTTP 200), and record the result in the soak (the rc.1 notes, soak
   item 7).

Nothing is reserved ahead of time: no placeholder versions are published;
the rc.1 run is the first publication of every name.

## Out of scope

1. `README.md`. Another branch owns it; its install section and its
   source build line (`(cd web && bun install && bun run build)`, "the
   binary embeds web/dist") are updated there. The path in that line
   changes with this change; the other branch is told in the pull request.
2. cargo-binstall metadata. The release archives are named after `apb`
   while the crate is `agentic-playbooks`, so binstall needs at least a
   `pkg-url`, a `bin-dir` and a `pkg-fmt`, and none of the three can be
   proven correct before a release exists on crates.io. A later change
   adds them with a real `cargo binstall` run.
3. `cargo install --git`: still unsupported. A git checkout has no built
   dashboard, and the build stops with the message of design D3.
4. A Windows target, and `dist` itself: `dist-workspace.toml`,
   `.github/build-setup.yml` and the generated `release.yml` do not
   change.
5. The project's own playbook `apb-task-implement`: its versions under
   `.apb/playbooks/` mention `web/dist` in a prompt; versions are
   immutable, and the next version of that playbook updates the sentence.
6. Historical text that names `web/dist`: archived changes,
   `docs/superpowers/specs/`, older release notes. They describe their
   own time.
7. Library crates as a public API: they are published only so the CLI
   builds, and say so.
8. Trusted publishing: crates.io does not accept it from a `workflow_run`
   workflow, so it needs a different trigger and is a later change; the
   1.0 releases publish with the token.

## Decisions

Decided by the orchestrator for the owner (`design.md` "Decisions"):

1. `v1.0.0-rc.1` is tagged only after this change merges.
2. The 1.0 releases publish with the `crates-io` environment's token;
   no job requests an identity token.
3. The one-line difference of the published CLI manifest and the
   `"dirty": true` flag of every published crate are accepted.
4. The `crates-io` environment needs no required reviewers.
5. docs.rs building the library docs is accepted.

## Impact

1. Crates: every manifest; `[lib] name` in the six libraries; the paths
   of the embedded content in `apb-core`, `apb-mcp` and `apb-server`; a
   new `build.rs` in `apb-server`; `apb` CLI: a check of the compiled
   package name in `apb self-update`. No behaviour of a run, the journal, the schema, the
   MCP tools or any stored file changes.
2. Developer commands: `cargo test -p apb-core` becomes
   `cargo test -p agentic-playbooks-core` (and so on for the six
   libraries); `cargo test -p apb` stays. The dashboard build writes
   `crates/apb-server/dashboard/` instead of `web/dist/`.
3. Release artifacts: unchanged, proven by `dist plan` before and after
   (design D1).
4. Older apb: nothing it reads changes.
5. Behaviour changes for release notes:
   1. apb installs with `cargo install --locked agentic-playbooks`
      (pre-releases with `--version`); it needs Rust and a C compiler
      and linker.
   2. `apb self-update` on a cargo install names the cargo command and
      exits 2.
   3. Building from source writes the dashboard to
      `crates/apb-server/dashboard/`.
6. Size: 82 changed files with the archived change (`design.md` "Files").
