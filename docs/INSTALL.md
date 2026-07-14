# Installing apb

`apb` is a single-binary CLI: the playbook engine, a web UI (embedded), and an
MCP server in one executable.

The project is currently pre-release. The supported install path is building
from a local clone. Prebuilt binaries, a Homebrew tap, and SHA256 checksums
will be published alongside the first tagged release (`v0.1.0`).

## Important: the web frontend is embedded in the binary

`apb-server` embeds the built frontend from `web/dist` via rust-embed. The
`web/dist` directory is not stored in git (gitignored), so when building from
source you must build it BEFORE `cargo build`, otherwise compilation fails on
the missing directory.

```sh
cd web
bun install
bun run build
cd ..
```

Prebuilt binaries from releases will already contain the built web assets, so a
separate frontend build is not needed for them.

## Installation methods

### 1. cargo install (from a local clone) - currently the only supported path

Clone the repository, build the web assets (see above), then install from the
local working copy:

```sh
git clone https://github.com/itechmeat/agentic-playbooks && cd agentic-playbooks
(cd web && bun install && bun run build)
cargo install --path crates/apb-cli
```

`cargo install --git ...` is not supported: git mode builds the package in a
private cargo cache where the built `web/dist` cannot be placed, and without
it the `apb-server` build (rust-embed) fails. Install from a local clone
(`--path`).

### 2. Prebuilt binaries (planned for v0.1.0, not yet available)

Once `v0.1.0` is tagged, release archives will be available for
`aarch64-apple-darwin`, `x86_64-apple-darwin`, and `x86_64-unknown-linux-gnu`.
Each archive will contain the `apb` binary and a copy of `LICENSE`, with a
companion SHA256 checksum file.

```sh
# Planned (not yet available):
tar -xzf apb-aarch64-apple-darwin.tar.gz
mv apb /usr/local/bin/
apb --version
```

### 3. Homebrew (planned for v0.1.0, not yet available)

A formula template lives at `packaging/apb.rb`. Once a release is published
and the formula's sha256 placeholders are filled in:

```sh
# Planned (not yet available):
brew install itechmeat/tap/apb
```

## Checking the environment

```sh
apb doctor
```

Shows the availability of agent binaries and runner runtimes, playbook
validity, and the state of the config and registry.
