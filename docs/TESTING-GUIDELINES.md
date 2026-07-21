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

## Every wait is bounded, and says what it waited for

This is the rule that cost us the most: a single unbounded wait in a test
helper hung CI for 30 minutes and, because the job had no ceiling, would
have burned six hours. Chasing it found four of the same defect in the
product itself. Treat an unbounded wait as a defect wherever it appears.

- `child.wait()`, `wait_with_output()`, `JoinHandle::join()`, channel
  `recv()`, and any read on a pipe are all unbounded by default. Use a
  deadline: a `try_wait` loop with a cap, `recv_timeout`, a join with a
  deadline. On expiry, fail with a message that NAMES the thing waited on
  ("timed out after 10s waiting for the driver pid to stop reading as
  alive"), never a bare timeout.
- **EOF is not proof that a process exited.** A pipe reaches EOF when the
  last write end closes, and any descendant that inherited the descriptor
  holds one. An agent that leaves a daemonized helper keeps its parent's
  stdout open forever after exiting. Wait on the process, not on the pipe,
  and treat EOF as one signal among several.
- A bound must not turn a hang into a wrong answer. If the work already
  produced a result, return the result; if output was lost, say so on a
  channel someone reads. Silently substituting an empty value while
  reporting success is worse than the hang, because nothing surfaces it.
- Bound a test BY CONSTRUCTION rather than by widening a timeout. If a
  test can only pass when the machine is fast, it asserts the machine.
- Build RAII reapers and guards BEFORE the first thing that can panic. A
  cleanup line after an assertion does not run when the assertion fails,
  and a leaked `sleep 300` outlives the tempdir it was pointed at.

## Processes, pipes, and signals

- Never shell out for signalling or liveness. `kill -9 -<pgid>` is accepted
  by BSD `kill` (macOS) and rejected as a bad option by procps-ng (Linux),
  and the divergence is invisible when the `ExitStatus` is discarded. Use
  the syscall through `libc` and read `errno`.
- **Validate any pid before it becomes a signal target.** In POSIX,
  `kill(-1, sig)` is not an invalid argument, it is the wildcard "signal
  every process the caller may signal", and `kill(0, sig)` hits the
  caller's own group. So `u32::MAX` narrows to `-1`, and a group kill that
  negates pid 1 also produces `-1`. The single-pid form requires `> 0`; the
  group form negates and therefore requires `> 1`. Never signal a pid that
  came from a file without validating it first; prefer a pid taken from a
  `Child` handle you own.
- Test the wiring, not just the helper. A pure function that computes a
  signal target proves nothing if the caller can bypass it: both stay green
  when someone restores the raw expression. Inject the effectful call (see
  `PsRun` in `liveness.rs` and `kill_group_with` in `proc.rs`) so a test
  asserts the exact value handed to the syscall with zero signals sent.
- Choose fixture pids that are plausible but absent: spawn, wait, reap, and
  use that number. Impossible values such as `u32::MAX` take a different
  code path in the kernel and in `ps`, so a test built on one is not
  testing what its name claims.
- A process spawned by a test is the test's to reap on every path,
  including the failing one. Detached children need their own group and
  null stdio, or they keep the harness alive after the test ends.

## Platform divergence: local green proves little

Three defects on the run-reliability branch existed only on Linux and were
invisible on macOS: a clippy lint that only the newer CI toolchain raises,
the `kill -9 -<pgid>` form above, and the pid wildcard. Two more were
timing-shaped: a run that finishes before a stop lands on a fast runner.

- When a test's subject is a syscall, a signal, a process relationship, or
  anything the shell mediates, assume the platforms differ until CI says
  otherwise. Say so in the test comment when you could not verify locally.
- Do not encode a race as an assumption. Assert the premise the test needs
  (the run must still be running when the stop lands) so a collapsed
  premise fails loudly instead of masquerading as the outcome under test.
- Never claim in a module doc that every test in the file fails against
  the pre-fix code unless you checked each one. An honest tally ("five of
  seven hang; one is platform-indistinguishable here; one passes both
  ways") is worth more than a confident wrong one.

## Time and flakiness

- Test self-time here is small; keep it that way. A real `sleep`/timeout
  belongs only in tests whose SUBJECT is timing (watch debounce, heartbeat
  loss). Everything else polls with a short interval and a generous
  deadline, or uses an event it can await.
- A stub that sleeps 30 seconds so a test can assert "the call returned
  sooner than that" costs 30 seconds of everyone's life on every run. Pick
  the shortest duration the assertion still distinguishes, and make it
  configurable when the honest value differs between local and CI.
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
  iterating; one full run before a task-closing commit. One cargo
  invocation at a time. Details and the reasoning are in
  `docs/BUILD-OPTIMIZATION.md`.
- To run one module of a suite: `cargo test -p apb-engine --test main
  <module_name>::` (module path prefixes filter test names).
- `cargo nextest run --workspace` runs the same selection with a per-test
  ceiling (`.config/nextest.toml`), which is what CI uses. Prefer it before
  a PR: it is the only local run that can catch a hang as a named failure.
  Note the two runners differ in isolation, and both must pass: `cargo
  test` shares one process per binary, so process-global state races
  between threads, while nextest gives each test its own process. A test
  that only passes under nextest is relying on that isolation and will
  break the plain run, so keep the shared lock and the Drop guards.
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
5. Every wait bounded, every message naming what it waited for; RAII
   cleanup constructed before the first thing that can panic.
6. `cargo test -p <crate>` green, then fmt and clippy gates.

## Before opening a PR

Run these and fix what they show. Each one corresponds to a defect that
already reached CI or a release.

1. `cargo nextest run --workspace` green with no test reported SLOW. A test
   the profile calls slow is on its way to being a hang.
2. `rg -n 'wait_with_output\(\)|\.join\(\)|\.recv\(\)|read_to_string' crates/*/src crates/*/tests`
   over your diff: every hit either has a deadline or a comment saying why
   it cannot hang.
3. `rg -n 'libc::kill|killpg|\-\(.*as i32\)|Command::new\("kill"\)' crates`
   over your diff: no new shelled-out signal, and every signal target
   validated. Signalling a pid read from a file needs a validator, not a
   comment.
4. `cargo test --workspace` green as well, since the plain runner is the
   one that exposes shared-state races between tests.
5. `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --
   -D warnings`, and `code-ranker check .` clean. CI runs a newer Rust than
   most local toolchains, so a clean local clippy is necessary, not
   sufficient.
6. Any test whose subject is a syscall, signal, process relationship, or
   race says in a comment what could not be verified on the local platform.
