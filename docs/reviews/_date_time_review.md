# Review of the current working tree against main

Date: 2026-07-29 18:04:40 +08

## Summary

Status: needs further work before merge.

The current checkout is on `main`, and `main...HEAD` is empty. The review was therefore performed against the entire working tree relative to `main`, including 35 changed tracked files and all untracked files. The scope covers suggestion decisions, onboarding standing instructions, connector setup documents, and playbook `apb-task-implement` version 1.12.0.

The architectural check through CodeGraph confirmed the project's core invariant: the MCP must hand verified state to the engine without re-resolving it, state files must be modified atomically under a lock, and user-facing surfaces must not report success when state was lost. The new functionality generally follows the existing separation of core, MCP, CLI, server, and web, but it violates the last two invariants in the places listed below.

## Findings

### High: a lock-acquisition failure turns into a write without mutual exclusion

Files: `crates/apb-core/src/dismiss.rs:242`, `crates/apb-core/src/dismiss.rs:309`, `crates/apb-core/src/dismiss.rs:743`

`lock_dir(...).ok()` discards `WouldBlock` and any IO errors, after which the code proceeds with a read-modify-write as if the lock had been acquired. This directly contradicts the documentation of `with_locked_store`, which promises a single atomic critical section. Under a live concurrent owner, two soft declines can both read one `declines = N` and both write `N + 1`; remove/reset/prune can overwrite a newer record; migration can race with an ordinary write. Atomic rename protects only the integrity of an individual file, not the read-modify-write against a lost update.

Fix: mutation and migration must return an error when the lock is not acquired. A never-fail read may return an unpruned snapshot with a diagnostic, but must not perform a prune-write without the lock. A test is needed that holds a fresh lock in advance and proves there is no write and no lost update.

### High: an unknown store-schema version is accepted and can be silently downgraded to schema 2

Files: `crates/apb-core/src/dismiss.rs:131`, `crates/apb-core/src/dismiss.rs:185`, `crates/apb-core/src/dismiss.rs:251`, `crates/apb-core/src/dismiss.rs:324`

`read_store` deserializes any `schema`, and `StoreFile` and `SuggestionRecord` accept unknown fields. Then prune or any mutation sets `file.schema = 2` and serializes only the fields known to the current version. A future schema-3 file can be read successfully, lose its new fields, and be rewritten as schema 2. This is silent data loss and a dangerous fallback.

Fix: after parsing, explicitly allow only schema 2. Return an unknown version as a separate diagnostic/error and never rewrite it. For mutation, it is better to refuse with a clear error than to quarantine a file supported by a newer version as corrupt. Add a test with `schema: 3` and an extra field that verifies the bytes are unchanged after `active` and after mutation.

### High: global dismiss reports success when there is nowhere to save the record

Files: `crates/apb-core/src/dismiss.rs:584`, `crates/apb-core/src/dismiss.rs:588`, `crates/apb-mcp/src/tools/capture.rs:195`, `crates/apb-mcp/src/tools/capture.rs:207`

When `APB_CONFIG_DIR`, `XDG_CONFIG_HOME`, and `HOME` are all unset, the global `store_dir` returns `None`. In that case `record_decision` builds the record in memory and returns `Ok(DecisionOutcome)`, and the MCP responds with the fields `dismissed`, `scope: global`, and `snoozed_until`. The user and the agent get confirmation of a saved decision, even though no state remains after the call.

Fix: a global mutation without a config dir should fail with `no global config dir`, the way global `playbook_capture` already does. If a config-less read needs to keep working, that does not require a false success for write.

### Medium: an explicit never-again automatically expires after 90 days

Files: `crates/apb-core/src/dismiss.rs:23`, `crates/apb-core/src/dismiss.rs:488`, `crates/apb-mcp/src/instructions.rs:15`, `crates/apb-mcp/src/server/playbook.rs:107`, `web/src/lib/suggestions.ts:69`

Every agent-facing instruction requires using hard only for an explicit never-again, and the dashboard labels the record as `never again`. Yet a hard record gets `HARD_TTL_DAYS`, 90 days by default, after which prune removes the record and the suggestion is allowed again. This behavior violates the user's explicitly stated intent, and the UI hides the temporary nature of the decision.

Fix: pick one consistent contract. For a genuine never-again, the hard record must be indefinite until `allow`. If a TTL is mandatory for backward compatibility, the copy and the label should describe a long snooze until a specific date, not a never-again.

