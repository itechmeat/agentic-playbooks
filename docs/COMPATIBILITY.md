# Compatibility

This document states what apb 1.x keeps stable, what it deprecates, which
platforms it supports, what the compatibility freeze does not cover, how
the freeze is guarded, and how to extend it. The frozen fixtures and the
tests that guard them live beside the code they pin; this page only
summarizes.

## What 1.x keeps

1. The playbook schema (schema 2). A definition that parses and validates
   on 1.0 parses on every 1.x with the same error codes. Warnings may come
   and go. A field may be added, never removed or renamed.
2. The run journal. Every 1.0 event type and field keeps its name and
   shape. A new field is optional (`#[serde(default)]`, see
   [decision record 0001](adr/0001-new-events-are-safe-to-skip.md)) and
   adds no member at its default. A new event type is safe to skip up to
   the next checkpoint, and the checkpoints stay checkpoints (an event may
   become a checkpoint). The order of members in a line is not part of
   the promise.
3. The MCP tools. A tool and an argument keep their names. Every input a
   1.0 tool accepts stays accepted: its bounds, integer widths and
   enumerated values only widen. No tool or argument object gains a
   required member. A tool served read-only stays served. New tools and
   optional arguments may come.
4. The validator codes. A code keeps its meaning and its severity. A
   retired code keeps its number (`RETIRED_CODES`, V15 and V16 among
   them) and a line of the frozen list is never removed.
5. Machine names and files. The reason, refusal and guard codes keep
   their spelling. A run's `manifest.yaml` and `run.yaml`, profile files,
   connector manifests and account files of both scopes, `policy.yaml`,
   `config.yaml`, `decisions.yaml`, start rule documents and the signed
   stores written by 1.0 still read with the same meaning. The result
   keys of every documented result variant of the tools a host is told to
   consume stay, with the result's envelope (a JSON payload or an error)
   and the machine code of each documented refusal.
6. The playbook store. The versions of a playbook (each with the
   derived diagram beside its YAML), the `current` pointer with its
   history, the `candidate` pointer, the provenance sidecar
   (`created_by` included), the trust store and the launch floor store
   written by 1.0 still read with the same meaning.
7. The install channels. The crate `agentic-playbooks` on crates.io
   installs the binary `apb` (`cargo install --locked agentic-playbooks`).
   The shell installer `apb-installer.sh` at
   `https://github.com/itechmeat/agentic-playbooks/releases/latest/download/apb-installer.sh`,
   the Homebrew formula `apb`, the archives `apb-<target>.tar.xz` and
   `apb self-update` of an installer install keep their names.

## Deprecation

A name being replaced keeps working through 1.x with a warning naming its
replacement. Removal waits for 2.0.

## Platforms

Linux and macOS are supported. Isolation level `full` needs Linux with
unprivileged bubblewrap; on macOS `full` refuses with `os_unsupported`
and `best_effort` runs without a sandbox, for the whole 1.x line
(see `docs/ISOLATION.md`). Windows is not supported.

## Not covered

The compatibility freeze does not cover: scenario files, eval cases and
suites; the dashboard's HTTP routes and payloads (also where they share
code with MCP); the CLI's text and JSON output; `diagnostics.jsonl`; the
generated TypeScript; the Rust API of the crates, including the
six `agentic-playbooks-*` library crates on crates.io, which are internal
to the `apb` command (any item may change in any release, and the crates
may be merged or retired); the minimum Rust version of `cargo install`
(the `rust-version` of the release, checked by the CI job `msrv`), which
may rise in a minor release; the native build tools a cargo install needs
(a C compiler and linker); the results of MCP
tools the host integration does not list, and result variants no document
names; the values (not the keys) of a frozen result other than the
machine codes; files signed by other means than `SignedDoc` (grade files,
the discovery result store, remembered run input); the workspace
registry `<config_dir>/projects.json` (the `projects_list` answer names
the registered workspaces, but no document freezes its file, and no
store of a foreign writer reads it); the intent file of an
interrupted `current` move; the per-incarnation trigger journal files and
the listener heartbeat `<config_dir>/trigger-listener.json`; the trigger
journal's decision records, where `trigger_test` is not a tool a host is
told to read.

## How it is guarded

Each frozen area has a fixture written through the production code and a
reader test that asserts its meaning, not only that it parses:

