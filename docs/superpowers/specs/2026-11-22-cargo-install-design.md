# cargo install

The public record of the change `cargo-install`: why apb is published to
crates.io and how, D1 to D8 in short.

## Why

A machine with a Rust toolchain and the usual build tools (a C compiler and
linker) installs apb with `cargo install --locked agentic-playbooks`; the
binary stays `apb`. Four things stood in the way: the crates.io names `apb`,
`apb-core` and `apb-cli` belong to unrelated projects; the release pipeline
derives its file names from the CLI package name; the packages were not
self-contained (no `LICENSE`, embedded files outside the crates, the
dashboard built into `web/dist`); and nothing published or noticed when the
workspace stopped being publishable.

## D1. The CLI package keeps its name in the repository

Renaming the CLI package renames the shell installer, the archives and the
updaters (dist derives them from the package name; only the Homebrew formula
can be pinned back), which would break `apb self-update` of every existing
install and the documented installer URL. A second package with a bin
target `apb` collides with the first in every workspace build, and a generic
dist package replaces dist's cargo builds. So the repository keeps the
package `apb`, and `scripts/crates.sh` renames it to `agentic-playbooks`
right before packaging: exactly the `[package]` name line of
`crates/apb-cli/Cargo.toml` and the one `name = "apb"` line of `Cargo.lock`,
checked by `cargo metadata --locked` (no resolver run, so it also works on a
cold registry cache), and both files restored on every exit. The script
refuses to start when either file has uncommitted changes. `dist plan` is
unchanged.

## D2. Library packages renamed, import names kept

The six libraries are the packages `agentic-playbooks-core`, `-engine`,
`-server`, `-mcp`, `-mcp-client` and `-decide`, each with `[lib] name`
`apb_core` and so on. Every normal internal dependency goes through one
`[workspace.dependencies]` entry that keeps the old key, names the package
and requires the exact workspace version (`=<version>`): the crates are
released together and promise nothing to each other, and a caret
requirement would let a later library join an older CLI under `cargo
install` without `--locked`. Dev-dependencies stay path-only, so no
published manifest depends on a `testing` feature. No Rust import changes.
`[workspace.package]` gains `homepage` and `rust-version = "1.99"`, the
lowest Rust the workspace builds with, checked by the CI job `msrv` on every
pull request rather than assumed. Two dependencies nothing used,
`tower-http` in apb-server and `imap-proto` in apb-engine, leave the
manifests.

## D3. The dashboard lives in `crates/apb-server/dashboard/`

Vite writes the built dashboard straight into the server crate, the one
path the workspace build, CI, the release build and the published package
read. `crates/apb-server/build.rs` stops a build without
`dashboard/index.html` with a message that names the build command, so a
checkout without the build never compiles to an empty dashboard.

## D4. Every crate is self-contained

Every crate has a committed `LICENSE` link to the root file. The shipped
playbooks, the official connectors, the template gallery, the models table
and the cloud catalog are reached through committed links under
`crates/apb-core/embedded/`, and the documents and the standing block the
MCP server serves through links under `crates/apb-mcp/embedded/`;
`cargo package` packs a link as the file it points to, so the workspace
build and the package read the same bytes. Each manifest has an `include`
list (no tests ship). The libraries say in their description and in the
first paragraph of their docs that they are internal and not a public API,
and carry no keywords or categories. `crates/apb-cli/README.md` is the
crate page.

## D5. The publishability gate

`scripts/crates.sh check` applies the rename, runs `cargo package
--workspace --locked --allow-dirty` (which verifies every crate by building
it from its packed sources against the other packed crates through cargo's
temporary local registry, so unpublished interdependent crates verify),
checks every file list (license, dashboard, embedded content, README), the
10 MB size limit, and writes `SHA256SUMS`. The CI job `crates` runs it on
every pull request, and the aggregate check `build-and-test` compares its
result explicitly like every job it needs. The job `msrv` reads
`rust-version` from `Cargo.toml` (the workflow repeats no version), builds
the workspace with exactly that toolchain over `rust-toolchain.toml`'s
`stable` (`cargo check --workspace --locked`), and is required the same way;
the packaging check and the publication keep the stable toolchain. The script refuses a registry
token everywhere but `publish`, refuses a checked-in cargo configuration,
and never hands cargo an empty package selection.

## D6. Publication

`.github/workflows/publish-crates.yml` follows every completed Release run
like `release-notes.yml`. Before any checkout or credential, its jobs check
that the tag names the commit the run built, that the commit is an ancestor
of main, and that the GitHub release exists and is not a draft. Code from the
tagged checkout never runs in a job that holds a credential or can request
one: `prepare` (no credential) builds the dashboard, runs `check` and
`preflight` (ownership of existing names, the index checksums, the plan) and
hands the verified packages over; `publish` (the only job with
the `crates-io` environment and its token; no job has `id-token: write`)
checks the artifact with
an inline step, runs only `crates.sh repack` (no build, the recorded
checksums required) and `crates.sh publish` from its own checkout, and
requires the index checksums to equal the recorded ones; `installed` (no
credential) installs the published version from crates.io. No job uses an
actions cache. A rerun skips crates already published with the verified
bytes. Every release publishes with the environment's token, and the
publish step fails closed when it is absent or empty: crates.io does not
accept trusted publishing from a `workflow_run` workflow, so trusted
publishing needs a different trigger and is a later change.

## D7. Self-update on a cargo install

The kind of install is a property of the binary: a binary compiled from the
published package `agentic-playbooks` (`CARGO_PKG_NAME`) answers `apb
self-update` with the cargo command and exit 2, changes nothing and never
follows an installer receipt; a binary compiled as `apb` behaves as before,
whatever cargo record sits next to it. No file on disk decides it.

## D8. Docs

`docs/INSTALL.md` has a cargo section with the build tools (the Xcode
Command Line Tools on macOS, `build-essential` on Debian or Ubuntu) and
"Rust 1.99 or newer", `llms.txt` names the command next to the warning
about the unrelated `apb` crate, and the contributor docs use the new
package names. The minimum Rust version is not part of the 1.x promise and
may rise in a minor release.