### Medium: allow promises the suggestion will be offered again, even though a second scope may keep suppressing it

Files: `crates/apb-core/src/dismiss.rs:397`, `crates/apb-cli/src/suggestions.rs:107`, `web/src/lib/components/SuggestionsSection.svelte:50`

The same pattern can exist in both the project and the global store. `allow` removes only the selected scope, but the CLI prints `the suggestion can be offered again`, and the dashboard shows a similar success toast. If the record in the second scope remains active, `active()` keeps suppressing after the removal. This is especially visible on the web: `load()` will immediately show the card again, at the same time as the toast about a successful re-enable.

Fix: after the removal, recompute the active view and report the actual result. The core/server response could return `removed` and `still_suppressed_by`. A simpler-contract alternative: the message confirms only that the record was removed from the given scope and makes no promise about re-enabling.

### Medium: the dashboard hides read errors and does not show global records when no project is registered

Files: `web/src/lib/components/SuggestionsSection.svelte:28`, `web/src/lib/components/SuggestionsSection.svelte:33`, `web/src/lib/api/core.ts:216`, `crates/apb-server/src/routes/suggestions.rs:25`, `crates/apb-server/src/state.rs:72`

Every failed `fetchSuggestions` call is turned into an empty array via `.catch(() => [])`, so the dashboard silently shows a partial result or hides the section entirely. The server already returns `diagnostics`, but `fetchSuggestions` discards them. A corrupt store thereby becomes visually indistinguishable from having no decisions at all.

In addition, the UI requests suggestions only inside `fetchProjects().map(...)`. With zero reachable projects there will be no request at all, and the server in global mode requires a workspace even for the global store. As a result, machine-wide global decisions cannot be viewed or removed from the dashboard on exactly the machine that has no registered project.

Fix: keep the response together with its diagnostics and show an error/warning per workspace instead of an empty fallback. Global records need a root-independent endpoint or scope-aware root resolution, similar to `resolve_root_for_scope`, so the global store stays manageable with zero projects.

## SOLID, KISS, DRY

- Separation of responsibilities is good overall: the store and timing live in `apb-core`, the CLI and HTTP handlers remain thin wrappers, and web formatting is factored out into `suggestions.ts`.

- DRY is improved by the shared `print_table` and the shared slug validator.
- `dismiss.rs` has grown to roughly 1500 lines including tests and combines schema IO, migration, locking, timing resolution, merging, time formatting, and command-level mutation. After fixing the correctness findings, it is worth splitting it into at least store IO/migration and decision policy. This will reduce coupling but does not by itself block the merge.
- Named constants and config overrides for backoff/TTL remove the main hardcoding. The hard-TTL problem right now is not where the value is declared, but the conflict between that value and the user-facing never-again contract.
- Falling back to default timing under a broken config is acceptable only because the diagnostic is returned to the caller. A fallback without persistence and a fallback without a lock are not acceptable, because they create a false confirmation of a correct operation.

## Checks

- CodeGraph: index healthy, 593 files, 7916 symbols, 10157 edges. Checked the end-to-end links of schema, profile/trust, RunPermit/manifest, and the new suggestion store -> MCP/CLI/server -> web chain.
- `cargo metadata --format-version 1 >/dev/null`: pass.
- `code-ranker check .`: pass, no ADP/cohesion/complexity/SOLID/DRY/KISS violations found.
- `cargo fmt --all -- --check`: pass.
- `cargo clippy --workspace --all-targets -- -D warnings`: pass. There is an external future-incompatibility warning for `imap-proto v0.10.2`.
- `bun run test`: pass, 30 files and 359 tests.
- `bun run check`: pass, 0 errors and 0 warnings.
- `bun run build`: pass.
- `cargo test --workspace`: all suites that finished passed, including the new core/MCP/CLI/server tests. The run was stopped manually after more than 3 minutes of waiting on two hung watcher tests: `runs_watch_test::watcher_emits_runs_changed_on_run_file` and `ws_test::watcher_publishes_on_file_change`. The full workspace gate is therefore not considered passed.

## Recommendation

Do not merge until the three High findings are fixed. After the fixes, rerun the workspace tests and add targeted regression tests for a busy lock, an unknown schema, a config-less global write, and cross-scope allow.
