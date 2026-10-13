# Contributing to Agentic Playbooks

Thank you for helping improve `apb`.

## License of contributions

The project is licensed under the Apache License, Version 2.0.
Unless you explicitly state otherwise, every contribution intentionally
submitted for inclusion in this project is licensed under the same terms.
No separate Contributor License Agreement is required at this time.

## Developer Certificate of Origin

Every commit submitted through a pull request must include a `Signed-off-by`
line certifying the Developer Certificate of Origin in [DCO](DCO).

Create a signed-off commit with:

```sh
git commit -s -m "Describe the change"
```

If you forgot the sign-off on the latest commit:

```sh
git commit --amend --signoff --no-edit
```

Use your real name and an email address you are entitled to use. The sign-off
means that you wrote the contribution, or otherwise have the right to submit it
under the project's Apache-2.0 license. It is not a copyright assignment.

## Building and testing

The binary embeds the web frontend, so build `web/` first:

```sh
(cd web && bun install && bun run build)
cargo build
```

Gates a pull request must pass (CI runs the same):

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
(cd web && bun run check && bun run test)
cargo deny --all-features check
```

Run one test with `cargo test -p <crate> --test main <module>::<test_name>`
(each crate has one integration test binary, `main`; the CLI package is
named `apb`). After changing a Rust type or constant mirrored into
TypeScript, regenerate `web/src/lib/api.gen.ts` and
`web/src/lib/consts.gen.ts` with
`APB_WRITE_TS=1 cargo test -p apb-server --lib ts_contract`.

More: [docs/TESTING-GUIDELINES.md](docs/TESTING-GUIDELINES.md) for how tests
are written here, and
[docs/BUILD-OPTIMIZATION.md](docs/BUILD-OPTIMIZATION.md) for faster local
builds.

## Before opening a pull request

- Keep each pull request focused and explain why the change is needed.
- Add or update tests and documentation when behavior changes.
- Do not commit secrets, credentials, private prompts, personal data, or run logs.
- Do not submit third-party code, assets, examples, or generated files unless
  their source and license are clearly identified and compatible with this project.
- If an employer or client may own your work, obtain permission before submitting it.
- If AI tools materially assisted the contribution, disclose that in the pull
  request. You remain responsible for reviewing, testing, and having the right to
  submit all resulting material.

## Third-party notices

Release archives ship `THIRD-PARTY-NOTICES.txt`, the license texts of every
Rust crate compiled into `apb` and every npm package the web dashboard
bundles. The file is generated; never edit it by hand. After changing
`Cargo.lock` or `web/bun.lock`, regenerate it and commit the result:

```sh
scripts/third-party-notices.sh
```

It needs `cargo-about` (`scripts/install-cargo-about.sh` installs the pinned
release on x86_64 Linux, or `cargo install --locked cargo-about --features cli
--version 0.9.2`) and `bun`. CI runs `scripts/third-party-notices.sh --check`
and fails when the committed file is stale. A crate under a license outside
the `about.toml` allowlist fails generation until the license is reviewed.

## Security-sensitive changes

Playbooks can execute scripts and invoke coding agents. Treat changes involving
command execution, file access, web or MCP interfaces, credentials, supervisor
permissions, and run-state storage as security-sensitive. Describe the trust
boundary and expected failure modes in the pull request.

## Pull request checklist

- All commits in the pull request are signed off under the DCO.
- Tests pass locally.
- User-facing changes are documented.
- No secrets or private run data are included.
- Third-party material and licenses are disclosed.
- Security implications have been considered.
