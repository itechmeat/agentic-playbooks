# First public release checklist

## Ownership and provenance

- Confirm that every current author had the right to contribute the code.
- Check employment, client, and contractor agreements that may cover the work.
- Remove copied snippets, assets, fonts, examples, or generated material whose
  origin or license cannot be established.
- Search the repository history for secrets and private data before publishing.

## Project metadata

- Keep the Apache-2.0 text in the repository root as `LICENSE` without changes.
- Add `license = "Apache-2.0"` to Cargo package metadata. In a workspace, this can
  be defined under `[workspace.package]` and inherited by member crates.
- Add `"license": "Apache-2.0"` to relevant `package.json` files.
- Add the Security, Contributing, and License sections from `README-SECTIONS.md`.
- Replace placeholder repository URLs and package-owner names in the README.

## Dependency licenses

- Review Rust dependencies with `cargo deny check licenses`.
- Review web dependencies and all assets embedded in the released binary.
- Generate and ship third-party license notices with binary release archives
  when required by dependency licenses.
- Keep `Cargo.lock` and the current text-based `bun.lock` committed.

## GitHub settings

- Install the DCO GitHub App for this repository.
- Create a ruleset for `main` that requires pull requests and passing status checks.
- Make CI, DCO, and dependency review required checks after each has run once.
- Block force pushes and branch deletion for `main`.
- Enable the dependency graph, Dependabot alerts, and security updates.
- Enable private vulnerability reporting.

## Release artifacts

- Include `LICENSE` and required third-party notices in every source and binary archive.
- Test installation from the exact archives that will be published.
- Publish checksums for binary archives.
- Confirm that the web server still binds to loopback by default.
- Confirm that runtime state, logs, tokens, and credentials are not included in archives.

## Naming

- Check the project, binary, crate, package, Homebrew formula, domain, and social
  names before investing in branding. A software license does not grant trademark rights.

## Revisit CLA only when needed

Do not add a CLA merely because the project is open source. Revisit the decision
before accepting external contributions only if the intended business model becomes
community copyleft plus a separate proprietary license for the same contributed code.
