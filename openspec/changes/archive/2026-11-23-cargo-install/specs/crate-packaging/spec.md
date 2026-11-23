## ADDED Requirements

### Requirement: Release names stay as they are
The repository SHALL keep the CLI package named `apb`, so that the release pipeline derives the same app name, installer file name, archive names, updater names and Homebrew formula as before this change. Only the publication step SHALL rename that package to `agentic-playbooks`, in its own checkout, and it SHALL restore the manifest and the lockfile afterwards.

#### Scenario: The release plan is unchanged
- **WHEN** `dist plan` runs on the workspace after this change
- **THEN** it lists one app `apb` with `apb-installer.sh`, `apb.rb`, `apb-<target>.tar.xz` and `apb-<target>-update` for every target, exactly as before

#### Scenario: The publication rename touches one line
- **WHEN** the publication step renames the CLI package
- **THEN** exactly the package name line of `crates/apb-cli/Cargo.toml` and the member's name line of `Cargo.lock` change, the bin target stays `apb`, and every dependency keeps its version

#### Scenario: The rename works on a cold cache
- **WHEN** the publication rename runs with an empty registry cache
- **THEN** it edits the two lines in place without running a resolver, and `cargo metadata --locked` accepts the edited lockfile

#### Scenario: A local check leaves the tree clean
- **WHEN** a person runs `scripts/crates.sh check` in a clean checkout and it ends, with success or failure
- **THEN** `crates/apb-cli/Cargo.toml` and `Cargo.lock` are as they were before

#### Scenario: A modified manifest is refused
- **WHEN** `crates/apb-cli/Cargo.toml` or `Cargo.lock` has uncommitted changes
- **THEN** `scripts/crates.sh` refuses to start and changes nothing

### Requirement: Library packages are renamed and keep their import names
The six library packages SHALL be named `agentic-playbooks-core`, `agentic-playbooks-engine`, `agentic-playbooks-server`, `agentic-playbooks-mcp`, `agentic-playbooks-mcp-client` and `agentic-playbooks-decide`, with library names `apb_core`, `apb_engine`, `apb_server`, `apb_mcp`, `apb_mcp_client` and `apb_decide`. Every normal dependency on one of them SHALL go through one workspace dependency entry that keeps the old key, names the package and requires the exact workspace version.

#### Scenario: Rust code is unchanged
- **WHEN** the workspace builds after the rename
- **THEN** no `use` line or path in any crate's sources names a new crate name

#### Scenario: A version bump misses an internal dependency
- **WHEN** the workspace version is bumped and one internal dependency still requires the old version
- **THEN** the manifest test fails naming that dependency

#### Scenario: A test seam is never published
- **WHEN** a library is packaged
- **THEN** its published manifest has no dev-dependency on an internal crate and no dependency enabling a `testing` feature

### Requirement: The dashboard is built into the server crate
The built dashboard SHALL live in `crates/apb-server/dashboard/`, the one path the workspace build, CI, the release build and the published package read. A build of the server crate without `dashboard/index.html` SHALL stop with a message naming the build command, and SHALL never produce a binary with an empty dashboard.

#### Scenario: A fresh checkout without the dashboard
- **WHEN** `cargo build` runs in a checkout where the dashboard was not built
- **THEN** the build fails with a message that names `bun run build` and `crates/apb-server/dashboard`

#### Scenario: An empty dashboard directory
- **WHEN** `crates/apb-server/dashboard/` exists but holds no `index.html`
- **THEN** the build fails with the same message

#### Scenario: The published server package
- **WHEN** the server crate is packaged after a dashboard build
- **THEN** the package holds `dashboard/index.html` and the files under `dashboard/assets/`

### Requirement: Every package is self-contained
Every published package SHALL hold `LICENSE` and every file its build embeds. Content kept at the repository root (shipped playbooks, official connectors, template gallery, models table, cloud catalog, the documents and the standing block the MCP server serves) SHALL be reached through committed symbolic links inside the crate, which packaging resolves into files.

#### Scenario: A package built from crates.io alone
- **WHEN** a package is verified by building it from its packed sources only
- **THEN** the build succeeds without any file of the repository outside that package

#### Scenario: The core package holds the shipped content
- **WHEN** the core crate is packaged
- **THEN** it holds every tracked file of `bundled/`, `connectors/` and `examples/playbooks/` and both asset files under `embedded/`

### Requirement: Pull requests fail when the workspace is not publishable
CI SHALL run `scripts/crates.sh check` on every pull request: the publication rename, `cargo package --workspace` with verification, checks of every package's file list and size, and a record of every package's SHA-256. `check` SHALL refuse to run with a registry token in its environment.

#### Scenario: A new embed outside the crate
- **WHEN** a change makes a crate embed a file outside its directory without a link
- **THEN** the verify build of `cargo package --workspace` fails and so does the pull request

#### Scenario: Interdependent crates that are not published
- **WHEN** the gate runs before any version of the crates exists on crates.io
- **THEN** every package verifies against the other packed packages through cargo's temporary local registry

#### Scenario: A package too large
- **WHEN** a packed crate is larger than 10000000 bytes
- **THEN** the gate fails naming the crate and its size

#### Scenario: A missing license
- **WHEN** a packed crate holds no `LICENSE`
- **THEN** the gate fails naming the crate

#### Scenario: A token in the environment
- **WHEN** `scripts/crates.sh check` starts with `CARGO_REGISTRY_TOKEN` or a `CARGO_REGISTRIES_*_TOKEN` variable set
- **THEN** it refuses before any cargo or network call

### Requirement: The packaging job is part of the required check
The aggregate check `build-and-test` SHALL list the job `crates` in its `needs` and SHALL compare `needs.crates.result` with `success` explicitly, as it does for every job it needs, because it runs under `!cancelled()` also after a needed job failed and fails only on what it compares.

#### Scenario: The packaging job fails
- **WHEN** the job `crates` fails, is cancelled or is skipped and every other Linux job succeeded
- **THEN** the required check `build-and-test` fails

#### Scenario: Every Linux job succeeded
- **WHEN** the jobs `web`, `lint`, `test-linux`, `journal-corpus`, `crates` and `msrv` all succeeded
- **THEN** the required check `build-and-test` succeeds

### Requirement: The minimum Rust version is checked
The workspace SHALL declare `rust-version = "1.99"` in `[workspace.package]`, the lowest Rust it builds with, inherited by every member. The CI job `msrv` SHALL read that value from `Cargo.toml` (repeating no version), install exactly that toolchain, use it over `rust-toolchain.toml` for every later step, and run `cargo check --workspace --locked`, while `scripts/crates.sh check` and the publication preflight keep the stable toolchain.

#### Scenario: Code or a dependency needs a newer Rust
- **WHEN** a change makes the workspace fail to build with the declared `rust-version`
- **THEN** the job `msrv` fails and so does the required check `build-and-test`

#### Scenario: The minimum is raised
- **WHEN** a release raises `rust-version` in `Cargo.toml`
- **THEN** the job `msrv` checks the new version with no change to the workflow

### Requirement: The minimum Rust version job is part of the required check
The aggregate check `build-and-test` SHALL list `msrv` in its `needs` and SHALL compare `needs.msrv.result` with `success` explicitly, as it does for every job it needs.

#### Scenario: The minimum Rust version job is not required
- **WHEN** `build-and-test` does not list `msrv` in its `needs` or does not compare its result
- **THEN** `release_workflow_test` fails
