# cargo-install-channel Specification

## Purpose

apb installs from crates.io with `cargo install --locked agentic-playbooks`, which builds the binary `apb` with the dashboard inside; `apb self-update` on a cargo install names the cargo command instead of updating, and the six library crates say they are internal to the `apb` command.

## Requirements

### Requirement: The crate agentic-playbooks installs apb
`cargo install --locked agentic-playbooks` SHALL build and install the binary `apb` on a machine with a Rust toolchain and a C compiler and linker (the usual build tools: the Xcode Command Line Tools on macOS, `build-essential` on Debian or Ubuntu) and no bun, cmake or pkg-config, with the exact dependency versions the release was tested with. Every place that describes the cargo channel SHALL name the C compiler and linker next to Rust.

#### Scenario: A machine with Rust and the usual build tools
- **WHEN** a person in a container `ubuntu:26.04` with only `build-essential`, `ca-certificates`, `curl` and rustup's stable toolchain runs `cargo install --locked agentic-playbooks`
- **THEN** `apb --version` prints the release version and `apb dashboard` serves the built dashboard

#### Scenario: An older Rust
- **WHEN** the toolchain is older than the release's `rust-version`
- **THEN** cargo refuses the install naming the version it needs

#### Scenario: The install docs name the build tools
- **WHEN** a person reads the cargo section of `docs/INSTALL.md`
- **THEN** it names a C compiler and linker with the macOS and the Debian or Ubuntu packages, and never says that Rust alone is enough

### Requirement: The docs name the minimum Rust version
`docs/INSTALL.md` SHALL name the minimum Rust version as "Rust 1.99 or newer", the `rust-version` of the release, and the compatibility wording SHALL say that the minimum Rust version is not part of the 1.x promise and may rise in a minor release.

#### Scenario: The install docs name the minimum Rust
- **WHEN** a person reads the cargo section of `docs/INSTALL.md`
- **THEN** it says "Rust 1.99 or newer"

### Requirement: Self-update leaves a cargo install alone
The kind of install SHALL be decided by the package name the running binary was compiled as, never by a file on disk. A binary compiled from the published package `agentic-playbooks` SHALL answer `apb self-update`, with or without `--check`, with exit code 2 and the cargo command that updates it, and SHALL change nothing and never follow an installer receipt. A binary compiled as `apb` SHALL behave as before this change, whatever cargo install record sits in its install root.

#### Scenario: A cargo install of a stable release
- **WHEN** apb 1.0.0 compiled from the published package runs `apb self-update`
- **THEN** it prints `cargo install --locked agentic-playbooks`, exits 2, and downloads and writes nothing

#### Scenario: A cargo install of a release candidate
- **WHEN** apb 1.0.0-rc.1 compiled from the published package runs `apb self-update`
- **THEN** the message also says that a pre-release is installed with `--version`

#### Scenario: Cargo replaced an installer install of the same version
- **WHEN** the config root holds an installer receipt at the running version and the running binary was compiled from `agentic-playbooks`
- **THEN** self-update prints the cargo command, exits 2, makes no request and leaves the receipt unchanged

#### Scenario: Cargo replaced an installer install of an older version
- **WHEN** the installer receipt names an older version and the running binary was compiled from `agentic-playbooks`
- **THEN** self-update prints the cargo command and changes nothing

#### Scenario: The installer replaced a cargo install of the same version
- **WHEN** the running binary was compiled as `apb`, an installer receipt names the running version and cargo's record lists `agentic-playbooks` at the same version
- **THEN** self-update follows the installer path exactly as without that record

#### Scenario: A build without a receipt
- **WHEN** a binary compiled as `apb` finds no installer receipt
- **THEN** it exits 2 with the existing guidance, which also names `cargo install --locked agentic-playbooks`

### Requirement: The library crates are not a public API
Each library crate SHALL say in its crates.io description and in its first documentation paragraph that it is internal to the apb command and not a public API, and SHALL carry no keywords or categories. `docs/COMPATIBILITY.md` SHALL list their Rust API as not covered by the 1.x promise, and SHALL list the install channels as covered: the crate `agentic-playbooks` installs the binary `apb`, and the installer, the Homebrew formula, the archives and installer self-update keep their names.

#### Scenario: A person reads a library crate's page
- **WHEN** a person opens `agentic-playbooks-core` on crates.io or docs.rs
- **THEN** the first sentence says it is an internal crate of the apb command and not a public API

### Requirement: The install docs name the cargo channel
`docs/INSTALL.md` SHALL describe `cargo install --locked agentic-playbooks` with its build tools, explain `--locked` in one plain sentence, show the `--version` form for a release candidate, and keep the warning that the crate named `apb` on crates.io is unrelated; `llms.txt` SHALL keep that warning next to the right command.

#### Scenario: An agent reads llms.txt
- **WHEN** an agent looks for a cargo install command in `llms.txt`
- **THEN** it finds `cargo install --locked agentic-playbooks` and the sentence that `cargo install apb` does not install this tool
