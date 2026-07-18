# Testing guidelines for this Rust workspace

How to write, place, and run tests here, and the traps that already cost us
real debugging time. Build and gate mechanics live in
`docs/BUILD-OPTIMIZATION.md`; this document is about the tests themselves.

## Layout: one integration binary per crate

Cargo turns every file directly under `crates/<c>/tests/*.rs` into a
separate binary that is compiled, linked, and spawned on its own. Binary
count, not test count, dominated our wall-clock (each fresh binary also
pays a first-spawn security scan on macOS), so the layout is fixed:

- `tests/main.rs` is the ONLY file directly under `tests/`. It declares one
  module per test file and the shared `common` module.
- Test files live under `tests/suite/`, included from `main.rs` via
  `#[path = "suite/<name>.rs"] mod <name>;`.
- Platform gating goes on the module line as an outer attribute
  (`#[cfg(unix)] mod foo;`). A crate-level inner `#![cfg(...)]` inside an
  included module does not compile.

DO NOT add a new `tests/<name>.rs` next to `main.rs`. It silently becomes a
new binary and reintroduces the cost this layout removed.

## Shared process, shared state: the rules that matter most

All modules of a crate's suite run in ONE process, and test functions run
as parallel threads across modules. Anything process-global is shared:
environment variables, the current working directory, PATH, statics.
Process isolation no longer hides sloppy state handling - consolidation
immediately exposed four real bugs of exactly this class (a PATH clobber
that broke every later test, config-dir leaks that failed an unrelated
suite, a probe reading PATH before taking the lock).

Rules:

- Every test that mutates process-global state acquires the ONE shared lock
  from `tests/suite/common` first. Never add a private per-file lock: two
  locks do not exclude each other.
- Restore the ORIGINAL value with a `Drop` guard captured before the
  mutation (`std::env::var_os` first, then set). Cleanup written at the end
  of the test body does not run on panic and leaks state into the next
  test. Declare the guard AFTER the lock guard, so restoration happens
  while the lock is still held.
- Anything that READS mutable global state (spawning a tool found via PATH,
  probing HOME) must also do so under the lock, or it can observe another
  test's temporary state. Take the lock before the probe, not after.
- Sync tests use the `std::sync::Mutex` lock. Async tests that hold the
  guard across an `.await` need `tokio::sync::Mutex` (clippy
  `await_holding_lock` rejects the std guard there, and it is right).
- Setting env on a CHILD process (`Command::env(...)`) is always safe and
  needs no lock; only mutation of the test runner's own process races.
- Prefer passing paths and config explicitly over env vars at all. The lock
  serializes every env-mutating test in the whole crate; each new env test
  makes the suite more sequential.

## Fixtures and paths

- `include_str!`/`include_bytes!` resolve relative to the SOURCE file. A
  file under `tests/suite/` needs one more `../` than it did under
  `tests/`. When a test moves, re-check every include.
- Runtime file access is relative to the crate root (cargo sets the test
  cwd), and `env!("CARGO_MANIFEST_DIR")` is immune to source moves. Prefer
  these for anything opened at runtime.
- Shared fixture data lives in `tests/fixtures/` (a data directory, not a
  test target). Do not duplicate fixtures per module.

## Time and flakiness

- Test self-time here is small (a couple of minutes for 500+ tests); keep
  it that way. A real `sleep`/timeout belongs only in tests whose SUBJECT
  is timing (watch debounce, heartbeat loss). Everything else polls with a
  short interval and a generous deadline, or uses an event it can await.
- Never calibrate a timing assertion to a loaded machine. Our supervisor
  and process-group tests flaked exactly when the host was saturated by
  parallel builds. If a test needs quiet CPU, it is asserting the machine,
  not the code - restructure it.
- A test that passes alone but fails in the full suite is a shared-state
  bug, not a flake. Fix the state handling; never "fix" it by rerunning or
  by moving it back into its own binary.

## Writing tests

- TDD as in the plans: write the failing test, watch it fail, implement,
  watch it pass. A test that never failed proves nothing.
- Assert behavior, not incidentals: fold the event log and check state, hit
  the API and check the response shape. Do not assert on log text or
  private layout that refactors legitimately change.
- Stub agents (shell scripts registered via the common helpers) are the way
  to test engine execution paths without real agents. Reuse
  `tests/suite/common` helpers; do not hand-roll a new stub pattern.
- Every test carries its own temp dirs (`tempfile::tempdir`); nothing
  touches the developer's real config, HOME, or trust store. If a test
  needs the config dir, it points APB_CONFIG_DIR at a temp dir under the
  shared lock, with a Drop restore.
- Do not weaken an existing assertion to make a new feature pass; that is a
  review finding, not a fix.

## Running tests

- Scoped first, full at milestones: `cargo test -p <crate>` while
  iterating; one full `cargo test --workspace` before a task-closing
  commit. One cargo invocation at a time. Details and the reasoning are in
  `docs/BUILD-OPTIMIZATION.md`.
- To run one module of a suite: `cargo test -p apb-engine --test main
  <module_name>::` (module path prefixes filter test names).
- If the suite hangs, diagnose before killing: find what the process waits
  on (`ps`, current test binary, lock holders). Two concurrent cargo runs
  on one checkout starve each other and flake the timing tests.

## Quick checklist for a new test file

1. File goes to `crates/<c>/tests/suite/<name>.rs`, one `mod` line added to
   `tests/main.rs` (outer `#[cfg]` there if platform-gated).
2. `use crate::common;` for helpers; shared lock + Drop guard for any
   process-global mutation or read.
3. Includes use `../fixtures/...`; runtime paths use the crate root or
   `CARGO_MANIFEST_DIR`.
4. No bare sleeps unless timing IS the subject; poll with a deadline.
5. `cargo test -p <crate>` green, then fmt and clippy gates.
