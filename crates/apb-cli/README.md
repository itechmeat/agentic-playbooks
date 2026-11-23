# agentic-playbooks: the `apb` command

apb runs agentic playbooks locally: repeatable multi-step workflows for coding
agents, defined in YAML, with a web dashboard and an MCP server your agent can
drive. This crate builds the `apb` binary.

## Install with cargo

```sh
cargo install --locked agentic-playbooks
```

`--locked` builds with the exact dependency versions the release was tested
with. The installed binary is named `apb`. To pin a release, name its
version: `cargo install --locked agentic-playbooks --version 1.0.0`. A
release candidate is installed only by its version.

It needs Rust 1.99 or newer (the crate's `rust-version`), and a C compiler
and linker: on macOS the Xcode Command Line Tools (`xcode-select
--install`), on Debian or Ubuntu the package
`build-essential`. No bun is needed: the dashboard is built into the package.

Update by running the same command again. `apb self-update` updates only
installs made by the apb installer; on a cargo install it prints the cargo
command instead. Uninstall with `cargo uninstall agentic-playbooks`.

The crate named `apb` on crates.io is an unrelated project:
`cargo install apb` does not install this tool.

## Other ways to install

The shell installer, Homebrew and the release archives are described in
[docs/INSTALL.md](https://github.com/itechmeat/agentic-playbooks/blob/main/docs/INSTALL.md).
Remove one install before switching to another channel.

## Compatibility

What the 1.x releases keep, and what they do not, is written in
[docs/COMPATIBILITY.md](https://github.com/itechmeat/agentic-playbooks/blob/main/docs/COMPATIBILITY.md).
The `agentic-playbooks-*` library crates are internal to this command and are
not a public API.
