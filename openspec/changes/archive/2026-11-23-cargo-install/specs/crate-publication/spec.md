## ADDED Requirements

### Requirement: Crates are published only from a released commit of main
The workflow `publish-crates` SHALL run only after a successful push run of the Release workflow of this repository whose ref is a version tag that dereferences to the commit that run built. Before any checkout and before any credential is requested it SHALL check that this commit is an ancestor of main and that the GitHub release of the tag exists with `draft` exactly `false`, and SHALL fail closed on any other answer.

#### Scenario: A pull request run of Release
- **WHEN** the Release workflow ran for a pull request
- **THEN** no job of `publish-crates` runs

#### Scenario: A failed release
- **WHEN** the Release run failed, for example on a test gate leg
- **THEN** no job of `publish-crates` runs and nothing is published

#### Scenario: A tag that names another commit
- **WHEN** the run's tag dereferences to a commit other than the one the run built
- **THEN** the job fails before checkout and nothing is published

#### Scenario: A tag on an unmerged commit
- **WHEN** the compare of main with the tagged commit answers anything but `behind` or `identical`, or the call fails
- **THEN** the job fails before checkout and before any credential, and nothing is published

#### Scenario: No release, or only a draft
- **WHEN** the release lookup fails or answers `draft` with any value other than `false`
- **THEN** the job fails before checkout and nothing is published

### Requirement: The preparing job holds no credential
Publication SHALL run in three jobs on separate runners: `prepare`, `publish` and `installed`. The job `prepare` SHALL hold `contents: read` only, no environment and no `id-token` permission. It SHALL build the dashboard, run `scripts/crates.sh check` and `scripts/crates.sh preflight` on the tagged commit, and hand the seven verified packages, their SHA-256 checksums, the publish plan and the built dashboard to `publish` as an artifact. No job SHALL restore or save an actions cache.

#### Scenario: Build code in the preparing job
- **WHEN** the dashboard build or a build script of the verify build runs in the job `prepare`
- **THEN** that job holds no registry token, no `id-token` permission and no environment, so the code cannot request a crates.io credential

#### Scenario: A cache written by a pull request
- **WHEN** any job of `publish-crates` runs
- **THEN** it restores no actions cache, so no cache entry reaches its build

### Requirement: The publishing job runs only the publish script of the tagged checkout
The job `publish` SHALL be the only job with the environment `crates-io`, and no job SHALL have `id-token: write`. It SHALL run no build and no JavaScript install, and from the tagged checkout SHALL execute only `scripts/crates.sh repack` and `scripts/crates.sh publish`, read from its own fresh checkout and never from the artifact. Only the publish step SHALL see the token.

#### Scenario: The script in the artifact
- **WHEN** the job `publish` runs the publish script
- **THEN** it runs the file of its own checkout of the tagged commit, never a file the job `prepare` produced

### Requirement: The publishing job closes cargo's other ways to run tree code
The job `publish` SHALL refuse a checked-in cargo configuration with an inline step before any cargo command, and SHALL set `RUSTUP_TOOLCHAIN` so that no toolchain file of the tree selects the toolchain.

#### Scenario: A toolchain or cargo configuration in the tagged tree
- **WHEN** the tagged tree holds a `.cargo/config.toml` in any directory
- **THEN** the job `publish` fails in its inline tree check before any cargo command, and a `rust-toolchain.toml` of the tree is not used because the job sets `RUSTUP_TOOLCHAIN`

### Requirement: The publishing job checks what it was handed before the credential
Before its checkout, `publish` SHALL check the artifact with an inline step of the workflow: the exact file set, every checksum, each crate's name and version, the plan format and a dashboard without links. It SHALL then repack the publishing crates without building and require the recorded checksums before the credential is acquired, and SHALL publish with verification disabled.

#### Scenario: The handed-over packages do not match their checksums
- **WHEN** a `.crate` file of the artifact has a SHA-256 other than its line in `SHA256SUMS`, or the artifact holds a file it should not
- **THEN** the job `publish` fails in its inline check, before its checkout and before the credential is acquired

#### Scenario: The repacked packages differ from the verified ones
- **WHEN** a package repacked in the job `publish` has a SHA-256 other than the one `prepare` recorded
- **THEN** the job fails before the credential is acquired, naming the crate and both checksums

#### Scenario: The published bytes
- **WHEN** the publish step has uploaded the packages
- **THEN** the index checksum of every crate equals the checksum `prepare` recorded, or the job fails naming the crate and both checksums

