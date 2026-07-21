# Installing apb

`apb` is a single-binary CLI: the playbook engine, a web UI (embedded), and an
MCP server in one executable.

## 1. One-liner install (macOS, Linux)

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/itechmeat/agentic-playbooks/releases/latest/download/apb-installer.sh | sh
```

This downloads the shell installer generated for the latest tagged release,
verifies it, and places the `apb` binary in `CARGO_HOME` (`~/.cargo/bin` by
default). It also writes an environment script and an install receipt under
`~/.config/apb/`; the receipt is what `apb self-update` reads later to update
in place.

To pin a specific version instead of the latest release, replace
`latest/download` with `download/vX.Y.Z`:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/itechmeat/agentic-playbooks/releases/download/vX.Y.Z/apb-installer.sh | sh
```

## 2. Homebrew (macOS, Linux)

```sh
brew install itechmeat/agentic-playbooks/apb
```

This taps `itechmeat/homebrew-agentic-playbooks` and installs the `apb`
formula.

## 3. Updating

- Installer-based installs: `apb self-update` updates the binary in place
  using the install receipt written under `~/.config/apb/`. It only works for
  installs made through the one-liner installer (or another installer-based
  method); it will not work for a Homebrew install or a source build.
- `apb self-update --check` reports whether an update is available without
  installing it. Exit codes: `0` already up to date, `10` an update is
  available, `2` the check failed (including when there is no install
  receipt to read, in which case it prints guidance instead of updating).
- Homebrew installs: `brew upgrade apb`.
- Source builds: pull the latest source, rebuild the web assets, and re-run
  `cargo install --path crates/apb-cli` (see Section 5).

## 4. Manual download and verify

Pick the archive that matches your platform from the
[Releases page](https://github.com/itechmeat/agentic-playbooks/releases),
then verify and unpack it:

```sh
shasum -a 256 -c apb-aarch64-apple-darwin.tar.gz.sha256
tar -xzf apb-aarch64-apple-darwin.tar.gz
mv apb /usr/local/bin/
apb --version
```

Each archive contains the `apb` binary and a copy of `LICENSE`, with a
companion `.sha256` checksum file.

## 5. Build from source (contributor path)

### Important: the web frontend is embedded in the binary

`apb-server` embeds the built frontend from `web/dist` via rust-embed. The
`web/dist` directory is not stored in git (gitignored), so when building from
source you must build it BEFORE `cargo build`, otherwise compilation fails on
the missing directory.

```sh
git clone https://github.com/itechmeat/agentic-playbooks && cd agentic-playbooks
(cd web && bun install && bun run build)
cargo install --path crates/apb-cli
```

`cargo install --git ...` is not supported: git mode builds the package in a
private cargo cache where the built `web/dist` cannot be placed, and without
it the `apb-server` build (rust-embed) fails. Install from a local clone
(`--path`).

To update a source install: `git pull`, rebuild `web/`, re-run
`cargo install --path crates/apb-cli`.

## 6. Uninstall

- Installer-based install: `rm ~/.cargo/bin/apb` (or wherever `CARGO_HOME`
  points), and remove the install receipt directory `~/.config/apb/` if you
  no longer want it around.
- Homebrew: `brew uninstall apb`.
- Source (`cargo install`): `cargo uninstall apb`.

None of these touch your project data. `.apb/` in each project and
`~/.config/apb/` (apb's own global config, separate from the install receipt
mentioned above) are never removed by uninstalling the binary; delete them
yourself if you want a clean slate.

## 7. After install: `apb init`

Run `apb init` in a project to set up its `.apb/` structure. In an
interactive terminal this now runs a short questionnaire:

1. Feedback-loop consent (default: yes). Accepting creates or appends the
   feedback-loop section into the current project's `CLAUDE.md` and
   `AGENTS.md`. This step is idempotent: running `apb init` again does not
   duplicate the section.
2. The agent subscriptions survey, shown only when subscriptions have not
   already been declared for this project.

Press Esc or Ctrl+C at any point to cancel cleanly (exit code 0).

Non-interactive runs (CI, agents, piped input) skip both prompts and behave
exactly as before: `apb init` completes without asking anything.

## Checking the environment

```sh
apb doctor
```

Shows the availability of agent binaries and runner runtimes, playbook
validity, and the state of the config and registry.
