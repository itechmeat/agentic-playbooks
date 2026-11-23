# Design: cargo install

Read `proposal.md` first. Every site below is named by its file and its
function, constant or attribute; find it by that name on the main the
implementer starts from. A site that no longer exists is a stop: report
it, do not invent one.

This change applies on top of one-point-oh-freeze, which bumps the
workspace to `1.0.0-rc.1` (the workspace `version` of `Cargo.toml`, the
path dependency versions in `crates/apb-cli/Cargo.toml` and
`crates/apb-server/Cargo.toml`, `Cargo.lock`), adds tests to
`crates/apb-cli/src/selfupdate.rs`, creates `docs/COMPATIBILITY.md` and
writes `docs/release-notes/v1.0.0-rc.1.md`. The branch of this change was
written before the freeze merged (its tree still says `1.0.0-beta.7` and
holds neither document); the orchestrator rebases it onto main after the
freeze merges and before the implementer starts (tasks.md, "Before
anything"). Every version string below is written as `<V>`: the workspace
version on the main this change starts from (`1.0.0-rc.1` after the
freeze). If the freeze is not in the tree, stop and report it.

Decision groups, built in the order D1 to D8 (tasks.md). Nothing here is
left to the implementer.

## Evidence

Every fact this design rests on was checked with the tools named, never
assumed: items 1 to 3 and 5 to 11 on main 121e5c060, items 4 and 12 to
16 on this change's branch on 2026-11-22, after the first design review,
and item 17 after the second.
The probes ran in throwaway local clones, throwaway crates and a
throwaway container under `~/.cache`, never in the worktree; they read
the public crates.io index and API and the GitHub API, and wrote nothing
to any registry or repository.

1. `dist plan --output-format=json` on main: one app `apb`, artifacts
   `apb-installer.sh`, `apb.rb`, `apb-<target>.tar.xz` with `.sha256`,
   `apb-<target>-update` for the four targets, `source.tar.gz`,
   `sha256.sum`; install hints
   `.../releases/download/v1.0.0-beta.7/apb-installer.sh | sh` and
   `brew install itechmeat/agentic-playbooks/apb`.
2. The same with only the CLI package renamed to `agentic-playbooks`:
   app `agentic-playbooks`, artifacts `agentic-playbooks-installer.sh`,
   `agentic-playbooks.rb`, `agentic-playbooks-<target>.tar.xz`,
   `agentic-playbooks-<target>-update`. Adding `formula = "apb"` to the
   package's `[package.metadata.dist]` brings back `apb.rb` and nothing
   else; adding `display-name = "apb"` changes only the display name. The
   list of dist configuration keys in the dist 0.32.0 binary holds no key
   for the app, installer or archive name.
3. The same with the six library packages renamed
   (`agentic-playbooks-core` and so on, `[lib] name` kept, dependents
   through `package = ...`) and the CLI package left as `apb`: the artifact
   list and every install hint are identical to item 1; `cargo metadata`
   succeeds; `Cargo.lock` holds the same 456 packages with the same
   versions, sources and checksums, only reordered by the new names.
4. In a clone of the change's branch, with `CARGO_HOME` pointing at an
   empty directory (no registry cache at all): `cargo update --workspace
   --offline` after the CLI rename fails ("offline mode ... surprising
   resolution failures", a dependency of `apb-core` not found), so an
   offline update depends on a warm cache. Editing instead exactly two
   lines in place (the `[package]` `name` of `crates/apb-cli/Cargo.toml`
   and the one line `name = "apb"` of `Cargo.lock`, its `[[package]]`
   entry; no other entry names `apb`, since nothing depends on the CLI)
   and running `cargo metadata --locked --format-version 1` passes on that
   cold cache in 11 s (it downloads what it needs) and leaves `Cargo.lock`
   byte for byte as edited, although the renamed entry now sits out of
   alphabetical order. Negative control: the same `Cargo.lock` edit with
   the manifest left as `apb` fails `--locked` ("cannot update the lock
   file ... because --locked was passed"). The bin target stays `apb`.
5. `cargo package --list -p <crate> --allow-dirty` for each of the seven
   crates on main: no `LICENSE` in any list; no dashboard, bundled,
   connector, template, model or cloud catalog file in any list.
6. Source: the production embeds that leave their crate are
   `apb-server` `assets.rs` `#[folder = "../../web/dist"]`; `apb-core`
   `templates.rs` `#[folder = "../../examples/playbooks"]`,
   `connector/official.rs` `#[folder = "../../connectors"]`,
   `bundled/mod.rs` `#[folder = "../../bundled"]`, `models_table.rs`
   `BUILTIN_YAML` (`assets/models.yaml` at the root),
   `cloud_models/near_ai.rs` the embedded catalog
   (`assets/cloud-models/near-ai-cloud.json`); `apb-mcp`
   `tools/playbook.rs` (`docs/HOWTO-authoring.md`,
   `docs/HOWTO-interview.md`), `setup/mod.rs` `HOWTO`
   (`docs/AGENT-SETUP.md`), `setup/standing.rs` `PLAYBOOK_BLOCK`
   (`crates/apb-cli/assets/playbook-instructions.md`). The other
   out-of-crate reads (`README.md`, `llms.txt`, `.apb/playbooks/...`,
   `docs/MCP.md`, test fixtures) sit in `#[cfg(test)]` code, which a
   package build does not compile.
7. A throwaway crate in a git repository with `include = [...]`: a
   committed symbolic link to a directory and one to a file outside the
   crate are listed and packed as plain files with their content (the
   `.crate` holds regular files); a git-ignored directory named in
   `include` is packed; without `--allow-dirty`, `cargo package` refuses
   because that ignored file counts as uncommitted.
8. A throwaway two-crate workspace (a library renamed with `[lib] name`
   kept, a binary depending on it through a `[workspace.dependencies]`
   entry with `package`, `path` and `version = "=<v>"`, both unpublished):
   `cargo package --workspace --offline` packs both and verifies the
   binary against the packed library through
   `target/package/tmp-registry`; the published manifest carries
   `version = "=<v>"` and `package = ...`; a path-only dev-dependency
   (also the self dev-dependency with `features = ["testing"]`) is
   removed from the published manifest.
9. rust-embed 8.14.0 (the version in `Cargo.lock`): the derive fails the
   build with "folder ... does not exist" when the folder is missing
   (unless `allow_missing`), and walks the folder with
   `follow_links(true)`.
10. crates.io publishes with an API token (`CARGO_REGISTRY_TOKEN`);
    scopes such as `publish-new` and `publish-update` and a crate name
    pattern limit what a token may do. Trusted publishing is not used:
    crates.io does not accept it from a `workflow_run` workflow (decided
    by the orchestrator, Decision 2). The Cargo book:
    "crates.io currently has a 10MB size limit on the `.crate` file";
    `cargo package`: "Path dependencies are not allowed unless they have
    a version key", dev-dependencies excepted.
11. `crates/apb-cli/src/selfupdate.rs`: an installer install is told
    apart by its install receipt, `AxoUpdater::new_for("apb")` and
    `load_receipt`; `AxoupdateError::NoReceipt` answers
    `NO_RECEIPT_GUIDANCE` with exit 2. The receipt is found through the
    config root, independent of the binary it describes. `cargo install`
    records what it installed in `.crates2.json` in the install root,
    keyed `<package> <version> (<source>)`, also without any identity of
    the binary file: on the machine this was written on, that file holds a
    stale `apb 0.25.0 (path+file://...)` record next to a valid installer
    receipt for the same `~/.cargo/bin/apb`. Neither record proves which
    installer wrote the file that is running.
12. `crates/apb-cli/src/main.rs` `Cli`: `#[command(name = "apb")]`, so the
    help text does not follow the package name; nothing reads
    `CARGO_PKG_NAME` today. Probe (a throwaway workspace, a bin crate
    printing `env!("CARGO_PKG_NAME")`): built as package
    `zzq-apbprobe-bin-7k2` it prints that name; after only its
    `[package] name` line was changed it prints the new name. Cargo sets
    `CARGO_PKG_NAME` for every compilation from the manifest it builds, so
    the binary that `cargo install agentic-playbooks` compiles from the
    published package carries `agentic-playbooks`, and every build from
    the repository (dist's legs, `cargo build`, `cargo install --path`)
    carries `apb`.
13. Package bytes (the throwaway two-crate workspace, a renamed library
    and a bin depending on it with `version = "=0.1.0"`, neither
    published; and `apb-decide` of the branch): `cargo package
    --no-verify` gives the same SHA-256 before and after touching every
    source file, in a second fresh clone, and with verification; `cargo
    publish --workspace --dry-run --no-verify` (cargo 1.99.1, which has
    `--workspace` and `--exclude` on `publish`) packs the two crates into
    `target/package/tmp-crate/` with exactly the SHA-256 of `cargo package
    --workspace --no-verify`. Packaging is reproducible, and the publish
    command packs what the package command packs.
14. crates.io sparse index and API: the index line of `serde` 1.0.200 has
    `"cksum":"ddc6f9cc..."`, the SHA-256 of
    `static.crates.io/crates/serde/serde-1.0.200.crate`, so the index
    checksum of a version is the SHA-256 of its `.crate` file.
    `GET https://crates.io/api/v1/crates/<name>/owners` (unauthenticated,
    with a `User-Agent`) lists the owners as `login` and `kind` (`user` or
    `team`); `GET /api/v1/crates/serde-json` answers the crate
    `serde_json`, so the API resolves a name that differs only by `-` and
    `_`, while the sparse index file is per spelling. On 2026-11-22 the
    seven `agentic-playbooks*` index files were 404 and
    `/api/v1/crates/agentic-playbooks/owners` was 404.
15. GitHub compare API on this repository:
    `repos/<repo>/compare/main...<sha>` answers `.status` `behind` for an
    older commit of main, `identical` for the head of main, `diverged`
    for an unmerged branch commit, and a 404 (non-zero exit of `gh api`)
    for an unknown commit. A commit is an ancestor of main exactly when
    the answer is `behind` or `identical`.
16. C toolchain: the workspace builds `libsqlite3-sys` with `bundled`
    (C sources through `cc`), `ring` 0.17.14 and `aws-lc-sys` 0.45.0
    (through `rustls` and `aws-lc-rs`, a normal dependency of
    `apb-engine`); `aws-lc-sys` lists `cc`, `cmake` and `pkg-config` as
    build dependencies, and on Linux `rustc` links through `cc`, so not
    even a pure Rust program installs without a C linker. Probe: a crate
    depending on those exact versions builds and runs (`sqlite 3.54.1`,
    both digests) in `ubuntu:26.04` with only `build-essential`,
    `ca-certificates` and `curl` installed from apt and rustup's minimal
    stable toolchain (rustc 1.100.0); `cmake`, `clang` and `pkg-config`
    were absent.
17. An empty package selection (cargo 1.99.1, a throwaway workspace
    with one member, after the second design review): `cargo package
    --workspace --no-verify --allow-dirty --exclude <the member>` and
    `cargo publish --workspace --no-verify --allow-dirty --dry-run
    --exclude <the member>` both exit 101 with `The manifest is virtual,
    and the workspace has no members`. A command that excludes every
    member fails instead of doing nothing (D5).
18. The minimum Rust version (after the implementation, on the final
    tree): `RUSTUP_TOOLCHAIN=1.99.1 cargo check --workspace
    --all-targets --locked` finishes with exit 0, as does the same
    command on 1.100.0; the code uses nothing that needs 1.100. Cargo
    1.100 warned `cargo::unused_dependencies` for `tower-http` in
    `apb-server` and `imap-proto` in `apb-engine`. Neither is named
    anywhere in its crate's code, tests or features (`rg` for
    `tower_http` and `imap_proto`): the IMAP decoder reads the envelope
    addresses through a macro over the `imap` crate's own pinned
    `imap-proto` 0.10, never the direct 0.17 dependency. Removing both
    drops `tower-http` and `imap-proto` 0.17 from `Cargo.lock` and
    nothing else, and 1.100 then warns about neither.

## D1. The CLI package keeps its name in the repository

Options compared against R1 (nothing that exists changes its name):

1. (a) Rename the CLI package to `agentic-playbooks` and pin the dist
   names. Rejected: evidence 2. Only the formula can be pinned; the
   installer, the archives, the updaters and the install receipt's app
   name follow the package name. `.../latest/download/apb-installer.sh`
   would 404 from the first release on, and a beta install's
   `apb self-update` looks for `apb-installer.sh` in the new release.
2. (b) Keep the dist package `apb` and add a second, publishable package
   `agentic-playbooks` with a bin target `apb`. Rejected: two bin targets
   named `apb` in one workspace collide in `target/<profile>/apb` on every
   `cargo build --workspace` and `cargo test --workspace` (cargo warns
   "output filename collision" and says it may become a hard error), and
   the new package cannot hold the CLI's sources, which live outside its
   directory, without duplicating them. Placing it outside the workspace
   gives it its own `Cargo.lock`, so `cargo install --locked` would no
   longer build what the release was tested with.
3. (c) A generic dist package (`dist.toml` naming the app `apb` with its
   own build command). Rejected: it replaces dist's cargo builds of all
   four targets (the musl leg, `profile.dist`) with a hand-written build,
   duplicates the version outside `Cargo.toml`, and would have to prove
   every release artifact again one release before 1.0.
4. (d) Chosen: the repository keeps the CLI package `apb` (dist derives
   exactly today's names, evidence 3), and the publication step renames
   it to `agentic-playbooks` in its own throwaway checkout right before
   packaging (evidence 4). The gate of D5 runs the same rename on every
   pull request, and the job `prepare` of D6 runs the same check again on the
   tagged commit, so what is published is what was packaged and verified.

The rename is done by `scripts/crates.sh` (D5) and nowhere else:

1. It refuses to start unless `crates/apb-cli/Cargo.toml` and
   `Cargo.lock` are unmodified in git, and restores both with
   `git checkout --` on every exit (a shell `trap`), so a local run leaves
   the tree as it found it.
2. It replaces the line `name = "apb"` of the `[package]` table of
   `crates/apb-cli/Cargo.toml` with `name = "agentic-playbooks"` and
   fails unless exactly one line changed and the `[[bin]]` table still
   reads `name = "apb"`.
3. It edits `Cargo.lock` in place, without cargo: the one line
   `name = "apb"` that opens a `[[package]]` entry (the line before it is
   `[[package]]`) becomes `name = "agentic-playbooks"`; it fails unless
   exactly one such line existed and exactly one line changed. No
   `cargo update`, online or offline, runs (evidence 4: offline needs a
   warm registry cache, and a CI cache is only a restore chance).
4. It runs `cargo metadata --locked --format-version 1` and fails when
   cargo would change the lockfile, which proves the edited lockfile
   matches the renamed workspace exactly (evidence 4, also on an empty
   registry cache).

What a user sees: the crate `agentic-playbooks` installs the binary `apb`;
`apb --version` prints `apb <V>` (clap's name is fixed, evidence 12).
`cargo install --path crates/apb-cli` from a checkout installs the package
`apb` as today.

## D2. Library packages: renamed, import names kept

The six library packages are renamed in the repository:

| Directory | Package | Library name |
|---|---|---|
| `crates/apb-core` | `agentic-playbooks-core` | `apb_core` |
| `crates/apb-engine` | `agentic-playbooks-engine` | `apb_engine` |
| `crates/apb-server` | `agentic-playbooks-server` | `apb_server` |
| `crates/apb-mcp` | `agentic-playbooks-mcp` | `apb_mcp` |
| `crates/apb-mcp-client` | `agentic-playbooks-mcp-client` | `apb_mcp_client` |
| `crates/apb-decide` | `agentic-playbooks-decide` | `apb_decide` |

Directories do not move. Both mechanisms of R2 are needed, each for its
own reason:

1. `[lib] name = "apb_core"` (and so on) in each library's manifest: the
   package's own integration tests, doctests and unit tests name the
   crate by its library name (`use apb_core::...` in
   `crates/apb-core/tests/`), which otherwise becomes
   `agentic_playbooks_core`.
2. A dependency key that differs from the package name, `apb-core = {
   package = "agentic-playbooks-core", ... }`: a path dependency is
   matched by package name, so the key alone no longer finds the crate;
   with `package`, the dependent's extern name stays the key
   (`apb_core`), so no `use` line in any crate changes.

One place holds the normal dependencies: the root `[workspace.dependencies]`
gains, below a comment "Internal crates: published under these package
names; the version is exact because they are released together and are
not a public API":

```toml
apb-core = { package = "agentic-playbooks-core", path = "crates/apb-core", version = "=<V>" }
apb-decide = { package = "agentic-playbooks-decide", path = "crates/apb-decide", version = "=<V>" }
apb-engine = { package = "agentic-playbooks-engine", path = "crates/apb-engine", version = "=<V>" }
apb-mcp = { package = "agentic-playbooks-mcp", path = "crates/apb-mcp", version = "=<V>" }
apb-mcp-client = { package = "agentic-playbooks-mcp-client", path = "crates/apb-mcp-client", version = "=<V>" }
apb-server = { package = "agentic-playbooks-server", path = "crates/apb-server", version = "=<V>" }
```

Every `[dependencies]` entry on one of them in a member manifest becomes
`apb-core.workspace = true` (with `features = [...]` where it has some
today). The exact `=` requirement matters for `cargo install` without
`--locked`: a caret requirement would let a later library patch release
join an older CLI, and the libraries promise nothing to each other.

Every `[dev-dependencies]` entry on an internal crate stays path-only, with
`package` added, for example
`apb-core = { package = "agentic-playbooks-core", path = "../apb-core", features = ["testing"] }`
and the self entries `path = "."`. Path-only dev-dependencies are removed
from the published manifest (evidence 8), so no published crate depends on
a `testing` feature or on itself.

The CLI's dependency entries use the same workspace entries; the CLI has
no library and no other package depends on it.

Two dependencies nothing uses leave the manifests, so no published crate
declares them (Evidence item 18): `tower-http` from `apb-server`, and
`imap-proto` from `apb-engine` together with its `[workspace.dependencies]`
entry and the comment that claimed the IMAP decoder named its types.

`[workspace.package]` gains `homepage =
"https://github.com/itechmeat/agentic-playbooks"` and `rust-version =
"1.99"`, so a `cargo install` on an older toolchain fails with cargo's own
message instead of a compile error deep in a dependency. The minimum is
the lowest Rust the workspace builds with, not the stable Rust CI builds
with: a minimum that follows stable shuts out every toolchain older than
the latest release for no reason in the code. It is checked, not assumed:
the CI job `msrv` (D5) builds the workspace with exactly this version on
every pull request (Evidence item 18 records the first check). Every member manifest
takes `homepage.workspace = true` and `rust-version.workspace = true`.

Developer commands change with the package names:
`cargo test -p agentic-playbooks-core --test main <module>` and so on;
`-p apb` stays. Every live file that names `-p apb-<library>` is
updated (Files).

## D3. The dashboard lives in `crates/apb-server/dashboard/`

One path for every build: vite writes the built dashboard straight into
`crates/apb-server/dashboard/`, and `apb-server` embeds that directory.
The workspace build, CI, the release build (dist's build legs run the
same `bun run build` from `.github/build-setup.yml`, which does not
change) and the published package all read it. Rejected alternatives: a
copy step from `web/dist` before packaging (a second path that can be
stale, and every build would need the copy); a committed build (0.9 MB of
generated files in every diff and a stale dashboard whenever someone
forgets to rebuild).

1. `web/vite.config.ts`: `build.outDir: '../crates/apb-server/dashboard'`
   and `build.emptyOutDir: true` (vite empties an out directory outside
   its root only when told to). A comment says the server embeds it.
2. `.gitignore`: the line `web/dist` is replaced by
   `crates/apb-server/dashboard/`. `web/.gitignore` keeps vite's own
   `dist` line (harmless).
3. `crates/apb-server/src/assets.rs` `WebAssets`:
   `#[folder = "dashboard"]` (relative to the crate, rust-embed's rule).
   The module comment names the directory and the build command.
4. `crates/apb-server/build.rs` (new): prints
   `cargo:rerun-if-changed=dashboard` and, when
   `dashboard/index.html` is not a file, prints to stderr
   `the dashboard is not built: run (cd web && bun install --frozen-lockfile && bun run build) from the repository root; it writes crates/apb-server/dashboard`
   and exits 1. This runs before rust-embed's own "folder does not exist"
   error (evidence 9) and also catches an existing but empty directory, so
   a checkout without a build never compiles to an empty dashboard. The
   `unbuilt` build id of `build_id` stays for the test builds that never
   reach it.
5. `crates/apb-server/Cargo.toml` `include` (D4) names `build.rs` and
   `dashboard/**`, so the published package holds the built files
   (evidence 7: an ignored directory named in `include` is packed).

Every place that names `web/dist` today, and what happens to it:

| Place | Change |
|---|---|
| `crates/apb-server/src/assets.rs` | `#[folder]` and comment (above) |
| `.gitignore` | the new path |
| `.github/workflows/ci.yml` | the four comments "apb-server embeds web/dist through rust-embed" name the new directory |
| `.github/workflows/test-gate.yml` | the comment before "Build web (needed by rust-embed)" |
| `.github/build-setup.yml` | none: it does not name the path, and changing it would regenerate `release.yml` |
| `crates/apb-core/src/connector/official.rs` | the comment that compares with the `web/dist` embed |
| `scripts/daily-cleanup.sh` | `regenerable_paths` gains `crates/apb-server/dashboard` (keeps `web/dist` for older worktrees) |
| `scripts/old-apb.sh` | none: it builds older tags, whose own vite config decides the path |
| `scripts/third-party-notices.sh` | the comment naming what the Vite build emits |
| `web/scripts/third-party-notices.ts` | its three comments |
| `docs/INSTALL.md` | section "Build from source" (D8) |
| `docs/BUILD-OPTIMIZATION.md` | the sentence on why CI is not split |
| `llms.txt` | the source build paragraph (D8) |
| `CLAUDE.md`, `AGENTS.md` | the apb-server bullet ("embedded via rust-embed (`assets.rs`, ...)") and the Commands section |
| `README.md` | out of scope (another branch) |
| `.apb/playbooks/apb-task-implement/*/playbook.yaml` | none: immutable versions |
| `docs/release-notes/v0.9.0.md`, archived changes, `docs/superpowers/` | none: history |

## D4. Every crate is self-contained

1. `LICENSE`: each of the seven crate directories gets a committed
   symbolic link `LICENSE -> ../../LICENSE`; `cargo package` packs it as a
   file (evidence 7). `license = "Apache-2.0"` stays (no `license-file`).
2. `crates/apb-core/embedded/` holds five committed symbolic links:
   `bundled -> ../../../bundled`, `connectors -> ../../../connectors`,
   `templates -> ../../../examples/playbooks`,
   `models.yaml -> ../../../assets/models.yaml`,
   `near-ai-cloud.json -> ../../../assets/cloud-models/near-ai-cloud.json`.
   The embeds read through them: `bundled/mod.rs`
   `#[folder = "embedded/bundled"]`, `connector/official.rs`
   `#[folder = "embedded/connectors"]`, `templates.rs`
   `#[folder = "embedded/templates"]`, `models_table.rs`
   `include_str!("../embedded/models.yaml")`, `cloud_models/near_ai.rs`
   `include_str!("../../embedded/near-ai-cloud.json")`. rust-embed
   follows the links (evidence 9). The source of truth stays where it is:
   tests, docs and the bundled scenario suite keep reading `bundled/`,
   `connectors/` and `examples/playbooks/` at the root.
3. `crates/apb-mcp/embedded/` holds four links:
   `HOWTO-authoring.md -> ../../../docs/HOWTO-authoring.md`,
   `HOWTO-interview.md -> ../../../docs/HOWTO-interview.md`,
   `AGENT-SETUP.md -> ../../../docs/AGENT-SETUP.md`,
   `playbook-instructions.md -> ../../apb-cli/assets/playbook-instructions.md`.
   `tools/playbook.rs`, `setup/mod.rs` `HOWTO` and `setup/standing.rs`
   `PLAYBOOK_BLOCK` read `../../embedded/<file>`.
4. `include` per manifest (tests and fixtures are not shipped; a package
   is for building):
   1. `agentic-playbooks-core`: `["src/**", "assets/**", "embedded/**", "LICENSE"]`
      (`assets/demo/` is the crate's own).
   2. `agentic-playbooks-mcp`: `["src/**", "embedded/**", "LICENSE"]`.
   3. `agentic-playbooks-server`: `["src/**", "build.rs", "dashboard/**", "LICENSE"]`.
   4. `apb` (published as `agentic-playbooks`): `["src/**", "assets/**", "README.md", "LICENSE"]`.
   5. `agentic-playbooks-engine`, `-mcp-client`, `-decide`:
      `["src/**", "LICENSE"]`.
   `Cargo.toml` is always packed. The gate's `cargo package` verify build
   proves that each list is complete.
5. Why links and not moves: moving `bundled/`, `connectors/` and
   `examples/playbooks/` into `crates/apb-core/` renames 166 tracked files
   and every test, doc and script path that names them, which alone
   exceeds the pull request's size limit; a copy step at packaging time
   gives the package other bytes than the workspace build compiled. The
   links make both read the same files. Windows is not a supported
   platform (`docs/COMPATIBILITY.md`), so git checkouts keep the links.

Metadata (R9: the libraries say they are not an API; they carry no
keywords or categories, so crates.io search does not offer them):

1. `apb` (published as `agentic-playbooks`): description
   `Local runner for agentic playbooks: the apb command, with a web dashboard and an MCP server for coding agents`;
   keywords `["agents", "playbooks", "mcp", "automation", "workflow"]`;
   categories `["command-line-utilities", "development-tools"]`;
   `readme = "README.md"`.
2. `agentic-playbooks-core`: `Internal crate of the apb command (crate agentic-playbooks): playbook schema, validator and stores. Not a public API; it changes in any release.`
3. `agentic-playbooks-engine`: `Internal crate of the apb command (crate agentic-playbooks): the run engine. Not a public API; it changes in any release.`
4. `agentic-playbooks-server`: `Internal crate of the apb command (crate agentic-playbooks): the dashboard web API with the built dashboard. Not a public API; it changes in any release.`
5. `agentic-playbooks-mcp`: `Internal crate of the apb command (crate agentic-playbooks): the MCP server and its tools. Not a public API; it changes in any release.`
6. `agentic-playbooks-mcp-client`: `Internal crate of the apb command (crate agentic-playbooks): the MCP client of connector calls. Not a public API; it changes in any release.`
7. `agentic-playbooks-decide`: `Internal crate of the apb command (crate agentic-playbooks): the decision model client. Not a public API; it changes in any release.`
   (replaces today's description).

Each library's `src/lib.rs` begins with this inner doc paragraph, before
any existing comment or doc text (an existing `//!` block follows it after
an empty `//!` line):

```rust
//! Internal crate of the `apb` command, published only so that
//! `cargo install agentic-playbooks` can build it. It is not a public API:
//! any item may change or disappear in any release, patch releases
//! included. Do not depend on it; use the `apb` command or its MCP server.
```

`crates/apb-cli/README.md` (new, the crate page on crates.io): what apb
is in two sentences, `cargo install --locked agentic-playbooks` with the
`--locked` sentence of D8, that the binary is `apb`, the other install
ways with a link to `docs/INSTALL.md` on GitHub, `apb self-update` not
used for cargo installs, and the link to `docs/COMPATIBILITY.md`. No
relative links (they break on crates.io).

## D5. The publishability gate and `scripts/crates.sh`

`scripts/crates.sh` (new, bash, `set -euo pipefail`) has five commands:
`check` (the pull request gate, and in the job `prepare` of D6),
`preflight` (the job `prepare`), `repack` and `publish` (the job
`publish`) and `installed` (the job `installed`). The script runs with a
credential, or in a job that can request one, only as `repack` and
`publish`; D6 says why that is the one file of the tagged checkout such a
job executes. Before anything else each command checks its credential
rule:

1. Credentials: `check`, `preflight`, `repack` and `installed` refuse to
   start when `CARGO_REGISTRY_TOKEN` or any variable named
   `CARGO_REGISTRIES_*_TOKEN` is set and not empty, with
   `refusing to run <command> with a registry token in the environment`;
   `publish` refuses to start without `CARGO_REGISTRY_TOKEN`.
2. The plan (`repack` and `publish` only): the second check reads the
   plan of `preflight` (below) and, when every line says `skip`, takes
   the all-skipped path of the command before any preparation, so an
   all-skipped plan runs no `cargo` command at all.

Then they share their preparation:

1. Checked-in cargo configuration: every command refuses when the
   checkout holds `.cargo/config` or `.cargo/config.toml` at the
   repository root or in a crate directory (none exists today), so no
   committed file can add a credential provider, an alias or a build
   wrapper. In the job `publish` an inline step of the workflow makes the
   same check before the script runs (D6), so that job does not rely on
   the script for it.
2. The D1 rename with its checks (the exact edits, `cargo metadata
   --locked`), and the restore trap.
3. A `cargo metadata --locked --format-version 1` read (with `python3`,
   present on every runner and on the machines that run this) that fails
   when a workspace member has a normal or build dependency on another
   member whose requirement is not `=<workspace version>`, or any
   dependency with a `git` source.
4. `crates/apb-server/dashboard/index.html` is present.

No command hands cargo an empty package selection (a selection that
excludes every member fails with exit 101, evidence 17): `check` always packs all seven crates;
`repack` and `publish` reach a selecting cargo command only with at
least one `publish` line in the plan and pass one `--exclude` per `skip`
line, and each asserts right before the command that it passes fewer
than seven `--exclude` arguments (`internal error: empty package
selection` otherwise); `preflight` runs no packaging command and
`installed` names one package.

The commands:

1. `check`: `cargo package --workspace --locked --allow-dirty`.
   `--workspace` packs the seven crates and verifies each one by building
   it from its packed sources against the other packed crates through
   cargo's temporary local registry (evidence 8), so interdependent crates
   that are not on crates.io yet verify; `--allow-dirty` is needed because
   the built dashboard is an untracked file (evidence 7) and the CLI
   manifest carries the rename. Then, for each `<target dir>/package/<name>-<V>.crate`
   (the target directory as `cargo metadata` names it; seven names,
   failing on a missing one), `tar -tzf` checks:
   1. it holds `<name>-<V>/LICENSE`;
   2. `agentic-playbooks-server` holds `dashboard/index.html` and at least
      one file under `dashboard/assets/`;
   3. `agentic-playbooks-core` holds, under `embedded/`, every file
      `git ls-files bundled connectors examples/playbooks` names (mapped
      to its link) and the two asset files; `agentic-playbooks-mcp` its
      four files;
   4. `agentic-playbooks` holds `README.md`;
   5. its size is at most 10000000 bytes (the Cargo book's 10MB limit);
      the script prints every size.
   Last, it writes `<target dir>/package/SHA256SUMS`: one line
   `<sha256>  <name>-<V>.crate` per crate, in the publish order below.
   The packaging of `check` is what produces the checksums every later
   decision compares, so it always runs, also when every crate turns out
   to be published already.
2. `preflight <version> <sums file>` (no credential, no build, no
   packaging): fails unless `<version>` equals the workspace version and
   the sums file holds exactly the seven names at that version. Then, for
   each package in the fixed order
   `agentic-playbooks-decide agentic-playbooks-core agentic-playbooks-mcp-client agentic-playbooks-engine agentic-playbooks-mcp agentic-playbooks-server agentic-playbooks`
   (a dependency order, checked by a test against `cargo metadata`), with
   `curl -fsS` and a fixed `User-Agent: agentic-playbooks-publish`, any
   failure other than a 404 stopping the script:
   1. Ownership (evidence 14):
      `GET https://crates.io/api/v1/crates/<name>`. A 404 means the name
      is new to crates.io. A 200 counts as ours only when the answered
      `crate.name` equals `<name>` exactly and
      `GET /api/v1/crates/<name>/owners` lists an owner with `kind`
      `user` and `login` `itechmeat` (the constant `OWNER_LOGIN` at the
      top of the script, the GitHub account that owns the repository);
      otherwise it stops with `crate <name> exists on crates.io and is
      not ours (name <answered name>, owners <logins>)`. A new name that
      someone else takes between this check and the upload is refused by
      crates.io itself, which never lets a non-owner publish a version.
   2. The index (`https://index.crates.io/<prefix>/<name>`, prefix by the
      index rules: `ag/en/` for these names): a 404 means not published.
      A line with `"vers":"<version>"` is a skip only when its `cksum`
      equals the checksum of that crate in the sums file; any other
      `cksum` stops the script with
      `<name> <version> is already on crates.io with checksum <index cksum>, but the verified package has <verified checksum>; nothing was published`.
      Crate versions cannot be overwritten, so a retargeted tag or a
      partial publication from other sources never turns into a skip.
   3. It writes `publish-plan` next to the sums file: the line
      `<version>`, then one line `<name> publish` or `<name> skip` per
      package in the publish order. When `GITHUB_OUTPUT` is set it
      appends `nothing_to_publish=true` to that file when every line says
      `skip`, else `nothing_to_publish=false`. With nothing to publish it
      prints `all seven crates are on crates.io with the verified
      checksums`.
3. `repack <version> <dir>` (no credential, no build): `<dir>` holds
   `SHA256SUMS` and `publish-plan`. Fails unless the plan's first line is
   `<version>`, it has exactly seven further lines naming the packages in
   the publish order, each `publish` or `skip`, and the sums file holds
   exactly the seven names at that version.
   1. All skipped: it prints `nothing to repack: all seven crates are on
      crates.io with the verified checksums` and exits 0 without the
      shared preparation and without any cargo command.
   2. Otherwise, after the preparation: `cargo package --workspace
      --no-verify --locked --allow-dirty` with one `--exclude <name>` per
      `skip` line (`--no-verify` compiles nothing and runs no build
      script). Every package of a `publish` line must be packed with the
      checksum of its sums line; otherwise it stops with `<name>: the
      package built here has <checksum>, the verified one <checksum>;
      nothing was published`. Packaging is reproducible (evidence 13), so
      equal sources give equal bytes.
4. `publish <version> <dir>` (with the token): the same plan and sums
   checks as `repack`.
   1. All skipped: no preparation and no cargo command; it goes straight
      to the index check of item 4.4.
   2. For every `publish` line it reads the index again; a version now
      listed stops it with `<name> <version> appeared on crates.io after
      the plan was made; nothing was published; rerun all jobs`.
   3. After the preparation: `cargo publish --workspace --no-verify
      --locked --allow-dirty` with one `--exclude <name>` per `skip`
      line. `cargo publish --workspace` packs exactly what `cargo package
      --workspace` packs (evidence 13), uploads in dependency order and
      waits for each upload to appear in the index before the next. No
      build script runs: `--no-verify` builds nothing. On a failure of
      `cargo publish` it prints `publishing <version> stopped; rerun all
      jobs of the publish-crates run later: crates already published with
      the verified checksums are skipped` and exits 1. crates.io limits
      the rate of new crate publications; the first release publishes
      seven new names, so this message is the expected stop on that
      limit.
   4. For each of the seven names it reads the index (every 10 seconds,
      at most 5 minutes, until the version is listed) and fails unless
      the listed `cksum` equals the sums line, naming the crate and both
      checksums.
5. `installed <version>` (no credential): with `CARGO_HOME` and an
   install root in two new directories under `${RUNNER_TEMP:-$TMPDIR}`,
   removed on exit, runs `cargo install --locked agentic-playbooks
   --version <version> --root <root>` and fails unless
   `<root>/bin/apb --version` prints exactly `apb <version>`. It is the
   last job of D6 and the owner's check of `RELEASE-CHECKLIST.md`.

`crates/apb-cli/tests/suite/crate_manifests_test.rs` (new, declared in
`crates/apb-cli/tests/main.rs`) reads the workspace through
`cargo metadata --format-version 1 --offline --no-deps` (run with
`std::process::Command` on `env!("CARGO")`, parsed with `serde_json`) and
the files it names as plain text:

1. `every_internal_dependency_is_exact`: every normal dependency of a
   member on a member has requirement `=<workspace version>` and its
   `rename` equals the directory name of the target crate.
2. `library_names_stay`: the six library targets are named `apb_core`,
   `apb_engine`, `apb_server`, `apb_mcp`, `apb_mcp_client`, `apb_decide`,
   and their packages the names of D2.
3. `the_cli_package_stays_apb`: the package in `crates/apb-cli` is named
   `apb` with one bin target `apb` and no library (D1; the rename belongs
   to the publication step only).
4. `libraries_say_they_are_not_an_api`: each library description contains
   `Not a public API` and has no keywords and no categories; each
   `src/lib.rs` starts with the D4 paragraph.
5. `the_publish_order_is_a_dependency_order`: the order line of
   `scripts/crates.sh` lists all seven names, the CLI under
   `agentic-playbooks`, and no package comes before one it depends on.
6. `the_script_refuses_a_token_where_code_runs`: runs
   `scripts/crates.sh check`, `preflight 0.0.0 /dev/null`,
   `repack 0.0.0 /nonexistent` and `installed 0.0.0` with
   `CARGO_REGISTRY_TOKEN=x`, and again with only
   `CARGO_REGISTRIES_OTHER_TOKEN=x`, and `publish 0.0.0 /nonexistent`
   without a token: each exits non-zero with its refusal before any
   cargo or network call (the refusal is the first check), and the CLI
   manifest and the lockfile are unchanged afterwards.
7. `an_all_skipped_plan_runs_no_cargo_command`: in a temporary directory,
   a sums file of seven made-up checksums at the workspace version and a
   plan of seven `skip` lines; `PATH` starts with a directory holding a
   `cargo` stub that records its call and exits 1, and a `curl` stub that
   answers each index URL with a line `"vers":"<V>"` and that crate's
   checksum from the sums file. `repack <V> <dir>` exits 0 and prints its
   all-skipped line; `publish <V> <dir>` with `CARGO_REGISTRY_TOKEN=x`
   exits 0 after its index check; the `cargo` stub was never called. With
   one line changed to `publish`, `repack` calls the stub with one
   `--exclude` per remaining `skip` line (six) and fails on the stub's
   exit.

CI: `.github/workflows/ci.yml` gains a job `crates` (Linux, the runner,
the pinned bun and Rust setup steps and the cargo cache of the `lint`
job), steps: build the dashboard (`bun install --frozen-lockfile`,
`bun run build`), then `scripts/crates.sh check`. The job needs no
registry cache: the lockfile edit runs no resolver, and the verify build
downloads what it needs. The verify build compiles the workspace once
more in the dev profile; it runs beside the other jobs.

The aggregate job `build-and-test` ("Every Linux job passed") is the
check the main ruleset requires. It runs under `if: ${{ !cancelled() }}`,
so it also runs after a job it needs failed, and it fails only on what
its step compares. The change therefore makes three edits to it, and the
rule for any later job is the same: every job in `needs` has its own
environment variable and its own comparison.

1. `needs: [web, lint, test-linux, journal-corpus, crates, msrv]`.
2. The step's env gains `CRATES: ${{ needs.crates.result }}` and
   `MSRV: ${{ needs.msrv.result }}`.
3. The step's script becomes:
   ```sh
   echo "web=$WEB lint=$LINT test-linux=$TEST journal-corpus=$CORPUS crates=$CRATES msrv=$MSRV"
   [ "$WEB" = "success" ] && [ "$LINT" = "success" ] && [ "$TEST" = "success" ] \
     && [ "$CORPUS" = "success" ] && [ "$CRATES" = "success" ] \
     && [ "$MSRV" = "success" ]
   ```

The job `msrv` checks the minimum Rust version of D2. In the style of the
other jobs (pinned actions, `ubuntu-26.04`, the cargo cache saved on main
only), its first step reads `rust-version` from `[workspace.package]` of
`Cargo.toml` (the workflow repeats no version, so changing the minimum is
one line of `Cargo.toml`), fails when it finds none, hands it to the
toolchain install as a step output and exports it as `RUSTUP_TOOLCHAIN`
for every later step: `rust-toolchain.toml` names `stable`, which would
otherwise win over the installed toolchain, the cache key included. It
builds the dashboard (apb-server's build script stops without it), then
runs `rustc --version` and `cargo check --workspace --locked`. A
dependency update or a language feature that needs a newer Rust fails
here first; the fix is to raise `rust-version` in a minor release (D8
item 3) or to keep the older dependency. `scripts/crates.sh check`, the
publication's `preflight` and every other job keep the stable toolchain.

The ruleset needs no settings change. `release_workflow_test.rs` gains
`the_required_check_fails_unless_every_linux_job_succeeded`: it loads
`ci.yml`; `build-and-test` needs exactly `web`, `lint`, `test-linux`,
`journal-corpus`, `crates` and `msrv`, so a missing `msrv` dependency
fails it; its condition is `${{ !cancelled() }}`;
the env of its step maps one variable to `${{ needs.<job>.result }}` for
every job in `needs` and for nothing else. It then runs the step's `run`
script with bash: with every variable `success` it exits 0; with
`CRATES` set to `failure`, then `cancelled`, then `skipped`, and the
others `success`, it exits non-zero each time; and so for each of the
other five variables. `the_msrv_job_checks_the_rust_version_of_cargo_toml`
reads `rust-version` from `Cargo.toml`, checks that the `msrv` job text
repeats no such literal, runs the reading step with bash and compares the
step output and the exported `RUSTUP_TOOLCHAIN` with it, and checks that
the toolchain install takes `${{ steps.msrv.outputs.version }}` and that
the check is `cargo check --workspace --locked`.

## D6. Publication

A hand-written workflow, `.github/workflows/publish-crates.yml`, in the
shape of `release-notes.yml`, with three jobs on fresh runners:
`prepare`, `publish` and `installed`. A `workflow_run` workflow runs the
file of the default branch, so every `run` script and `if:` in it is
main's text, never the tag's.

The rule: code from the tagged checkout, and any file a job that ran
such code produced, never executes in a job that holds a credential or
can request one. The environment `crates-io` holds
`CARGO_REGISTRY_TOKEN`, and only the job that names that environment can
read it; no job has `id-token: write`, so no job can request an identity
token. Hence:

| Job | `permissions` | `environment` | Runs code of the tagged checkout |
|---|---|---|---|
| `prepare` | `contents: read` | none | yes: the dashboard build (bun), the verify build of `check` (build scripts), `crates.sh check` and `preflight` |
| `publish` | `contents: read` | `crates-io` | only `scripts/crates.sh repack` and `publish`, read from its own fresh checkout; cargo only with `--no-verify` |
| `installed` | `contents: read` | none | yes: `crates.sh installed`, and `cargo install` builds the published crates |

The top level has `permissions: {}`; no job has any other permission.
The one exception in `publish` is the script needed to publish (the
rename and the two cargo commands): it is read from `publish`'s own
checkout of `HEAD_SHA`, the reviewed file of a commit on main, and never
from the artifact, which comes out of `prepare`, where build code ran and
could have changed any file. Everything else `publish` checks is an
inline step of main's workflow file. Cargo with `--no-verify` compiles
nothing and runs no build script; its two other ways to run a program the
tree names are closed in `publish`: a checked-in cargo configuration
(refused by an inline step before any cargo command) and the toolchain
file `rust-toolchain.toml` (overridden by the job env
`RUSTUP_TOOLCHAIN: stable`, which rustup ranks above a toolchain file, so
a `path` toolchain named there is never used). Git in that job reads only
the configuration `actions/checkout` wrote.

No job restores or saves an actions cache: a cache entry a pull request
run wrote could put code into a build. Each job takes its toolchain from
its pinned setup action and its dependencies from their registries.

What the artifact can and cannot do: `prepare` could hand over other
bytes with matching checksums. `publish` then still publishes only the
seven names at the tag's version (the inline check below and crates.io's
owner rules), packed from its own checkout of the tagged commit plus the
handed-over dashboard files, which the tagged commit's build produced
anyway; it cannot hand any code a credential, publish another crate or
another version, or run code next to the credential.

1. `on: workflow_run: workflows: [Release], types: [completed]`;
   `permissions: {}`; `concurrency: { group: publish-crates,
   cancel-in-progress: false }`.
2. Every job: `runs-on: ubuntu-26.04`; job env `TAG` (the run's
   `head_branch`) and `HEAD_SHA`, and in `publish` also
   `RUSTUP_TOOLCHAIN: stable` (below).
   `GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}` is set on the check steps that
   call `gh` and on no other step. The conditions, where `<event>` is the
   notes job's condition (the run succeeded, its event is `push` and its
   head repository is this repository):
   1. `prepare`: `if: <event>`.
   2. `publish`: `if: <event> && needs.prepare.outputs.nothing_to_publish != 'true'`.
   3. `installed`: `if: !cancelled() && <event> && needs.prepare.result == 'success' && (needs.publish.result == 'success' || (needs.publish.result == 'skipped' && needs.prepare.outputs.nothing_to_publish == 'true'))`.
   `!cancelled()` lets `installed` run when `publish` was skipped; the
   rest of its condition admits that only for an all-skipped plan, and a
   failed or cancelled `publish` skips it.
3. The three checks, before any checkout and before any credential is
   requested, identical in the jobs `prepare` and `publish`:
   1. "Verify the tag of the Release run": the notes job's `run` script,
      byte for byte (a version tag of this repository that dereferences
      to the run's head commit).
   2. "The tagged commit is on main" (evidence 15):
      ```sh
      set -euo pipefail
      status=$(gh api "repos/${GITHUB_REPOSITORY}/compare/main...${HEAD_SHA}" --jq .status)
      case "${status}" in
        behind|identical) ;;
        *) echo "::error::${HEAD_SHA} is not an ancestor of main (${status})"; exit 1 ;;
      esac
      ```
      A failing `gh api` (an unknown commit is a 404) stops the step too.
      A tag pushed on an unmerged commit therefore publishes nothing; the
      owner's tag ruleset (proposal, owner step 4) keeps anyone else from
      pushing a `v*` tag at all.
   3. "The GitHub release exists and is not a draft":
      ```sh
      set -euo pipefail
      draft=$(gh api "repos/${GITHUB_REPOSITORY}/releases/tags/${TAG}" --jq .draft)
      if [[ "${draft}" != "false" ]]; then
        echo "::error::the release of ${TAG} is missing or a draft (draft: ${draft})"
        exit 1
      fi
      ```
      A missing release is a 404 and fails the `gh api` call; `true`, an
      empty answer or anything else fails the comparison. The Release
      run's host job creates the release only after the test gate legs
      succeeded, so the gate has passed before anything is published.
4. Job `prepare` (no credential): `timeout-minutes:
   60`; `permissions: { contents: read }`; no environment; `outputs:
   { nothing_to_publish: ${{ steps.plan.outputs.nothing_to_publish }} }`.
   Steps, in this order:
   1. The three checks.
   2. Checkout of `HEAD_SHA` with `persist-credentials: false`.
   3. Bun (the pinned action ci.yml uses), then
      `cd web && bun install --frozen-lockfile && bun run build`.
   4. Rust (the pinned toolchain action ci.yml uses, stable).
   5. `scripts/crates.sh check` (the same command as the pull request
      gate, on the tagged commit and its freshly built dashboard).
   6. "Plan", `id: plan`:
      `scripts/crates.sh preflight "${TAG#v}" target/package/SHA256SUMS`.
   7. "Hand over": copies the seven `target/package/<name>-<V>.crate`,
      `target/package/SHA256SUMS`, `target/package/publish-plan` and
      `crates/apb-server/dashboard/` into `$RUNNER_TEMP/verified/` and
      uploads that directory with `actions/upload-artifact` (pinned) as
      `verified-crates`, `retention-days: 7`. It is uploaded also for an
      all-skipped plan, as the record of what was compared.
5. Job `publish` (the credential): `needs: prepare`; `timeout-minutes:
   60`; `environment: crates-io`; `permissions: { contents: read }`.
   Steps, in this order:
   1. The three checks.
   2. Rust (the pinned toolchain action, stable), before the checkout.
   3. `actions/download-artifact` (pinned) of `verified-crates` into
      `$RUNNER_TEMP/verified`.
   4. "Check the handed-over packages" (inline, no repository code):
      with `V="${TAG#v}"` and the seven names in the publish order
      written in the step (a test keeps them equal to the script's order
      line), it fails with an `::error::` naming the check unless:
      1. `$RUNNER_TEMP/verified` holds exactly the seven
         `<name>-<V>.crate`, `SHA256SUMS`, `publish-plan` and the
         directory `dashboard`, nothing else;
      2. `SHA256SUMS` is exactly seven lines `<64 lowercase hex>  <name>-<V>.crate`
         in the publish order, and `sha256sum --strict --check SHA256SUMS`
         passes in that directory;
      3. every `.crate` lists only paths under `<name>-<V>/`, and its
         `<name>-<V>/Cargo.toml` holds the lines `name = "<name>"` and
         `version = "<V>"`;
      4. `publish-plan` is the line `<V>` followed by exactly seven lines
         `<name> publish` or `<name> skip` in the publish order;
      5. `dashboard/index.html` exists and `find dashboard ! -type f ! -type d`
         prints nothing (no link, no device, no pipe).
   5. Checkout of `HEAD_SHA` with `persist-credentials: false`.
   6. "Prepare the tree" (inline): fails when
      `find . -path ./.git -prune -o \( -path '*/.cargo/config' -o -path '*/.cargo/config.toml' \) -print`
      prints anything, or when `crates/apb-server/dashboard` already
      exists; then moves `$RUNNER_TEMP/verified/dashboard` to
      `crates/apb-server/dashboard`. No Bun step and no JavaScript
      install run in this job.
   7. "Repack and compare":
      `scripts/crates.sh repack "${TAG#v}" "$RUNNER_TEMP/verified"`, no
      credential in its environment.
   8. "Publish": the only step whose env has `CARGO_REGISTRY_TOKEN`
      (`${{ secrets.CARGO_REGISTRY_TOKEN }}`). It first fails closed with
      an `::error::` saying the `crates-io` environment has no
      `CARGO_REGISTRY_TOKEN` secret, or an empty one, and exits 1 before
      cargo runs; then
      `scripts/crates.sh publish "${TAG#v}" "$RUNNER_TEMP/verified"`.
   What ties the upload to the checked files: `prepare` records the
   SHA-256 of every verified `.crate`; the inline check proves the
   handed-over files match those records, names and version; `repack`
   packs the publishing crates again from `publish`'s own checkout
   without building and requires the same checksums before the credential
   exists; `publish` packs the same bytes (evidence 13) and afterwards
   requires the index checksum of every crate to equal the recorded one.
   A partly skipped plan repacks and publishes only its `publish` lines.
6. Job `installed` (no credential): `needs: [prepare,
   publish]`; `timeout-minutes: 60`; `permissions: { contents: read }`;
   no environment. Steps: checkout of `HEAD_SHA` with
   `persist-credentials: false` (for the script), Rust, then
   `scripts/crates.sh installed "${TAG#v}"`: the published version
   installs from the registry into an empty directory and reports its
   version. A failure here means the published release does not install;
   the owner yanks it (proposal, owner step 5).
7. The all-skipped path: when all seven checksums match the index,
   `prepare` reports `nothing_to_publish=true`, `publish` is skipped
   (no credential is acquired, no repacking, no `cargo publish`) and
   `installed` runs at once. This is the path of "Re-run all jobs" after
   a run whose publication succeeded and whose `installed` job failed.
8. Reruns: "Re-run all jobs" repeats `prepare` and makes a new artifact;
   crates already published with the verified checksums are skipped.
   "Re-run failed jobs" of a failed `publish` reuses that run's artifact,
   which passes the same inline check again.
9. Pre-releases are published: the workflow does not look at the
   pre-release flag. `cargo install agentic-playbooks` ignores
   pre-releases unless `--version` names one, so stable users never get
   one.
10. Why not a dist publish job: `publish-jobs` would need a regenerated
    `release.yml`; dist's generated publish condition reads one global
    `publish_prereleases` switch (the Homebrew job's `if:`), and turning
    it on would also publish Homebrew formulas for pre-releases, which
    `dist-workspace.toml` avoids on purpose. Whether a custom dist publish
    job gets the same condition was not verified (the dist book was
    unreachable); the choice does not depend on it.
11. Authentication: every release publishes with the `crates-io`
    environment's token (owner steps 2 and 3, Decision 2). crates.io does
    not accept trusted publishing from a `workflow_run` workflow, so it
    needs a different trigger and is a later change.

`crates/apb-cli/tests/suite/release_workflow_test.rs` gains, reusing its
`load`, `condition`, `run_of`, `eval_condition`, `release_run` and
`run_with_gh` helpers:

1. `crates_follow_every_completed_release_run`: the trigger, the empty
   top-level permissions, the concurrency group without cancellation,
   exactly the three jobs with their `needs` (`publish` needs `prepare`,
   `installed` needs `prepare` and `publish`), their permissions as the
   table above says, each map exactly (`prepare` and `installed`:
   `contents: read`, all three), `id-token` nowhere in the workflow,
   the environment `crates-io` on `publish` and on no other job, and the
   output `nothing_to_publish` of
   `prepare` read from the step `plan`.
2. `the_crates_job_conditions_admit_a_pre_release_and_refuse_a_pull_request`:
   the condition of `prepare` equals the notes job's; the condition of
   `publish` equals the notes condition followed by
   ` && needs.prepare.outputs.nothing_to_publish != 'true'`; the condition
   of `installed` equals the exact string of item 2.3 above. The notes
   condition part is evaluated with `eval_condition` as the notes test
   does; the other clauses are compared as exact text (the evaluator
   refuses `!` and parentheses by design).
3. `the_crates_jobs_check_tag_main_and_release_before_checkout`: in
   `prepare` and `publish` the first three steps are the tag check (its
   `run` byte for byte the notes job's), the main check and the release
   check, in that order, before any checkout step.
4. `the_main_check_admits_only_an_ancestor_of_main`: runs the main check
   step's script through `run_with_gh` with the stub answering `behind`
   and `identical` (success) and `ahead`, `diverged`, an empty answer and
   a failing call (failure).
5. `the_release_check_refuses_a_draft`: runs the release check step's
   script with the stub answering `false` (success) and `true`, `null`,
   an empty answer and a failing call (failure).
6. `no_code_from_the_checkout_runs_next_to_a_credential`:
   1. In `publish`: the steps are, in order, the three checks, Rust, the
      download, "Check the handed-over packages", the checkout, "Prepare
      the tree", "Repack and compare" and "Publish"; the only `uses` are
      the checkout, the Rust toolchain and `actions/download-artifact`
      actions (no job uses `rust-lang/crates-io-auth-action`); exactly
      two `run` scripts name `crates.sh`, "Repack and compare" (`crates.sh repack`) and "Publish" (`crates.sh publish`);
      no `run` names `bun`, `npm`, `npx`, `cargo build`, `cargo test`,
      `cargo run`, `cargo install`, `make`, `crates.sh check`,
      `crates.sh preflight` or `crates.sh installed`; the job env sets
      `RUSTUP_TOOLCHAIN` to `stable`.
   2. `CARGO_REGISTRY_TOKEN` appears only in the env of "Publish", as
      `${{ secrets.CARGO_REGISTRY_TOKEN }}`, and that step has no `if:`
      and checks the token, with `exit 1`, before `crates.sh publish`;
      `secrets.` appears only in `GH_TOKEN` of a check step (as
      `secrets.GITHUB_TOKEN`) and in the env of "Publish", never in a job
      env; `GH_TOKEN` appears only on the check steps; no job has
      `id-token`.
   3. `prepare` runs `crates.sh check` and then `crates.sh preflight`;
      `installed` runs `crates.sh installed`; neither has `id-token`, an
      environment or a `secrets.` reference other than `GH_TOKEN` of a
      check step.
   4. No step of any job uses `actions/cache` or an action whose name
      contains `cache`, and no `with:` of a setup action enables a cache.
7. `the_handed_over_packages_are_checked_inline`: builds, in a temporary
   `RUNNER_TEMP`, a `verified` directory with seven small `.crate`
   tarballs made by `tar` (each with its `<name>-<V>/Cargo.toml`), their
   `SHA256SUMS`, a plan and `dashboard/index.html`, and runs the step's
   `run` script with bash and `TAG=v<V>`: it passes. Each of these makes
   it fail: one checksum changed, an extra file, a crate whose
   `Cargo.toml` names another version, a crate with a path outside its
   directory, a plan with six package lines, a link under `dashboard/`.
   The seven names written in the step equal the order line of
   `scripts/crates.sh`.
8. `the_publish_step_fails_closed_without_the_token`: runs the
   "Publish" step's script with bash over a stand-in `scripts/crates.sh`
   that records being run: with `CARGO_REGISTRY_TOKEN` unset and with it
   empty, it exits 1 with the `::error::` line and the stand-in never
   runs; with a token set, the stand-in runs.
9. `the_tree_step_refuses_cargo_configuration`: runs "Prepare the tree"
   in a temporary checkout: with `crates/apb-core/.cargo/config.toml`
   present it fails and moves nothing; with an existing
   `crates/apb-server/dashboard` it fails; on a clean tree it moves the
   dashboard into place.
10. The existing `every_third_party_action_is_pinned_to_a_full_commit_sha`
   covers the new file without a change (it walks every workflow file).

## D7. `apb self-update` on a cargo install

The kind of install is a property of the binary, fixed when it was
compiled (evidence 11 and 12): a binary compiled from the published
package `agentic-playbooks` came from `cargo install`, whatever receipt
or record sits next to it; a binary compiled as `apb` came from the
installer, a source build or `cargo install --path`, which keep today's
behaviour. No file is read to decide it: not `.crates2.json`, not the
binary's path.

New module `crates/apb-cli/src/cargo_install.rs` (declared in `main.rs`):

1. `pub(crate) const PUBLISHED_PACKAGE: &str = "agentic-playbooks";` and
   `pub(crate) fn built_for_cargo_install(package: &str) -> bool`, true
   exactly when `package == PUBLISHED_PACKAGE`.
2. `pub(crate) fn guidance(running: &Version) -> String`: for a stable
   build `apb was installed with cargo. Update it with: cargo install --locked agentic-playbooks`;
   for a pre-release build the same plus
   ` --version <the version you want> (pre-releases are installed only by version)`;
   both end with `. apb self-update updates only installs made by the apb installer.`
3. `selfupdate.rs`: the body of `self_update_as` moves into a new
   `fn self_update_from(check, config_root, running, package: &str)`;
   `self_update_as` keeps its signature (the freeze's tests call it) and
   passes `env!("CARGO_PKG_NAME")`. In `self_update_from`, before
   `updater_for`, when `built_for_cargo_install(package)` holds, it
   returns `Outcome { code: 2, message: guidance(running), updated: false, warning: None }`
   for the check and the install alike. Nothing is downloaded, written or
   reconciled, and an installer receipt is never loaded.
4. `NO_RECEIPT_GUIDANCE` gains the sentence
   `To install the published crate with cargo instead, run: cargo install --locked agentic-playbooks.`
5. What the two replacement orders do:
   1. `cargo install agentic-playbooks` over an installer install (the
      receipt stays, at any version, the same one included): the running
      binary is the cargo one, so self-update prints the cargo command and
      never follows the receipt, and never overwrites the cargo binary.
   2. The installer over a cargo install (cargo's record stays, at any
      version, the same one included): the running binary is `apb`, so
      self-update follows the receipt as today. Cargo's record then names
      a file the installer wrote; that only matters to `cargo uninstall`,
      and `docs/INSTALL.md` says to remove one install before switching
      channels.
6. Tests in `cargo_install.rs` `mod tests`: `agentic-playbooks` is a cargo
   build, `apb` and `agentic-playbooks-core` are not; the stable and the
   pre-release guidance text. In `selfupdate.rs` `mod tests`, each
   calling `self_update_from` with the stub release API and an install
   receipt in the config root (the freeze's helpers):
   1. `a_cargo_build_ignores_an_installer_receipt_of_the_same_version`:
      package `agentic-playbooks`, receipt at the running version: code 2,
      the guidance, `updated: false`, the stub saw no request, the receipt
      file unchanged.
   2. `a_cargo_build_ignores_an_older_installer_receipt`: the same with a
      receipt of an older version.
   3. `a_cargo_build_answers_check_the_same_way`: with `check` set.
   4. `an_installer_build_follows_its_receipt_next_to_a_cargo_record`:
      package `apb`, receipt at the running version, and a `.crates2.json`
      holding `agentic-playbooks <running> (registry+https://github.com/rust-lang/crates.io-index)`
      in the parent of the receipt's install directory: the stub sees the
      request exactly as without that file.
   5. `an_installer_build_without_a_receipt_names_the_cargo_command`:
      package `apb`, no receipt: `NO_RECEIPT_GUIDANCE` with the new
      sentence, code 2.

## D8. Docs and the promise

What a cargo install needs, said the same way everywhere (evidence 16): a
Rust toolchain and a C compiler and linker (the usual build tools). The
workspace keeps its native dependencies (bundled SQLite, `ring`,
`aws-lc-sys`); no bun, no cmake and no pkg-config are needed.

1. `docs/INSTALL.md`: a new section "## 3. cargo (Rust and a C compiler)"
   after Homebrew; later sections renumber, and every in-file reference
   to a section number follows (search the docs for `INSTALL.md#` and
   "Section" first). Its text: `cargo install --locked agentic-playbooks`;
   "`--locked` builds with the exact dependency versions the release was
   tested with."; the binary is `apb`; what it needs: "Rust 1.99 or
   newer" (the `rust-version` of the release), and a C compiler and linker, on
   macOS the Xcode Command Line Tools (`xcode-select --install`), on
   Debian or Ubuntu the package `build-essential`
   (`sudo apt install build-essential`); no bun (the dashboard is inside
   the package); a release candidate needs `--version 1.0.0-rc.1`; update
   by running the same command again; `apb self-update` does not update a
   cargo install and says so; uninstall with
   `cargo uninstall agentic-playbooks`; remove one install before
   switching to another channel (D7 item 5); the crate named `apb` on
   crates.io is an unrelated project. The sentence "`cargo install apb`
   does not install this tool" under Homebrew becomes a pointer to the new
   section. "Updating" and "Uninstall" gain the cargo lines. "Build from
   source": the dashboard is written to `crates/apb-server/dashboard/`, a
   source build needs the same C compiler, `cargo install --git` stays
   unsupported because a git checkout has no built dashboard (the build
   stops with the D3 message).
2. `llms.txt`: the warning becomes "Install with cargo:
   `cargo install --locked agentic-playbooks` (the binary is `apb`; it
   needs Rust and a C compiler). The crate named `apb` on crates.io is an
   unrelated project: `cargo install apb` does not install this tool."
   The source build paragraph names the new path and the reason `--git`
   is unsupported.
3. `docs/COMPATIBILITY.md` (written by the freeze): "## What 1.x keeps"
   gains an item "install channels": the crate `agentic-playbooks`
   installs the binary `apb`; the installer `apb-installer.sh` at
   `.../releases/latest/download/apb-installer.sh`, the Homebrew formula
   `apb`, the archives `apb-<target>.tar.xz` and `apb self-update` of an
   installer install keep their names. "## Not covered" gains: the Rust
   API of the six `agentic-playbooks-*` library crates (internal, any item
   may change in any release; the crates may be merged or retired), the
   minimum Rust version of `cargo install` (the `rust-version` of the
   release, checked by the CI job `msrv`), which is not part of the 1.x
   promise and may rise in a minor release, and the native build tools a
   cargo install needs.
4. `docs/release-notes/v1.0.0-rc.1.md` (written by the freeze): under
   "## Installing the release candidate" a paragraph with
   `cargo install --locked agentic-playbooks --version 1.0.0-rc.1`, the
   `--locked` sentence and the build tools of the opening paragraph; a
   section "## Install with cargo" with the behaviour changes of the
   proposal; "## The soak" gains item 7: in a container `ubuntu:26.04`
   with exactly `build-essential`, `ca-certificates` and `curl` installed
   from apt and rustup's minimal stable toolchain (no bun, no cmake), the
   command above, then `apb --version` (`apb 1.0.0-rc.1`), `apb doctor`
   and `apb dashboard` (its page answers HTTP 200 from inside the
   container), recorded like item 6.
5. `CLAUDE.md` and `AGENTS.md` (the same shared text): the crate list
   names each crate's package (`apb-core` (package
   `agentic-playbooks-core`)); the apb-server bullet names
   `crates/apb-server/dashboard`; Commands: the single-test example uses
   `-p agentic-playbooks-engine`, the frontend build writes
   `crates/apb-server/dashboard`, a paragraph "Publishing to crates.io"
   names `scripts/crates.sh check|preflight|repack|publish|installed`,
   the D1 rename, the gate jobs `crates` and `msrv` (both required
   through the explicit result comparison of `build-and-test`) and the
   three jobs of
   `publish-crates.yml` (`prepare`, `publish`, `installed`, and that only
   `publish` holds the `crates-io` environment and its token); Release pipeline: the notes paragraph gains one
   sentence on `publish-crates.yml`.
6. `CONTRIBUTING.md`, `docs/TESTING-GUIDELINES.md`,
   `docs/BUILD-OPTIMIZATION.md`: the `-p apb-<library>` commands.
7. `RELEASE-CHECKLIST.md`: the bump step names the workspace `version` and
   the six `=<V>` versions of `[workspace.dependencies]` (the
   `every_internal_dependency_is_exact` test fails on a missed one);
   "After the merge" gains: watch the three jobs of `publish-crates`, and
   when `installed` fails, yank the seven versions and report it.
8. `docs/superpowers/specs/2026-11-22-cargo-install-design.md`: the public
   design record (the last task), D1 to D8 in short.

The generated `web/src/lib/api.gen.ts` and `web/src/lib/consts.gen.ts`
carry the regenerate command in their header (`ts_contract.rs` `HEADER`):
it becomes `cargo test -p agentic-playbooks-server --lib ts_contract`,
regenerated with that command, never edited by hand.

## Decisions

Decided by the orchestrator for the owner; nothing is left open.

1. The rc.1 tag: `v1.0.0-rc.1` is tagged only after this change merges,
   so the release candidate is the first publication and the soak covers
   the channel.
2. Token only: crates.io does not accept trusted publishing from a
   `workflow_run` workflow, so the 1.0 releases publish with the
   `crates-io` environment's token (scopes `publish-new` and
   `publish-update`, pattern `agentic-playbooks*`, valid one year), no
   job requests an identity token, and the "Publish" step fails closed
   when the token is absent or empty. Trusted publishing needs a
   different trigger and is a later change.
3. The one-line difference of the published CLI manifest (the package
   name) and `"dirty": true` in every `.cargo_vcs_info.json` (the built
   dashboard is untracked) are accepted; the alternatives of D1 break
   release names or the build.
4. The `crates-io` environment has no required reviewers: its branch
   rule, the tag ruleset, the main ancestry check and the release check
   bind a publication to a released commit of main.
5. docs.rs builds documentation for the six libraries on its own; crates
   cannot opt out. Accepted: their first doc line says they are internal.

## Files

Counted against the owner's limit of about 80 changed files per pull
request. Paths with (new) are added; (link) is a committed symbolic link.

Manifests and lock (9):

1. `Cargo.toml`
2. `Cargo.lock`
3. `crates/apb-cli/Cargo.toml`
4. `crates/apb-core/Cargo.toml`
5. `crates/apb-engine/Cargo.toml`
6. `crates/apb-server/Cargo.toml`
7. `crates/apb-mcp/Cargo.toml`
8. `crates/apb-mcp-client/Cargo.toml`
9. `crates/apb-decide/Cargo.toml`

Library docs (6):

10. `crates/apb-core/src/lib.rs`
11. `crates/apb-engine/src/lib.rs`
12. `crates/apb-server/src/lib.rs`
13. `crates/apb-mcp/src/lib.rs`
14. `crates/apb-mcp-client/src/lib.rs`
15. `crates/apb-decide/src/lib.rs`

License links (7):

16. `crates/apb-cli/LICENSE` (link)
17. `crates/apb-core/LICENSE` (link)
18. `crates/apb-engine/LICENSE` (link)
19. `crates/apb-server/LICENSE` (link)
20. `crates/apb-mcp/LICENSE` (link)
21. `crates/apb-mcp-client/LICENSE` (link)
22. `crates/apb-decide/LICENSE` (link)

Embedded content (17):

23. `crates/apb-core/embedded/bundled` (link)
24. `crates/apb-core/embedded/connectors` (link)
25. `crates/apb-core/embedded/templates` (link)
26. `crates/apb-core/embedded/models.yaml` (link)
27. `crates/apb-core/embedded/near-ai-cloud.json` (link)
28. `crates/apb-core/src/bundled/mod.rs`
29. `crates/apb-core/src/connector/official.rs`
30. `crates/apb-core/src/templates.rs`
31. `crates/apb-core/src/models_table.rs`
32. `crates/apb-core/src/cloud_models/near_ai.rs`
33. `crates/apb-mcp/embedded/HOWTO-authoring.md` (link)
34. `crates/apb-mcp/embedded/HOWTO-interview.md` (link)
35. `crates/apb-mcp/embedded/AGENT-SETUP.md` (link)
36. `crates/apb-mcp/embedded/playbook-instructions.md` (link)
37. `crates/apb-mcp/src/tools/playbook.rs`
38. `crates/apb-mcp/src/setup/mod.rs`
39. `crates/apb-mcp/src/setup/standing.rs`

Dashboard (6):

40. `crates/apb-server/src/assets.rs`
41. `crates/apb-server/build.rs` (new)
42. `web/vite.config.ts`
43. `web/scripts/third-party-notices.ts`
44. `scripts/third-party-notices.sh`
45. `.gitignore`

CLI (4):

46. `crates/apb-cli/README.md` (new)
47. `crates/apb-cli/src/cargo_install.rs` (new)
48. `crates/apb-cli/src/selfupdate.rs`
49. `crates/apb-cli/src/main.rs`

Scripts and workflows (5):

50. `scripts/crates.sh` (new)
51. `scripts/daily-cleanup.sh`
52. `.github/workflows/ci.yml`
53. `.github/workflows/test-gate.yml`
54. `.github/workflows/publish-crates.yml` (new)

Tests (3):

55. `crates/apb-cli/tests/suite/crate_manifests_test.rs` (new)
56. `crates/apb-cli/tests/suite/release_workflow_test.rs`
57. `crates/apb-cli/tests/main.rs`

Package names in commands and generated headers (5):

58. `crates/apb-engine/src/decision/report/tests.rs`
59. `crates/apb-engine/tests/suite/agent_env_live_test.rs`
60. `crates/apb-server/src/ts_contract.rs`
61. `web/src/lib/api.gen.ts`
62. `web/src/lib/consts.gen.ts`

Docs (11):

63. `docs/INSTALL.md`
64. `llms.txt`
65. `docs/COMPATIBILITY.md`
66. `docs/release-notes/v1.0.0-rc.1.md`
67. `CLAUDE.md`
68. `AGENTS.md`
69. `CONTRIBUTING.md`
70. `docs/TESTING-GUIDELINES.md`
71. `docs/BUILD-OPTIMIZATION.md`
72. `RELEASE-CHECKLIST.md`
73. `docs/superpowers/specs/2026-11-22-cargo-install-design.md` (new)

OpenSpec at archive time (9): the six files of this change move under
`openspec/changes/archive/<date>-cargo-install/`, and three new spec files
`openspec/specs/crate-packaging/spec.md`,
`openspec/specs/crate-publication/spec.md` and
`openspec/specs/cargo-install-channel/spec.md` (74 to 82).

Total: 82 changed files, 73 without the archive. Not counted:
`THIRD-PARTY-NOTICES.txt`, regenerated because removing `tower-http` and
`imap-proto` 0.17 (D2) drops their notices; the rename alone changes
nothing in it (`about.toml` ignores the workspace's own crates). Not changed on purpose: `dist-workspace.toml`,
`.github/build-setup.yml`, `.github/workflows/release.yml`,
`.github/workflows/release-notes.yml`, `README.md`.
