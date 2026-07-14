# Installing apb

`apb` is a single-binary CLI: the playbook engine, a web UI (embedded), and an
MCP server in one executable.

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

Prebuilt binaries from releases already contain the built web assets, so a
separate frontend build is not needed for them.

## Installation methods

### 1. Prebuilt binaries (recommended)

Download the archive for your platform from the releases page and unpack
`apb` into any directory on your `PATH`:

```sh
# example for Apple Silicon
tar -xzf apb-aarch64-apple-darwin.tar.gz
mv apb /usr/local/bin/
apb --version
```

Release targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`x86_64-unknown-linux-gnu`.

### 2. Homebrew (tap)

A formula template lives at `packaging/apb.rb` (points at the release
archives). After publishing a release and filling in the sha256:

```sh
brew install itechmeat/tap/apb
```

### 3. cargo install (from a local clone)

Clone the repository, build the web assets (see above), then install from the
local working copy:

```sh
git clone https://github.com/OWNER/playbooks && cd playbooks
(cd web && bun install && bun run build)
cargo install --path crates/apb-cli
```

`cargo install --git ...` is not supported: git mode builds the package in a
private cargo cache where the built `web/dist` cannot be placed, and without
it the `apb-server` build (rust-embed) fails. Install from a local clone
(`--path`), or use the prebuilt binaries.

## Checking the environment

```sh
apb doctor
```

Shows the availability of agent binaries and runner runtimes, playbook
validity, and the state of the config and registry.
