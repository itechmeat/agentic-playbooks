# Proposal: one point oh freeze

## Why

apb 1.0 promises that a playbook, a run journal, an MCP host and a
validator code that work on 1.0 keep working through 1.x. Nothing on main
checks that promise, and no document states it.

1. The playbook schema has no freeze. `Playbook` does not deny unknown
   fields, so a top-level field renamed or removed in a later 1.x parses
   in silence and the definition runs without it. No test parses a frozen
   copy of the examples or the bundled playbooks; the tests that read
   `examples/playbooks/` read the live files, which change with the code.
2. The journal has no full freeze. `known_events.jsonl` holds one
   coherent run of 34 lines and 26 event types, while `EventPayload` has
   100 variants: a changed field name of most of them is not caught. The
   released binary corpus reads with v1.0.0-beta.1 and v0.25.0 only.
3. MCP tool names and arguments have no freeze. The read-only tables
   prove every tool is classified, not that a tool, an argument or the
   input it accepts survives, nor that the result keys a host reads stay.
4. Validator codes have no freeze. A retired code is kept by a comment
   only, and no registry says which codes exist and at what severity.
5. The machine codes a host branches on (reason, refusal and guard
   names), a run's manifest and configuration, the files a person keeps
   (profiles, connector manifests and account files, `policy.yaml`, start
   rule documents), the signed stores and the playbook store (versions,
   `current` with its history, the provenance sidecar, the trust store,
   the launch floors) have no freeze either.
6. No document states the 1.x promise; the platform position for macOS
   lives in one paragraph of `docs/ISOLATION.md`, and the fit report and
   `apb doctor` word it on their own.

This change is the last one before the release candidate. It is built
after every other change of the 1.0 plan is merged (people-in-the-loop,
before-running-alone, typed-inbox-reads, wait-park, eval-doubles,
secret-params-in-run-files, mcp-tool-connectors, one-point-oh-fixes,
mcp-http-and-surfaces, incremental-lists-and-session-fork, start-rules,
inbox-waits, templates-and-global-playbooks), because its fixtures are
generated from that tree and nothing may be added to the frozen surface
after them without joining the freeze.

| Decision | Code example | Non-code example |
|---|---|---|
| D1 freeze | a 1.3 apb still parses a 1.0 `dependency-security-fix` definition with its `baseline_check` and validates it to the same error codes | a CRM routine's journal written by 1.0 resumes on 1.4 after a quota pause |
| D3 platform | on a Mac, `isolation: full` refuses `os_unsupported` with the 1.x sentence | the same `best_effort` mail triage runs on a Mac without a sandbox and says so |
| D5 release candidate | 1.0.0-rc.1 is published from this PR | the soak runs non-code routines on rc.1 |

## What changes

1. Compatibility freeze (capability `compatibility-freeze`): a frozen
   schema corpus with its frozen error codes (warnings recorded, not
   asserted), a coverage inventory of every schema place checked from the
   types (struct fields, enum variants and their fields, flattened
   fields) and a key retention rule at exact paths; a journal fixture
   with one line per event type, compared as parsed values, with frozen
   checkpoints that stay checkpoints; a frozen MCP tool list with each
   tool's normalized input schema (bounds, integer widths and enumerated
   values included), union examples, boundary values sent through each
   tool's argument type and read-only class, and the result keys of every
   documented result variant of the tools a host consumes; a validator
   code registry (`CODES`, `RETIRED_CODES` with V15 and V16) proven
   against the sources, every active code with an executed witness;
   frozen machine codes listed from their production constants and
   functions, with the scan as the completeness check, and run guard
   names; frozen run files, person-kept files (account files of both
   scopes), signed stores and the playbook store, each with nondefault
   state and named typed values asserted; one writer per fixture group
   behind `APB_WRITE_FREEZE=1` that only adds and refuses any removal; a
   new reader row pair in the released binary corpus for the newest
   published beta; `docs/COMPATIBILITY.md`.
2. Platform position (capability `platform-position`): the 1.x sentence
   of `os_unsupported`, shown by the fit report and `apb doctor` too, and
   the position in `docs/COMPATIBILITY.md`.
3. Release candidate (capability `release-candidate`): version
   1.0.0-rc.1, `docs/release-notes/v1.0.0-rc.1.md` with the lines of
   every change since the last beta, a beta install for which `apb
   self-update --check` reports the candidate, and the soak checklist in
   the release notes with the real update as a step a person performs
   and records.

## Out of scope

1. Global playbooks from the CLI and the dashboard and the template
   gallery: the change templates-and-global-playbooks, merged before this
   one.
2. A macOS sandbox: `design.md` "Decisions pending the owner".
3. A `v1.0.0-rc.1` reader in the released binary corpus: rc.1 is the tag
   this PR produces, so no CI run of this PR can download it. This change
   adds the newest published beta as a reader; rc.1 and 1.0.0 join as
   readers in release-1.0.0.
4. Verifying the no-prompt git transport on a macOS desktop session in
   CI: it is a soak checklist line.
5. Freezing the CLI (its text and JSON output, `apb templates`, `apb
   trigger`, `apb run --scope`), the dashboard's HTTP API, scenario files,
   eval cases and suites, `diagnostics.jsonl`, the generated TypeScript,
   the Rust API of the crates, the trigger journal files and the listener
   heartbeat:
   `docs/COMPATIBILITY.md` says they are not part of the 1.x promise
   (`design.md` D1.5, D1.6).
6. Any new feature.

## Impact

1. Crates: `apb-core` (the registry `validate/codes.rs`; the
   `os_unsupported` sentence; freeze tests), `apb-engine` (the fit item
   and the doctor line; freeze tests; two carried test fixes), `apb-mcp`
   (freeze tests), `apb` CLI (the self-update tests, the corpus rows; the
   version), `web` (the regenerated `consts.gen.ts`).
2. Journal: no new event and no new field. Validator: no new code;
   `RETIRED_CODES` lists every retired code, V15 (never emitted) and V16.
   MCP: no new tool.
3. Behaviour changes for the release notes: the macOS refusal sentence
   states the 1.x position; the 1.x promise is written in
   `docs/COMPATIBILITY.md` and guarded by tests; version 1.0.0-rc.1.
4. The freeze fixtures are generated from merged main, never from this
   document.
5. Size: 60 changed files (`design.md` "Files"), under the owner's
   limit of about 80.
6. Two carried test fixes ride along (`design.md` "Carried fixes"): the
   loaded-runner timeout of an inbox wait test and the macOS flake of an
   OAuth consent test.

## Open questions

None. The owner decided that 1.x reads a 1.0 playbook store (`design.md`
D1.5 item 6) and that V15 is listed retired; the trigger journal codes
and the dry test values stay outside the promise under the result-value
rule (`design.md` D1.6 item 4).