1. `crates/apb-core/tests/fixtures/schema-1.0/`: the validator corpus,
   the field fixtures, the coverage inventory, the witnesses and the
   frozen code list, read by `crates/apb-core/tests/suite/schema_freeze_test.rs`.
2. `crates/apb-core/tests/fixtures/files-1.0/`: profiles, connector
   manifests, connector accounts, settings and start rule documents, read
   by `crates/apb-core/tests/suite/files_freeze_test.rs`.
3. `crates/apb-engine/tests/fixtures/journal/every_event.jsonl`: one line
   per event type, read by
   `crates/apb-engine/tests/suite/journal_compat_test.rs`.
4. `crates/apb-engine/tests/fixtures/run-files-1.0/` and
   `crates/apb-engine/tests/fixtures/signed-1.0/`: the run documents
   (`manifest.yaml`, `run.yaml`, and the journal lines of a run's start
   decisions: its budget, notification subscription, deadline and
   concurrency key) and the signed stores, read by
   `crates/apb-engine/tests/suite/persistence_freeze_test.rs`.
5. `crates/apb-mcp/tests/fixtures/`: the frozen tool schemas (with an
   accepted value of every union alternative and both edges of every
   numeric argument), the machine codes (a scan of the sources that fails
   on a code it cannot resolve) and the documented result contracts (the
   keys, the envelope and the refusal code of the accepted result and of
   each documented refusal), read by
   `crates/apb-mcp/src/server/freeze_tests.rs`.
6. `crates/apb-core/tests/fixtures/store-1.0/`: the playbook store (the
   version files with their diagrams, `current` with its history, a
   `candidate`, the provenance sidecars, the lifecycle file, the trust
   store and the launch floors), read by
   `crates/apb-core/tests/suite/store_freeze_test.rs`.

The reader tests of the person files, the run documents, the signed
stores and the playbook store (items 2, 4 and 6) run each document
through its production reader, check that the reader's result keeps
every key the document holds, and run the document's hand-written
expectation of what the reader decides from it.

The released binary corpus, `crates/apb-cli/tests/suite/journal_corpus_test.rs`,
runs released binaries (`v0.25.0`, `v1.0.0-beta.1` and the freeze
reader `v1.0.0-beta.7`) and this one against each other's run journals:
each writes a run the other reads, resumes and stops. It covers the
journal only; the other frozen files are checked against the fixtures
above, not against files an earlier release wrote. The old binaries come
from `APB_OLD_BINARIES` (built by `scripts/old-apb.sh`); the CI job
`journal-corpus` names its tags in `OLD_APB_TAGS` and fails on a missing
binary, while a local run without them skips each case.

## Extending the freeze

Every frozen group except the journal has one writer test, run with the
switch set:

```sh
APB_WRITE_FREEZE=1 cargo test -p agentic-playbooks-core --test main schema_freeze_test::write_freeze
APB_WRITE_FREEZE=1 cargo test -p agentic-playbooks-core --test main files_freeze_test::write_freeze
APB_WRITE_FREEZE=1 cargo test -p agentic-playbooks-engine --test main persistence_freeze_test::write_freeze
APB_WRITE_FREEZE=1 cargo test -p agentic-playbooks-mcp --lib server::freeze_tests::write_freeze
APB_WRITE_FREEZE=1 cargo test -p agentic-playbooks-core --test main store_freeze_test::write_freeze
```

The switch only adds: a frozen line, key or file is never removed, and a
frozen value is never rewritten. The one exception is machine state a
writer replaces with a fixed value, so a fixture is the same on every
machine: a temporary project path, a file fingerprint, a wall-clock time
and a random change id (each writer names the values it replaces). A
failing freeze test is a compatibility break to fix, never a fixture to
regenerate.

The journal fixture has no writer. A new event type gets one line in
`every_event.jsonl`, written by hand: build the smallest value of the
variant with every optional field set in a temporary unit test that
prints `serde_json::to_string(&Event { seq, ts, payload })`, paste the
line in variant declaration order, and delete the temporary test (see
`crates/apb-engine/tests/suite/journal_compat_test.rs`).

A new tool, argument, event type, code or guard name joins the freeze in
the change that adds it: run the group's writer (or write the journal
line), review the diff, write the expectation, and record what the new
names mean. A machine code that reaches an MCP result through a variable
or a call is listed by hand, with the codes its producer can return, in
`SCANNED_EXPRESSIONS` of `freeze_tests.rs`.
