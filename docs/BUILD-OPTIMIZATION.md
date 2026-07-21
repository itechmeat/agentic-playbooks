# Build and test gate optimization

Rules for keeping Rust build and test cycles cheap during iterative,
agent-driven development. They apply to humans and coding agents alike.
Full quality gates still exist; the point is WHEN each gate runs, not
whether it runs.

## Why this matters

The workspace chains apb-core <- apb-engine <- apb-mcp <- apb-cli/server,
so any change low in the chain invalidates almost every crate. There are
about a hundred integration test files, and each one is a separate binary
that must be compiled and linked. A single `cargo test --workspace` after
a core change therefore rebuilds and relinks nearly everything, saturating
every CPU core. Repeating that for every small task multiplies the cost
without adding coverage: cargo's dependency graph guarantees that a scoped
run tests exactly what a change can affect.

## Rules

### 1. Scope gates to the change while iterating

During a task, run tests only for the crates the change can affect: the
crate you touched plus its dependents.

- Touched `apb-server` or `apb-cli` only: `cargo test -p <that-crate>`.
- Touched `apb-mcp`: `cargo test -p apb-mcp` (nothing depends on it except
  the cli/server binaries; add them only when their code consumed the change).
- Touched `apb-engine`: `cargo test -p apb-engine -p apb-mcp -p apb-server`.
- Touched `apb-core`: scoped runs still help while iterating (fail fast on
  the changed crate first), but finish with the full workspace run below.

Clippy follows the same scoping while iterating:
`cargo clippy -p <crate> --all-targets -- -D warnings`.

### 2. Full gates at milestones, not per micro-step

`cargo test --workspace` and
`cargo clippy --workspace --all-targets -- -D warnings` run:

- at the end of a task that touched `apb-core` or `apb-engine`,
- at part/phase boundaries of a larger plan,
- before every commit that concludes a task,
- always before a release together with `cargo clippy --release` and
  `code-ranker check .` (see CLAUDE.md gates, which stay authoritative).

### 3. Never run a redundant `cargo build`

`cargo test` builds everything it needs. A separate
`cargo build --workspace` before or after it duplicates a full compile
pass. Use `cargo build` only when you need the binaries themselves or a
compile-only signal without running tests (`cargo check` is cheaper still).

### 4. One cargo invocation at a time

Never start a cargo command in the background and then run another one:
two workspace builds compete for cores and memory, and the engine's
timing-sensitive tests (scheduler, process-group, detect probes) start
flaking under saturation. Every cargo command runs in the foreground,
sequentially. This also applies across agents: one implementer at a time.

### 5. Keep dev debug info light

`Cargo.toml` sets `[profile.dev] debug = "line-tables-only"`. Backtraces
stay readable (file and line), but the linker no longer packages full
debug info into every test binary, which is the single most expensive
phase of the per-task cycle. Do not raise it back to `true` for the
workspace; if a debugging session needs full info for one crate, override
locally with `[profile.dev.package.<crate>] debug = true` and drop it
before committing.

### 6. Leave headroom on shared machines

On a developer machine (or a VPS also hosting the agent itself), cap
parallelism locally so builds do not starve everything else: set
`CARGO_BUILD_JOBS` to about two thirds of the logical cores (or pass
`--jobs N`). Do not commit a hard cap into `.cargo/config.toml`: the right
number is machine-specific and CI should keep its own defaults.

### 7. Linux hosts: use a fast linker

On Linux (VPS, CI) install `mold` and wire it in the local (uncommitted)
`~/.cargo/config.toml`:

```toml
[target.x86_64-unknown-linux-gnu]
linker = "clang"
rustflags = ["-C", "link-arg=-fuse-ld=mold"]
```

Linking a hundred test binaries is where most wall-clock goes; mold cuts
that several-fold. On macOS the system linker is already reasonable.

### 8. One integration-test binary per crate

Cargo compiles and links every file directly under `crates/<c>/tests/*.rs`
as its own binary. Each fresh binary also pays a per-launch cost on macOS
(security scan). So integration tests live as modules of a single binary
per crate, never as loose `tests/*.rs` files:

```rust
// crates/<c>/tests/main.rs  (the only file directly under tests/)
#[path = "suite/foo.rs"] mod foo;   // former tests/foo.rs, now tests/suite/foo.rs
#[cfg(unix)]
#[path = "suite/bar.rs"] mod bar;   // outer #[cfg], never a #![cfg] inside the module
mod common;                          // shared helpers + the ONE env lock
```

Rules when adding or moving a test:
- Put the file under `tests/suite/` and add one `mod` line to `tests/main.rs`.
  Do not create a new `tests/*.rs` at the top level.
- All modules share ONE process, so tests run as parallel threads across
  modules. Every test that mutates process-global state (env vars via
  `set_var`/`remove_var`, `set_current_dir`, PATH) must acquire the single
  shared lock from `tests/suite/common` and restore the original value with
  a `Drop` guard (so a panic cannot leak state into a sibling module). Do
  not add a second per-file lock. Sync tests use `std::sync::Mutex`; async
  tests holding the guard across `.await` use `tokio::sync::Mutex`.
- `include_str!`/`include!` paths are relative to the source file: a file
  under `tests/suite/` needs one extra `../` versus `tests/`.

### 9. CI runs nextest, and every job has a ceiling

cargo-nextest was previously listed here as not adopted, on the grounds
that a spawn per test costs more than a spawn per binary. That trade was
re-decided when a single unbounded wait in a test helper hung CI: with
`cargo test`, one stuck test takes its whole binary down with it and the
job sits there reporting nothing about which test is stuck, until the
workflow ceiling or a human intervenes. Ours had no ceiling, so the
default was 360 minutes. A per-test deadline is worth the spawn cost.

What is in place now:

- `.config/nextest.toml` defines two profiles. `default` (local) allows
  120s per test, which clears the slowest honest case on macOS: the
  FSEvents watcher registration costs about 34s under `/var/folders`,
  which the Linux inotify path does not pay. `ci` allows 180s, a wide
  margin over the real worst case of about 11s. Both work by
  `slow-timeout` marking a test SLOW and `terminate-after` ending it after
  N such periods, SIGTERM first and then SIGKILL, failing it BY NAME.
- Retries are off everywhere. A hang that a retry papers over is a defect
  that should be visible on the first run.
- `ci.yml` and `release.yml` both run `cargo nextest run --profile ci
  --workspace` plus `cargo test --workspace --doc`, because nextest does
  not run doctests. Coverage matches the old `cargo test --workspace`:
  same selection, and `#[ignore]`d tests stay ignored.
- Every job in both workflows carries `timeout-minutes` (30 for test jobs,
  45 per release build leg). Reaching a job ceiling now means something
  outside the tests is wrong, since a hung test is caught earlier and by
  name.

Consequences for how tests are written:

- Locally, both runners must pass. `cargo test` shares one process per
  binary, so the shared `ENV_LOCK` and the Drop guards from rule 8 are
  still mandatory. nextest gives each test its own process, which hides
  exactly those bugs, so a green nextest run is not evidence that
  process-global state is handled correctly.
- Prefer `cargo nextest run --workspace` for the run before a PR: it is
  the only local invocation that turns a hang into a named failure rather
  than a stalled terminal.

### 10. Wall-clock is a shared cost, and hangs are the worst case

Numbers from the run-reliability branch, for calibration: the full test
run is about 25s on CI (1039 tests) and the whole pipeline about 2m30s.
Before the fix, one hung test cost 30 minutes before a human cancelled it,
against a 6-hour ceiling.

- A stub sleeping 30s so a test can assert "it returned sooner" costs that
  30s on every run for everyone. Choose the shortest duration that still
  distinguishes pass from fail.
- Do not split the CI job to parallelize unless the split is genuinely
  cheap. Here it is not: `web/dist` is gitignored and rust-embed depends
  on it, so every cargo step needs the bun build first, and splitting
  would duplicate the whole pipeline for no gain.
- The macOS binary-spawn cost from rule 8 still argues for one integration
  binary per crate. nextest changes the per-test cost, not the per-binary
  compile and link cost, which is the part that dominates a rebuild.