#### Scenario: A token reaches a command that builds or packs
- **WHEN** `scripts/crates.sh check`, `preflight`, `repack` or `installed` starts with a registry token in its environment
- **THEN** it refuses before any cargo or network call

### Requirement: Publication is ordered and safe to rerun
Publication SHALL publish the seven crates in dependency order, CLI last as `agentic-playbooks`, SHALL refuse a version other than the workspace version, and SHALL skip a version already in the index only when its index checksum equals the verified package's checksum.

#### Scenario: A failure half way
- **WHEN** a run published three crates and stopped on the fourth
- **AND** the run is started again
- **THEN** the first three are skipped because their index checksums equal the verified ones, and publication continues with the fourth

#### Scenario: A rate limit on new crates
- **WHEN** crates.io refuses a publication because of its rate limit
- **THEN** the step stops with a message saying to rerun all jobs later and that crates already published with the verified checksums are skipped

#### Scenario: Every crate is already published
- **WHEN** all seven index checksums equal the verified checksums, for example on a rerun after publication succeeded and the install check failed
- **THEN** `preflight` reports nothing to publish, the job `publish` is skipped, no cargo command packs or publishes anything, and the job `installed` runs

#### Scenario: Some crates are already published
- **WHEN** the plan marks some crates `skip` and others `publish`
- **THEN** `repack` and `publish` pass one `--exclude` per skipped crate and pack and publish only the others

#### Scenario: No empty selection reaches cargo
- **WHEN** `repack` or `publish` would pass cargo an exclusion for every member
- **THEN** it runs no cargo command: an all-skipped plan takes its all-skipped path, and any other path stops with `internal error: empty package selection`

#### Scenario: A tag that does not match the workspace
- **WHEN** the tag is `v1.0.1` and the workspace version is `1.0.0`
- **THEN** the step fails before publishing anything

### Requirement: Publication never accepts bytes or names that are not ours
A version already in the index with a checksum other than the verified package's SHALL fail the run naming the crate and both checksums. A name that exists on crates.io SHALL count as ours only when the crates.io API answers exactly that name and lists the repository owner's account as an owner; otherwise the run SHALL stop before publishing anything.

#### Scenario: A version already published with other bytes
- **WHEN** the index lists the version of a crate with a checksum other than the verified package's
- **THEN** the run fails before the credential is acquired with a message that names the crate and both checksums

#### Scenario: A name someone else owns
- **WHEN** a crate name already exists on crates.io under another spelling or without the repository owner among its owners
- **THEN** the run stops before publishing anything, naming the crate and its owners

### Requirement: The published version installs from the registry
After publication, or at once when every crate was already published with the verified checksums, a job without any credential SHALL install the published version from crates.io with `cargo install --locked` into an empty directory and SHALL fail unless `apb --version` prints `apb <version>`.

#### Scenario: A failed publication
- **WHEN** the job `publish` failed or was cancelled
- **THEN** the job `installed` does not run

#### Scenario: A release that does not install
- **WHEN** the install from the registry fails or prints another version
- **THEN** the job fails and the owner yanks the published versions

### Requirement: Pre-releases are published
A pre-release tag SHALL be published like a release.

#### Scenario: The release candidate
- **WHEN** the Release run of `v1.0.0-rc.1` succeeded
- **THEN** the seven crates are published at `1.0.0-rc.1`, and `cargo install agentic-playbooks` without `--version` still does not choose it

### Requirement: Publication uses the environment's token and fails closed without it
Every job of the workflow SHALL hold exactly `contents: read`, and no job SHALL have the `id-token` permission or use a token exchange action; the workflow's top-level permissions SHALL be empty. The job `publish` SHALL run in the environment `crates-io` and publish with that environment's secret `CARGO_REGISTRY_TOKEN`, seen only by its publish step. When that secret is absent or empty, the publish step SHALL fail with a message naming the missing secret before any cargo command runs.

#### Scenario: The environment holds the token
- **WHEN** the environment `crates-io` holds a non-empty `CARGO_REGISTRY_TOKEN`
- **THEN** the publish step publishes with it

#### Scenario: The token is missing or empty
- **WHEN** the environment holds no `CARGO_REGISTRY_TOKEN`, or an empty one
- **THEN** the publish step fails with an error naming the missing secret, cargo does not run, and nothing is published

#### Scenario: Who can request an identity token
- **WHEN** any step of any job of `publish-crates` runs
- **THEN** its job has no `id-token` permission, so no identity token can be requested there
