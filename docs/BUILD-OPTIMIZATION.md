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

## Future options (not adopted yet)

- **cargo-nextest**: faster test execution and better reports, but it runs
  each test in its own process. Engine tests currently serialize env-var
  mutation through an in-process mutex (`ENV_LOCK`); migrating requires
  nextest test groups before it is safe. Revisit if test wall-clock
  becomes the bottleneck again.
- **Consolidating integration test files**: merging the many per-file test
  binaries into a few per crate (files become modules) removes most link
  jobs permanently. Worth doing as a dedicated refactor, not piecemeal.
