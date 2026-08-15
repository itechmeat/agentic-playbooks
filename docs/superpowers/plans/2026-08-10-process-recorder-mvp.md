# Process Recorder MVP: implementation plan

Status: DEFERRED (2026-08-15). Preserved for a future stage together with the
design spec `docs/superpowers/specs/2026-08-10-process-recorder-design.md`.
Do not execute this plan until the owner explicitly reactivates it.

For agentic workers. REQUIRED SUB-SKILL: superpowers:executing-plans (execute
one task at a time, stop at each reviewer gate).

Goal: build the browser-only MVP of the process recorder, a new `apb-recorder`
crate plus an MV3 extension that turn one recorded browser session into a
reviewed draft apb playbook carrying an explicit goal and machine-checkable
goal criteria, which the existing apb engine then runs.

Architecture: capture happens in an MV3 browser extension that emits a typed
event stream; the Rust `apb-recorder` crate performs deterministic
consolidation and heuristic segmentation, then a single model pass over a
compact segmented document produces a marked-up reconstruction; a mandatory
review plus a short clarifying interview produce the goal and criteria, and the
crate assembles a draft `apb_core::schema::Playbook` written to the normal
registry. Reconstruction and draft assembly are platform-independent; only
capture is surface-specific. The extension and the local Rust core talk over
the browser native-messaging channel.

Tech Stack: Rust workspace (edition 2024), `serde` / `serde_json` /
`serde_yaml_ng` / `thiserror`, `apb-core` for the playbook schema, atomic file
IO and the wall clock; the extension is TypeScript on WXT (MV3), built and
tested with bun and vitest (happy-dom environment).

## Global Constraints

Copy these rules into working memory; every task obeys them.

- Rust edition is 2024 (workspace-inherited). Do not set `edition` per crate;
  use `edition.workspace = true`.
- Dependency direction is enforced by code-ranker (ADP, no cycles). The new
  crate sits as `apb-core <- apb-recorder`. `apb-recorder` depends ONLY on
  `apb-core` in its `[dependencies]`. `apb-cli` gains a dependency on
  `apb-recorder` (cli sits on top, like it already does over engine and mcp).
  `apb-recorder` may `[dev-dependencies]` on `apb-engine` for the end-to-end
  test only; that is acyclic (engine never depends on recorder) and dev-deps do
  not count toward ADP.
- New `EventPayload`-style serde fields anywhere (raw events, wire messages,
  schema additions) are added only with `#[serde(default)]` so an older
  document parses unchanged. New optional fields also carry
  `skip_serializing_if` where a default value should stay off the wire.
- All state and artifact files are written atomically via
  `apb_core::fsutil::atomic_write` (or `atomic_write_private` for anything
  privacy-sensitive, 0600 on unix). Never write a file with `std::fs::write`
  directly.
- Timestamps come from `apb_core::clock` (`now_ms` returns `u128`
  milliseconds). Never call `SystemTime::now` directly in this crate.
- No em-dashes (U+2014) and no exclamation marks in any doc or user-facing
  string. No CJK anywhere. Machine-facing fields are English.
- Tests are bounded by construction (docs/TESTING-GUIDELINES.md): one
  integration binary per crate (`tests/main.rs` plus modules under
  `tests/suite/`), no bare sleeps unless timing is the subject, every wait has a
  deadline that names what it waited for, RAII cleanup built before the first
  thing that can panic, and any process-global mutation takes the one shared
  lock with a Drop restore. Do NOT add a new `tests/<name>.rs` next to
  `main.rs`.
- The model pass is a trait with a deterministic fake as the primary test seam.
  No test in this plan calls a real model. The single real adapter is exercised
  only by hand, never in CI.
- Every commit uses `git commit -s` (DCO signoff is mandatory in this repo) and
  ends the message body with the trailer, on its own line after the
  `Signed-off-by:` line that `-s` adds:
  `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`
- Commit subjects use conventional-commit prefixes (`feat:`, `test:`, `fix:`,
  `chore:`, `docs:`).
- No absolute machine-local paths anywhere in code, tests, or commit messages;
  all paths are repo-relative or come from `CARGO_MANIFEST_DIR` / a tempdir at
  runtime.
- Before a task-closing commit: `cargo fmt --all -- --check`, `cargo clippy
  --workspace --all-targets -- -D warnings`, and `code-ranker check .` are
  clean. Warm the cargo cache first with `cargo metadata --format-version 1
  >/dev/null`, then `code-ranker check .`; read `code-ranker docs base <ID>`
  before fixing any violation.
- Do not commit until the reviewer approves the task. The plan lists the exact
  commit command per task; run it only after the gate passes.

## Decisions already made (do not re-litigate)

- The Rust reconstruction and draft core live inside apb as the crate
  `apb-recorder`. Transport between the extension and the core is browser
  native messaging (Chrome frames messages; the Rust host implements the
  length-prefix codec).
- Reconstruction is deterministic consolidation, then heuristic segmentation,
  then a SINGLE model pass over a compact segmented document. The model never
  sees the raw event stream. The model enters late.
- Goal criteria are machine-checkable facts. The supervisor may repair the
  process but may never change the goal criteria (a runtime rule, not a
  validator rule; noted where relevant, enforced by the engine outside this
  plan's scope).
- Sessions and crops are stored as atomically-written JSON and blob files under
  a local recorder store, not SQLite. SQLite is deferred until scale requires
  it; JSON keeps the MVP dependency-light and the tests hermetic. This is a
  deliberate narrowing of the spec's "SQLite for events" note, valid because
  MVP recordings are single-session and small.
- Mac-first desktop capture is a FUTURE stage. The MVP is browser-only and runs
  on any OS. Nothing in this plan implements desktop capture.
- The interactive review screen (a human reading the show protocol, editing
  steps, answering questions, confirming goal criteria in a UI) is a FUTURE
  stage, deferred with the live pilot. The spec names review the primary
  human interaction; in this emulation-first MVP that interaction is driven by
  a structured `interview::InterviewAnswers` value (produced by a test or a
  file), not a UI. Task 8 renders the show protocol to human-readable text and
  Task 9 models the questions, so the review surface has everything it needs
  when it is built; the surface itself is out of this plan. The mandatory
  review-before-use guarantee holds in the MVP because no draft is assembled
  without an explicit `InterviewAnswers` (Task 11), and no draft runs
  automatically without a person supplying it.

## Goal and goal-criteria schema decision

Goal and goal criteria become NEW first-class additive optional fields on
`apb_core::schema::Playbook` (`goal: Option<Goal>`), not a convention buried in
`description` or `params`. Reason in one line: an additive `#[serde(default)]`
optional field keeps schema 2 backward compatible with no migration (exactly
how `trigger`, `requires`, and `effects` were added) while giving the validator
a typed target for a new machine-checkability rule (V41); a "convention within
existing fields" would be MORE invasive because the validator would have to
parse structure out of free-form text, and less reliable.

## File structure

Every file created or modified, with its one responsibility. Paths are
repo-relative.

Rust crate `apb-recorder` (new):

- `crates/apb-recorder/Cargo.toml` - crate manifest; deps: apb-core, serde,
  serde_json, serde_yaml_ng, thiserror; dev-deps: tempfile, apb-engine.
- `crates/apb-recorder/src/lib.rs` - module declarations and the crate's public
  re-exports.
- `crates/apb-recorder/src/raw.rs` - raw captured event types
  (`RecordingSession`, `RawEvent`, `RawEventKind`, `ElementRef`), the serde
  contract the extension mirrors.
- `crates/apb-recorder/src/consolidate.rs` - deterministic consolidation: fold
  keystrokes, drop pointer moves, coalesce clicks, aggregate scrolls,
  deduplicate. Pure functions.
- `crates/apb-recorder/src/segment.rs` - heuristic segmentation by
  navigation / tab-switch / clipboard / commit boundaries; commit-label
  detection. Pure functions.
- `crates/apb-recorder/src/document.rs` - the compact segmented document the
  model consumes (text lines plus crop references, never the raw stream).
- `crates/apb-recorder/src/model.rs` - the `ModelPass` trait, its output types
  with honesty markers, the deterministic `FakeModelPass`, and the real
  adapter behind a non-default feature.
- `crates/apb-recorder/src/reconstruct.rs` - the orchestrator that runs
  consolidate -> segment -> build_doc -> model pass, applies markers, and falls
  back to a deterministic step list on model failure or handles an empty
  recording.
- `crates/apb-recorder/src/protocol.rs` - the show protocol view over a
  reconstruction and its render-to-text.
- `crates/apb-recorder/src/interview.rs` - the clarifying interview model:
  questions derived from markers plus the mandatory goal-criteria question, and
  the answers structure.
- `crates/apb-recorder/src/draft.rs` - assembles a reviewed reconstruction plus
  answers into an `apb_core::schema::Playbook` with goal and criteria, and
  serializes it to YAML.
- `crates/apb-recorder/src/nativemsg.rs` - the host-side native-messaging codec
  (4-byte native-endian length prefix plus JSON body), the wire message enums,
  and the host-manifest JSON generator.
- `crates/apb-recorder/src/store.rs` - the local recorder store: persist and
  load a `RecordingSession` as atomically-written JSON under a store root.
- `crates/apb-recorder/tests/main.rs` - the single integration-test binary.
- `crates/apb-recorder/tests/suite/common/mod.rs` - shared test helpers
  (fixture loading, temp store roots).
- `crates/apb-recorder/tests/suite/*.rs` - one module per integration test file.
- `crates/apb-recorder/tests/fixtures/*.json` - recorded raw-event fixtures.

apb-core (modified):

- `crates/apb-core/src/schema.rs` - add `Goal` and `GoalCriterion` types and
  the `goal: Option<Goal>` field on `Playbook`.
- `crates/apb-core/src/validate/goal.rs` - new validator rule family (V41) for
  goal-criteria machine-checkability. New file.
- `crates/apb-core/src/validate/mod.rs` - wire `check_goal` into `validate`.

apb-cli (modified):

- `crates/apb-cli/Cargo.toml` - add the `apb-recorder` dependency.
- `crates/apb-cli/src/recorder_host.rs` - the `apb recorder-host` subcommand: a
  native-messaging host loop that persists incoming session chunks via
  `apb-recorder`. New file.
- `crates/apb-cli/src/main.rs` - register the hidden `recorder-host` subcommand.

Extension `extension/` (new, top-level, WXT + MV3):

- `extension/package.json` - extension package; WXT, vitest, happy-dom.
- `extension/wxt.config.ts` - WXT config, MV3 manifest (permissions: activeTab,
  scripting, nativeMessaging; not incognito).
- `extension/entrypoints/background.ts` - recording lifecycle, native-messaging
  port, incognito-window refusal, session buffering, delete-last-30s and
  delete-all.
- `extension/entrypoints/content.ts` - DOM capture: locators, semantic label,
  password-field exclusion, event emission to background.
- `extension/entrypoints/popup/` - the recording indicator and controls (start,
  stop, delete last 30s, delete all).
- `extension/lib/events.ts` - the TypeScript mirror of the Rust raw-event serde
  contract.
- `extension/lib/capture.ts` - pure capture helpers (locator + label
  extraction, password/redaction predicates) unit-tested without a browser.
- `extension/lib/nativemsg.ts` - a thin wrapper over
  `chrome.runtime.connectNative`.
- `extension/lib/*.test.ts` - vitest specs (happy-dom / jsdom, no live browser).

---

### Task 1: Scaffold the apb-recorder crate and wire it into the workspace

Files:
- Create: `crates/apb-recorder/Cargo.toml`
- Create: `crates/apb-recorder/src/lib.rs`
- Create: `crates/apb-recorder/tests/main.rs`
- Create: `crates/apb-recorder/tests/suite/common/mod.rs`
- Create: `crates/apb-recorder/tests/suite/smoke_test.rs`
- Modify: `Cargo.toml` (workspace members)

Interfaces:
- Produces: crate `apb-recorder` with an empty public surface and a version
  constant `pub const VERSION: &str = env!("CARGO_PKG_VERSION");`.

Steps:

1. Write the failing smoke test first.

`crates/apb-recorder/tests/main.rs`:
```rust
//! Single integration-test binary for apb-recorder. Cargo compiles every file
//! directly under `tests/` into its own binary (each paying a first-spawn scan
//! on macOS), so all test files live under `tests/suite/` and are declared as
//! modules here. Do not add a `tests/<name>.rs` next to this file.

#[path = "suite/common/mod.rs"]
mod common;

#[path = "suite/smoke_test.rs"]
mod smoke_test;
```

`crates/apb-recorder/tests/suite/common/mod.rs`:
```rust
//! Shared helpers for the apb-recorder integration suite.

/// Absolute path to a fixture under `tests/fixtures/`, resolved from the crate
/// manifest dir so it is immune to a source-file move.
#[allow(dead_code)]
pub fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}
```

`crates/apb-recorder/tests/suite/smoke_test.rs`:
```rust
#[test]
fn crate_exposes_a_version() {
    assert!(!apb_recorder::VERSION.is_empty());
}
```

2. Run to fail: `cargo test -p apb-recorder --test main smoke_test::`.
   Expected: compile error, `apb-recorder` is not a workspace member / unknown
   crate `apb_recorder`.

3. Minimal implementation.

`Cargo.toml` (workspace) members line becomes:
```toml
members = ["crates/apb-core", "crates/apb-server", "crates/apb-cli", "crates/apb-engine", "crates/apb-mcp", "crates/apb-recorder"]
```

`crates/apb-recorder/Cargo.toml`:
```toml
[package]
name = "apb-recorder"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
apb-core = { path = "../apb-core" }
serde.workspace = true
serde_json.workspace = true
serde_yaml_ng.workspace = true
thiserror.workspace = true

[dev-dependencies]
tempfile = "3.27.0"
# End-to-end only (Task 15): drive the drafted playbook through the engine.
# Acyclic: apb-engine never depends on apb-recorder, and dev-deps are excluded
# from the ADP cycle check.
apb-engine = { path = "../apb-engine" }
```

`crates/apb-recorder/src/lib.rs`:
```rust
//! Process recorder core (spec 2026-08-10): consolidation, segmentation, the
//! single model pass, the show protocol, the clarifying interview, and draft
//! playbook assembly. Capture is surface-specific and lives in the browser
//! extension; this crate is platform-independent.

/// Crate version, surfaced so hosts and the extension can check compatibility.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
```

4. Run to pass: `cargo test -p apb-recorder --test main smoke_test::`. Expected:
   1 passed.

5. `cargo fmt --all -- --check`; `cargo clippy -p apb-recorder --all-targets --
   -D warnings`; warm cache then `code-ranker check .`.

6. Commit:
```
git commit -s -m "feat(recorder): scaffold apb-recorder crate and wire into workspace

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Raw captured event types

Files:
- Create: `crates/apb-recorder/src/raw.rs`
- Modify: `crates/apb-recorder/src/lib.rs`
- Create: `crates/apb-recorder/tests/suite/raw_test.rs`
- Create: `crates/apb-recorder/tests/fixtures/invoice_session.json`
- Modify: `crates/apb-recorder/tests/main.rs`

Interfaces:
- Produces:
```rust
pub struct ElementRef { pub tag: String, pub xpath: String, pub css: String, pub label: String }
pub struct RawEvent {
    pub seq: u64,
    pub ts_ms: u128,       // corrected time (one timeline)
    pub raw_ts_ms: u128,   // raw extension time, kept per spec clock-alignment
    pub page_url: String,
    pub frame_url: String,
    pub kind: RawEventKind,
}
pub enum RawEventKind {
    Click { element: ElementRef },
    KeyDown { key: String, element: Option<ElementRef> },
    TextInput { element: ElementRef, value: String },
    Change { element: ElementRef, value: String },
    Navigate { url: String },
    TabActivated { tab_id: i64 },
    TabClosed { tab_id: i64 },
    Scroll { x: f64, y: f64 },
    ClipboardWrite { text_len: usize },
    PointerMove { x: f64, y: f64 },
    CaptureGap { reason: String },
}
pub struct RecordingSession { pub id: String, pub started_ms: u128, pub events: Vec<RawEvent> }
```
   `RawEventKind` is `#[serde(tag = "type", rename_all = "snake_case")]`, every
   struct derives `Debug, Clone, PartialEq, Serialize, Deserialize`.

Steps:

1. Write the failing test. Add the fixture file
   `crates/apb-recorder/tests/fixtures/invoice_session.json`:
```json
{
  "id": "sess-1",
  "started_ms": 1723200000000,
  "events": [
    { "seq": 0, "ts_ms": 1723200000100, "raw_ts_ms": 1723200000100, "page_url": "https://app.example/invoices", "frame_url": "https://app.example/invoices", "kind": { "type": "navigate", "url": "https://app.example/invoices" } },
    { "seq": 1, "ts_ms": 1723200000500, "raw_ts_ms": 1723200000500, "page_url": "https://app.example/invoices", "frame_url": "https://app.example/invoices", "kind": { "type": "click", "element": { "tag": "button", "xpath": "//button[1]", "css": "button.new", "label": "New invoice" } } },
    { "seq": 2, "ts_ms": 1723200001000, "raw_ts_ms": 1723200001000, "page_url": "https://app.example/invoices/new", "frame_url": "https://app.example/invoices/new", "kind": { "type": "text_input", "element": { "tag": "input", "xpath": "//input[@name='amount']", "css": "#amount", "label": "Amount" }, "value": "100" } }
  ]
}
```

`crates/apb-recorder/tests/suite/raw_test.rs`:
```rust
use crate::common::fixture_path;
use apb_recorder::raw::{RawEventKind, RecordingSession};

#[test]
fn session_fixture_round_trips_through_serde() {
    let text = std::fs::read_to_string(fixture_path("invoice_session.json")).unwrap();
    let session: RecordingSession = serde_json::from_str(&text).unwrap();
    assert_eq!(session.id, "sess-1");
    assert_eq!(session.events.len(), 3);
    assert!(matches!(session.events[0].kind, RawEventKind::Navigate { .. }));

    // Re-serializing and parsing again yields the same struct (serde contract).
    let again = serde_json::to_string(&session).unwrap();
    let back: RecordingSession = serde_json::from_str(&again).unwrap();
    assert_eq!(session, back);
}

#[test]
fn unknown_optional_fields_do_not_break_parsing() {
    // A newer extension may add fields; older cores must still parse. The kind
    // tag stays stable.
    let json = r#"{"id":"s","started_ms":1,"events":[
      {"seq":0,"ts_ms":2,"raw_ts_ms":2,"page_url":"u","frame_url":"u",
       "kind":{"type":"click","element":{"tag":"a","xpath":"/a","css":"a","label":"x"}}}]}"#;
    let s: RecordingSession = serde_json::from_str(json).unwrap();
    assert_eq!(s.events.len(), 1);
}
```

   Add to `tests/main.rs`:
```rust
#[path = "suite/raw_test.rs"]
mod raw_test;
```

2. Run to fail: `cargo test -p apb-recorder --test main raw_test::`. Expected:
   unresolved import `apb_recorder::raw`.

3. Implement `crates/apb-recorder/src/raw.rs` with the types from Interfaces
   above (all `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]`,
   `RawEventKind` tagged `type`, snake_case). Add `pub mod raw;` to `lib.rs`.

4. Run to pass: `cargo test -p apb-recorder --test main raw_test::`. Expected:
   2 passed.

5. fmt, clippy, code-ranker clean.

6. Commit:
```
git commit -s -m "feat(recorder): raw captured event types and serde contract

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Deterministic consolidation

Files:
- Create: `crates/apb-recorder/src/consolidate.rs`
- Modify: `crates/apb-recorder/src/lib.rs`
- Create: `crates/apb-recorder/tests/suite/consolidate_test.rs`
- Modify: `crates/apb-recorder/tests/main.rs`

Interfaces:
- Consumes: `&[raw::RawEvent]`.
- Produces:
```rust
pub struct ConsolidatedEvent { pub ts_ms: u128, pub page_url: String, pub frame_url: String, pub action: Action }
pub enum Action {
    Click { element: ElementRef },
    TypeText { element: ElementRef, text: String },
    SetValue { element: ElementRef, value: String },
    KeyPress { key: String },
    Navigate { url: String },
    TabSwitch { tab_id: i64 },
    TabClosed { tab_id: i64 },
    ClipboardWrite { text_len: usize },
    ScrollTo { x: f64, y: f64 },
    CaptureGap { reason: String },
}
pub fn consolidate(events: &[RawEvent]) -> Vec<ConsolidatedEvent>;
```
   Rules: drop every `PointerMove`; fold a run of `TextInput` on the SAME
   element into one `TypeText` (last value wins, since browser input events
   carry the full field value); coalesce a click that follows nothing new into
   one `Click`; aggregate a run of `Scroll` into a single `ScrollTo` at the last
   position; drop an exact-duplicate consecutive event; a printable-key
   `KeyDown` inside a text field folds into the field's `TypeText`, a non-text
   key (Enter, Tab, Escape) becomes `KeyPress`.

Steps:

1. Failing test `crates/apb-recorder/tests/suite/consolidate_test.rs`:
```rust
use apb_recorder::consolidate::{consolidate, Action};
use apb_recorder::raw::{ElementRef, RawEvent, RawEventKind};

fn el(css: &str, label: &str) -> ElementRef {
    ElementRef { tag: "input".into(), xpath: format!("//{css}"), css: css.into(), label: label.into() }
}
fn ev(seq: u64, kind: RawEventKind) -> RawEvent {
    RawEvent { seq, ts_ms: seq as u128, raw_ts_ms: seq as u128, page_url: "u".into(), frame_url: "u".into(), kind }
}

#[test]
fn pointer_moves_are_dropped() {
    let out = consolidate(&[
        ev(0, RawEventKind::PointerMove { x: 1.0, y: 1.0 }),
        ev(1, RawEventKind::PointerMove { x: 2.0, y: 2.0 }),
        ev(2, RawEventKind::Click { element: el("#b", "Save") }),
    ]);
    assert_eq!(out.len(), 1);
    assert!(matches!(out[0].action, Action::Click { .. }));
}

#[test]
fn consecutive_text_input_on_same_element_folds_to_one_type_text() {
    let out = consolidate(&[
        ev(0, RawEventKind::TextInput { element: el("#amt", "Amount"), value: "1".into() }),
        ev(1, RawEventKind::TextInput { element: el("#amt", "Amount"), value: "10".into() }),
        ev(2, RawEventKind::TextInput { element: el("#amt", "Amount"), value: "100".into() }),
    ]);
    assert_eq!(out.len(), 1);
    match &out[0].action {
        Action::TypeText { text, element } => { assert_eq!(text, "100"); assert_eq!(element.css, "#amt"); }
        other => panic!("expected TypeText, got {other:?}"),
    }
}

#[test]
fn scroll_runs_aggregate_to_last_position() {
    let out = consolidate(&[
        ev(0, RawEventKind::Scroll { x: 0.0, y: 100.0 }),
        ev(1, RawEventKind::Scroll { x: 0.0, y: 250.0 }),
    ]);
    assert_eq!(out.len(), 1);
    assert!(matches!(out[0].action, Action::ScrollTo { y, .. } if (y - 250.0).abs() < f64::EPSILON));
}

#[test]
fn enter_key_becomes_a_key_press_not_folded_into_typing() {
    let out = consolidate(&[
        ev(0, RawEventKind::TextInput { element: el("#q", "Search"), value: "cats".into() }),
        ev(1, RawEventKind::KeyDown { key: "Enter".into(), element: Some(el("#q", "Search")) }),
    ]);
    assert_eq!(out.len(), 2);
    assert!(matches!(out[1].action, Action::KeyPress { ref key } if key == "Enter"));
}
```
   Add the `mod` line to `tests/main.rs`.

2. Run to fail: `cargo test -p apb-recorder --test main consolidate_test::`.
   Expected: unresolved import `apb_recorder::consolidate`.

3. Implement `consolidate.rs` per the rules. `Action` derives
   `Debug, Clone, PartialEq`. Add `pub mod consolidate;` to `lib.rs`. Keep every
   function pure (no IO, no clock).

4. Run to pass. Expected: 4 passed.

5. fmt, clippy, code-ranker clean. If code-ranker flags complexity on
   `consolidate`, split per-rule helpers (`fold_typing`, `drop_pointer_moves`,
   `aggregate_scrolls`) and re-run.

6. Commit:
```
git commit -s -m "feat(recorder): deterministic event consolidation

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: Heuristic segmentation

Files:
- Create: `crates/apb-recorder/src/segment.rs`
- Modify: `crates/apb-recorder/src/lib.rs`
- Create: `crates/apb-recorder/tests/suite/segment_test.rs`
- Modify: `crates/apb-recorder/tests/main.rs`

Interfaces:
- Consumes: `&[consolidate::ConsolidatedEvent]`.
- Produces:
```rust
pub enum BoundaryKind { Start, Navigation, TabSwitch, ClipboardWrite, Commit, CaptureGap }
pub struct Segment { pub index: usize, pub boundary: BoundaryKind, pub events: Vec<ConsolidatedEvent> }
pub fn segment(events: &[ConsolidatedEvent]) -> Vec<Segment>;
pub fn is_commit_label(label: &str) -> bool;      // save|submit|send|create|delete|pay (case-insensitive, word match)
pub fn is_irreversible_label(label: &str) -> bool; // send|pay|delete
```
   A new segment starts at: a `Navigate`, a `TabSwitch`, a `ClipboardWrite`, a
   `CaptureGap`, or a commit action (an `Action::KeyPress { key: "Enter" }`, or
   an `Action::Click` on an element whose label satisfies `is_commit_label`).
   The commit action is the LAST event of the segment it closes; the next
   segment opens after it. The first segment carries `BoundaryKind::Start`.

Steps:

1. Failing test `crates/apb-recorder/tests/suite/segment_test.rs`:
```rust
use apb_recorder::consolidate::{Action, ConsolidatedEvent};
use apb_recorder::raw::ElementRef;
use apb_recorder::segment::{is_commit_label, segment, BoundaryKind};

fn ce(action: Action) -> ConsolidatedEvent {
    ConsolidatedEvent { ts_ms: 0, page_url: "u".into(), frame_url: "u".into(), action }
}
fn el(label: &str) -> ElementRef {
    ElementRef { tag: "button".into(), xpath: "/b".into(), css: "b".into(), label: label.into() }
}

#[test]
fn commit_label_matches_save_submit_send_create_delete() {
    for w in ["Save", "SUBMIT", "Send invoice", "Create", "Delete row", "Pay now"] {
        assert!(is_commit_label(w), "{w} should be a commit label");
    }
    assert!(!is_commit_label("Amount"));
    assert!(!is_commit_label("Cancel"));
}

#[test]
fn navigation_opens_a_new_segment() {
    let out = segment(&[
        ce(Action::Navigate { url: "a".into() }),
        ce(Action::Click { element: el("Amount") }),
        ce(Action::Navigate { url: "b".into() }),
    ]);
    assert_eq!(out.len(), 2);
    assert!(matches!(out[0].boundary, BoundaryKind::Start));
    assert!(matches!(out[1].boundary, BoundaryKind::Navigation));
}

#[test]
fn a_commit_click_closes_its_segment() {
    let out = segment(&[
        ce(Action::Click { element: el("Amount") }),
        ce(Action::Click { element: el("Save") }),
        ce(Action::Click { element: el("Next field") }),
    ]);
    assert_eq!(out.len(), 2);
    // The Save click is the last event of the first segment.
    assert_eq!(out[0].events.len(), 2);
    assert!(matches!(out[1].boundary, BoundaryKind::Commit));
}
```
   Add the `mod` line to `tests/main.rs`.

2. Run to fail. Expected: unresolved import `apb_recorder::segment`.

3. Implement `segment.rs`. `BoundaryKind` derives `Debug, Clone, PartialEq`;
   `is_commit_label` lowercases and matches whole words against the set
   `{save, submit, send, create, delete, pay}`; `is_irreversible_label` against
   `{send, pay, delete}`. Add `pub mod segment;` to `lib.rs`. Pure functions.

4. Run to pass. Expected: 3 passed.

5. fmt, clippy, code-ranker clean.

6. Commit:
```
git commit -s -m "feat(recorder): heuristic segmentation by strong boundaries

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: Compact segmented document

Files:
- Create: `crates/apb-recorder/src/document.rs`
- Modify: `crates/apb-recorder/src/lib.rs`
- Create: `crates/apb-recorder/tests/suite/document_test.rs`
- Modify: `crates/apb-recorder/tests/main.rs`

Interfaces:
- Consumes: `&[segment::Segment]`.
- Produces:
```rust
pub struct CropRef { pub id: String }
pub struct DocSegment { pub index: usize, pub boundary: String, pub lines: Vec<String>, pub crops: Vec<CropRef> }
pub struct SegmentedDoc { pub segments: Vec<DocSegment> }
pub fn build_doc(segments: &[Segment]) -> SegmentedDoc;
```
   `build_doc` renders each consolidated action into one compact human-readable
   line (for example `click "Save" (button)`, `type "100" into "Amount"`,
   `navigate https://...`, `capture gap: canvas-rendered region`). This text is
   what the model pass consumes; the raw stream is never handed to the model.
   `crops` is empty in the MVP text path (crop capture is best-effort in the
   extension and optional here). All types derive
   `Debug, Clone, PartialEq, Serialize, Deserialize`.

Steps:

1. Failing test `crates/apb-recorder/tests/suite/document_test.rs`:
```rust
use apb_recorder::consolidate::{Action, ConsolidatedEvent};
use apb_recorder::document::build_doc;
use apb_recorder::raw::ElementRef;
use apb_recorder::segment::segment;

fn ce(action: Action) -> ConsolidatedEvent {
    ConsolidatedEvent { ts_ms: 0, page_url: "u".into(), frame_url: "u".into(), action }
}
fn el(label: &str) -> ElementRef {
    ElementRef { tag: "input".into(), xpath: "/i".into(), css: "i".into(), label: label.into() }
}

#[test]
fn document_renders_one_compact_line_per_action() {
    let segs = segment(&[
        ce(Action::TypeText { element: el("Amount"), text: "100".into() }),
        ce(Action::Click { element: ElementRef { tag: "button".into(), xpath: "/b".into(), css: "b".into(), label: "Save".into() } }),
    ]);
    let doc = build_doc(&segs);
    assert_eq!(doc.segments.len(), 1);
    let lines = &doc.segments[0].lines;
    assert!(lines.iter().any(|l| l.contains("Amount") && l.contains("100")));
    assert!(lines.iter().any(|l| l.contains("Save")));
    assert!(doc.segments[0].crops.is_empty());
}
```
   Add the `mod` line to `tests/main.rs`.

2. Run to fail. Expected: unresolved import `apb_recorder::document`.

3. Implement `document.rs`; add `pub mod document;` to `lib.rs`. Pure.

4. Run to pass. Expected: 1 passed.

5. fmt, clippy, code-ranker clean.

6. Commit:
```
git commit -s -m "feat(recorder): compact segmented document for the model pass

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: The model pass trait, its output types, and a deterministic fake

Files:
- Create: `crates/apb-recorder/src/model.rs`
- Modify: `crates/apb-recorder/src/lib.rs`
- Create: `crates/apb-recorder/tests/suite/model_test.rs`
- Modify: `crates/apb-recorder/tests/main.rs`

Interfaces:
- Consumes: `&document::SegmentedDoc`.
- Produces:
```rust
pub enum Confidence { High, Medium, Low }
pub struct ModelStep {
    pub segment_index: usize,
    pub name: String,
    pub description: String,
    pub reads: Vec<String>,
    pub enters: Vec<String>,
    pub varies_per_run: Vec<String>,
    pub confidence: Confidence,
    pub unexplained_choice: bool,
    pub limited_context: bool,
}
pub struct ModelReconstruction { pub steps: Vec<ModelStep> }
pub enum ModelError { Failed(String), TimedOut }
pub trait ModelPass { fn reconstruct(&self, doc: &SegmentedDoc) -> Result<ModelReconstruction, ModelError>; }
pub struct FakeModelPass { pub result: Result<ModelReconstruction, ModelError> }
```
   `FakeModelPass` returns a clone of its canned `result`. It is the primary
   test seam. The single real adapter (`ClaudeModelPass`) is compiled only under
   a non-default `real-model` feature and is never constructed in any test.

Steps:

1. Failing test `crates/apb-recorder/tests/suite/model_test.rs`:
```rust
use apb_recorder::document::SegmentedDoc;
use apb_recorder::model::{Confidence, FakeModelPass, ModelPass, ModelReconstruction, ModelStep};

fn one_step() -> ModelReconstruction {
    ModelReconstruction { steps: vec![ModelStep {
        segment_index: 0,
        name: "Enter invoice amount".into(),
        description: "Type the amount into the Amount field".into(),
        reads: vec![],
        enters: vec!["Amount".into()],
        varies_per_run: vec!["Amount".into()],
        confidence: Confidence::High,
        unexplained_choice: false,
        limited_context: false,
    }] }
}

#[test]
fn fake_model_returns_its_canned_reconstruction() {
    let fake = FakeModelPass { result: Ok(one_step()) };
    let out = fake.reconstruct(&SegmentedDoc { segments: vec![] }).unwrap();
    assert_eq!(out.steps.len(), 1);
    assert_eq!(out.steps[0].name, "Enter invoice amount");
    assert!(matches!(out.steps[0].confidence, Confidence::High));
}

#[test]
fn fake_model_can_simulate_a_failure() {
    let fake = FakeModelPass { result: Err(apb_recorder::model::ModelError::TimedOut) };
    assert!(fake.reconstruct(&SegmentedDoc { segments: vec![] }).is_err());
}
```
   Add the `mod` line to `tests/main.rs`.

2. Run to fail. Expected: unresolved import `apb_recorder::model`.

3. Implement `model.rs`. Output types derive
   `Debug, Clone, PartialEq, Serialize, Deserialize`; `Confidence` is
   `rename_all = "snake_case"`. `FakeModelPass::reconstruct` clones `self.result`
   (so `ModelError` and `ModelReconstruction` are `Clone`). Add the real adapter
   skeleton guarded by `#[cfg(feature = "real-model")]` with a `real-model = []`
   entry under `[features]` in `Cargo.toml`, and a module doc line stating no
   test constructs it. Add `pub mod model;` to `lib.rs`.

4. Run to pass. Expected: 2 passed.

5. fmt, clippy, code-ranker clean.

6. Commit:
```
git commit -s -m "feat(recorder): model-pass trait with a deterministic fake

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: The reconstruction orchestrator (with model-failure fallback and empty handling)

Files:
- Create: `crates/apb-recorder/src/reconstruct.rs`
- Modify: `crates/apb-recorder/src/lib.rs`
- Create: `crates/apb-recorder/tests/suite/reconstruct_test.rs`
- Modify: `crates/apb-recorder/tests/main.rs`

Interfaces:
- Consumes: `&raw::RecordingSession`, `&impl model::ModelPass`.
- Produces:
```rust
pub enum StepMarker { UnexplainedChoice, LimitedContext, Irreversible }
pub struct Step {
    pub index: usize,
    pub label: String,
    pub description: String,
    pub reads: Vec<String>,
    pub enters: Vec<String>,
    pub varies: Vec<String>,
    pub confidence: Confidence,
    pub markers: Vec<StepMarker>,
}
pub enum ReconstructionSource { Model, DeterministicFallback }
pub struct Reconstruction { pub steps: Vec<Step>, pub source: ReconstructionSource }
pub fn reconstruct<M: ModelPass>(session: &RecordingSession, model: &M) -> Reconstruction;
```
   Pipeline: `consolidate` -> `segment` -> `build_doc` -> `model.reconstruct`.
   On `Ok`, map each `ModelStep` to a `Step`, adding `UnexplainedChoice` /
   `LimitedContext` markers from the model flags and `Irreversible` when the
   segment's closing action is a click whose label is `is_irreversible_label`.
   On `Err`, produce a `DeterministicFallback` reconstruction: one generically
   named `Step` per segment (label like `Step 1`, confidence `Low`), so the
   person still has something to review. An empty or action-free session yields
   `Reconstruction { steps: vec![], source: Model }` (draft assembly refuses it
   in Task 11).

Steps:

1. Failing test `crates/apb-recorder/tests/suite/reconstruct_test.rs`:
```rust
use apb_recorder::model::{Confidence, FakeModelPass, ModelError, ModelReconstruction, ModelStep};
use apb_recorder::raw::{ElementRef, RawEvent, RawEventKind, RecordingSession};
use apb_recorder::reconstruct::{reconstruct, ReconstructionSource, StepMarker};

fn ev(seq: u64, kind: RawEventKind) -> RawEvent {
    RawEvent { seq, ts_ms: seq as u128, raw_ts_ms: seq as u128, page_url: "u".into(), frame_url: "u".into(), kind }
}
fn commit(label: &str) -> RawEventKind {
    RawEventKind::Click { element: ElementRef { tag: "button".into(), xpath: "/b".into(), css: "b".into(), label: label.into() } }
}

#[test]
fn model_success_marks_irreversible_steps() {
    let session = RecordingSession {
        id: "s".into(), started_ms: 0,
        events: vec![ev(0, commit("Send invoice"))],
    };
    let model = FakeModelPass { result: Ok(ModelReconstruction { steps: vec![ModelStep {
        segment_index: 0, name: "Send".into(), description: "send it".into(),
        reads: vec![], enters: vec![], varies_per_run: vec![],
        confidence: Confidence::High, unexplained_choice: false, limited_context: false,
    }] }) };
    let recon = reconstruct(&session, &model);
    assert!(matches!(recon.source, ReconstructionSource::Model));
    assert!(recon.steps[0].markers.contains(&StepMarker::Irreversible));
}

#[test]
fn model_failure_falls_back_to_a_deterministic_step_list() {
    let session = RecordingSession {
        id: "s".into(), started_ms: 0,
        events: vec![ev(0, RawEventKind::Navigate { url: "a".into() }), ev(1, commit("Save"))],
    };
    let model = FakeModelPass { result: Err(ModelError::Failed("boom".into())) };
    let recon = reconstruct(&session, &model);
    assert!(matches!(recon.source, ReconstructionSource::DeterministicFallback));
    assert!(!recon.steps.is_empty());
    assert!(recon.steps.iter().all(|s| matches!(s.confidence, Confidence::Low)));
}

#[test]
fn empty_session_yields_no_steps() {
    let session = RecordingSession { id: "s".into(), started_ms: 0, events: vec![] };
    let model = FakeModelPass { result: Ok(ModelReconstruction { steps: vec![] }) };
    assert!(reconstruct(&session, &model).steps.is_empty());
}
```
   Add the `mod` line to `tests/main.rs`.

2. Run to fail. Expected: unresolved import `apb_recorder::reconstruct`.

3. Implement `reconstruct.rs`; re-export `Confidence` from `model`. Types derive
   `Debug, Clone, PartialEq`. Add `pub mod reconstruct;` to `lib.rs`.

4. Run to pass. Expected: 3 passed.

5. fmt, clippy, code-ranker clean. If `reconstruct` trips complexity, extract
   `map_model_steps` and `deterministic_fallback` helpers.

6. Commit:
```
git commit -s -m "feat(recorder): reconstruction orchestrator with honesty markers and fallback

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 8: Show protocol model and render-to-text

Files:
- Create: `crates/apb-recorder/src/protocol.rs`
- Modify: `crates/apb-recorder/src/lib.rs`
- Create: `crates/apb-recorder/tests/suite/protocol_test.rs`
- Modify: `crates/apb-recorder/tests/main.rs`

Interfaces:
- Consumes: `&reconstruct::Reconstruction`.
- Produces:
```rust
pub struct ShowProtocol { pub steps: Vec<Step>, pub source: ReconstructionSource }
impl ShowProtocol { pub fn from_reconstruction(r: &Reconstruction) -> Self; }
pub fn render_text(p: &ShowProtocol) -> String;
```
   `render_text` produces the human-readable step list: a numbered line per step
   with its label, a confidence tag (`[high]` / `[medium]` / `[low]`), and any
   markers spelled out (`needs clarification: unexplained choice`, `limited
   context here`, `irreversible step`). No em-dashes, no exclamation marks.

Steps:

1. Failing test `crates/apb-recorder/tests/suite/protocol_test.rs`:
```rust
use apb_recorder::model::Confidence;
use apb_recorder::protocol::{render_text, ShowProtocol};
use apb_recorder::reconstruct::{Reconstruction, ReconstructionSource, Step, StepMarker};

fn recon() -> Reconstruction {
    Reconstruction {
        source: ReconstructionSource::Model,
        steps: vec![
            Step { index: 0, label: "Open invoices".into(), description: "d".into(), reads: vec![], enters: vec![], varies: vec![], confidence: Confidence::High, markers: vec![] },
            Step { index: 1, label: "Send".into(), description: "d".into(), reads: vec![], enters: vec![], varies: vec![], confidence: Confidence::Low, markers: vec![StepMarker::Irreversible, StepMarker::UnexplainedChoice] },
        ],
    }
}

#[test]
fn render_lists_numbered_steps_with_confidence_and_markers() {
    let text = render_text(&ShowProtocol::from_reconstruction(&recon()));
    assert!(text.contains("1. Open invoices"));
    assert!(text.contains("[high]"));
    assert!(text.contains("2. Send"));
    assert!(text.contains("[low]"));
    assert!(text.contains("irreversible"));
    assert!(text.contains("unexplained choice"));
    assert!(!text.contains('\u{2014}'));
    assert!(!text.contains('!'));
}
```
   Add the `mod` line to `tests/main.rs`.

2. Run to fail. Expected: unresolved import `apb_recorder::protocol`.

3. Implement `protocol.rs`; add `pub mod protocol;` to `lib.rs`.

4. Run to pass. Expected: 1 passed.

5. fmt, clippy, code-ranker clean.

6. Commit:
```
git commit -s -m "feat(recorder): show protocol model and text rendering

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 9: Clarifying interview model

Files:
- Create: `crates/apb-recorder/src/interview.rs`
- Modify: `crates/apb-recorder/src/lib.rs`
- Create: `crates/apb-recorder/tests/suite/interview_test.rs`
- Modify: `crates/apb-recorder/tests/main.rs`

Interfaces:
- Consumes: `&reconstruct::Reconstruction`.
- Produces:
```rust
pub enum QuestionKind { UnexplainedChoice, LowConfidence, LimitedContext, GoalCriteria }
pub struct Question { pub step_index: Option<usize>, pub kind: QuestionKind, pub prompt: String }
pub struct Interview { pub questions: Vec<Question> }
pub fn build_interview(r: &Reconstruction) -> Interview;
pub struct GoalDraft { pub statement: String, pub criteria: Vec<String> }
pub struct InterviewAnswers {
    pub step_answers: std::collections::BTreeMap<usize, String>,
    pub goal: Option<GoalDraft>,
}
```
   `build_interview` emits one question per step carrying an
   `UnexplainedChoice` or `LimitedContext` marker, one per `Low`-confidence
   step, and always appends exactly one `GoalCriteria` question with
   `step_index: None` and the prompt "How do you yourself know you did this
   right?" (the spec's criteria-elicitation question). All types derive
   `Debug, Clone, PartialEq, Serialize, Deserialize`.

Steps:

1. Failing test `crates/apb-recorder/tests/suite/interview_test.rs`:
```rust
use apb_recorder::interview::{build_interview, QuestionKind};
use apb_recorder::model::Confidence;
use apb_recorder::reconstruct::{Reconstruction, ReconstructionSource, Step, StepMarker};

fn step(index: usize, confidence: Confidence, markers: Vec<StepMarker>) -> Step {
    Step { index, label: "s".into(), description: "d".into(), reads: vec![], enters: vec![], varies: vec![], confidence, markers }
}

#[test]
fn interview_asks_about_markers_low_confidence_and_always_goal_criteria() {
    let recon = Reconstruction {
        source: ReconstructionSource::Model,
        steps: vec![
            step(0, Confidence::High, vec![StepMarker::UnexplainedChoice]),
            step(1, Confidence::Low, vec![]),
            step(2, Confidence::High, vec![]),
        ],
    };
    let iv = build_interview(&recon);
    assert!(iv.questions.iter().any(|q| q.step_index == Some(0) && matches!(q.kind, QuestionKind::UnexplainedChoice)));
    assert!(iv.questions.iter().any(|q| q.step_index == Some(1) && matches!(q.kind, QuestionKind::LowConfidence)));
    let goal_qs: Vec<_> = iv.questions.iter().filter(|q| matches!(q.kind, QuestionKind::GoalCriteria)).collect();
    assert_eq!(goal_qs.len(), 1);
    assert_eq!(goal_qs[0].step_index, None);
    // Step 2 is clean: no question about it.
    assert!(!iv.questions.iter().any(|q| q.step_index == Some(2)));
}
```
   Add the `mod` line to `tests/main.rs`.

2. Run to fail. Expected: unresolved import `apb_recorder::interview`.

3. Implement `interview.rs`; add `pub mod interview;` to `lib.rs`.

4. Run to pass. Expected: 1 passed.

5. fmt, clippy, code-ranker clean.

6. Commit:
```
git commit -s -m "feat(recorder): clarifying interview model

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 10: Goal and goal-criteria schema fields plus validator rule V41

Files:
- Modify: `crates/apb-core/src/schema.rs`
- Create: `crates/apb-core/src/validate/goal.rs`
- Modify: `crates/apb-core/src/validate/mod.rs`
- Create: `crates/apb-core/tests/suite/goal_schema_test.rs`
- Modify: `crates/apb-core/tests/main.rs`

Interfaces:
- Produces (in `apb_core::schema`):
```rust
pub enum GoalCheck { Manual, Marker { marker: String }, Script { path: String } }
pub struct GoalCriterion { pub description: String, pub check: GoalCheck }
pub struct Goal { pub statement: String, pub criteria: Vec<GoalCriterion> }
// New field on Playbook:
#[serde(default, skip_serializing_if = "Option::is_none")]
pub goal: Option<Goal>,
```
   `GoalCheck` is `#[serde(tag = "kind", rename_all = "snake_case")]` with a
   default of `Manual` when absent. All derive
   `Debug, Clone, PartialEq, Deserialize, Serialize`.
- Produces (validator): `check_goal(playbook, &mut report)` emitting V41 when a
  `goal` is present but its `statement` is empty, or it has zero criteria, or
  any criterion has an empty `description`. V41 is an Error. A playbook with no
  `goal` is unaffected (backward compatible).

Steps:

1. Failing test `crates/apb-core/tests/suite/goal_schema_test.rs`:
```rust
use apb_core::schema::Playbook;
use apb_core::validate::{validate, Severity, ValidationContext};

fn codes(yaml: &str) -> Vec<&'static str> {
    let pb = Playbook::from_yaml(yaml).unwrap();
    validate(&pb, &ValidationContext::default())
        .issues.iter()
        .filter(|i| i.severity == Severity::Error)
        .map(|i| i.code)
        .collect()
}

const BASE: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\n\
nodes:\n  - id: start\n    type: start\n  - id: done\n    type: finish\n    outcome: success\n\
edges:\n  - { from: start, to: done }\n";

#[test]
fn a_playbook_without_a_goal_still_validates() {
    assert!(codes(BASE).is_empty());
}

#[test]
fn a_well_formed_goal_validates_and_round_trips() {
    let yaml = format!("{BASE}goal:\n  statement: The invoice is recorded and sent\n  criteria:\n    - description: A row with the amount appears in the sheet\n      check: {{ kind: marker, marker: INVOICE_ROW }}\n");
    assert!(codes(&yaml).is_empty());
    let pb = Playbook::from_yaml(&yaml).unwrap();
    let back = serde_yaml_ng::to_string(&pb).unwrap();
    assert!(back.contains("statement:"));
    assert!(back.contains("INVOICE_ROW"));
}

#[test]
fn v41_rejects_a_goal_with_no_criteria() {
    let yaml = format!("{BASE}goal:\n  statement: Something happened\n  criteria: []\n");
    assert!(codes(&yaml).contains(&"V41"));
}

#[test]
fn v41_rejects_an_empty_statement() {
    let yaml = format!("{BASE}goal:\n  statement: \"\"\n  criteria:\n    - description: X\n      check: {{ kind: manual }}\n");
    assert!(codes(&yaml).contains(&"V41"));
}
```
   Add the `mod` line to `crates/apb-core/tests/main.rs` (verify the exact form
   used there; follow the existing style in that file).

2. Run to fail: `cargo test -p apb-core --test main goal_schema_test::`.
   Expected: unresolved fields / `Playbook` has no field `goal`.

3. Implement:
   - In `schema.rs`, add the `Goal` / `GoalCriterion` / `GoalCheck` types and
     the `goal` field on `Playbook` (place it beside `effects`, keeping the
     additive `#[serde(default)]` pattern; `GoalCheck` defaults to `Manual` via
     a `#[serde(default)]` on the criterion's `check` field or a `Default`
     impl).
   - Add `crates/apb-core/src/validate/goal.rs` with `pub(crate) fn
     check_goal(playbook: &Playbook, r: &mut ValidationReport)` emitting V41,
     following the existing rule-family module style (see `nodes.rs`).
   - In `validate/mod.rs`, add `mod goal;`, `use goal::check_goal;`, and a
     `check_goal(playbook, &mut r); // V41` call in the unconditional block of
     `validate` (alongside `check_finish` etc., before the `is_valid` gate).

4. Run to pass. Expected: 4 passed.

5. Run the wider core suite once: `cargo test -p apb-core --test main`. fmt,
   clippy, code-ranker clean.

6. Commit:
```
git commit -s -m "feat(core): goal and machine-checkable goal criteria (schema + V41)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 11: Draft playbook assembly

Files:
- Create: `crates/apb-recorder/src/draft.rs`
- Modify: `crates/apb-recorder/src/lib.rs`
- Create: `crates/apb-recorder/tests/suite/draft_test.rs`
- Modify: `crates/apb-recorder/tests/main.rs`

Interfaces:
- Consumes: `&str` (playbook id), `&reconstruct::Reconstruction`,
  `&interview::InterviewAnswers`.
- Produces:
```rust
pub enum DraftError { Empty, Schema(String), Yaml(String) }
pub fn assemble_draft(id: &str, name: &str, recon: &Reconstruction, answers: &InterviewAnswers) -> Result<apb_core::schema::Playbook, DraftError>;
pub fn draft_to_yaml(pb: &apb_core::schema::Playbook) -> Result<String, DraftError>;
```
   Assembly rules: refuse an empty reconstruction with `DraftError::Empty` (no
   empty playbook, per spec). Build `schema: 2`, a `start` node, one node per
   step (an `agent_task` whose `prompt` is the step label plus description; a
   step with an `UnexplainedChoice` marker gets its clarifying answer appended
   to the prompt so a branch point is captured as an explicit note), edges
   chaining `start -> step-0 -> ... -> finish`, a `finish` node with
   `outcome: success`. Set `goal` from `answers.goal` (statement plus each
   criterion as a `GoalCriterion` with `check: Manual` for the MVP). Every
   distinct value in any step's `varies` becomes a `Param`. The result must pass
   `apb_core::validate::validate`; a validation failure returns
   `DraftError::Schema`.

Steps:

1. Failing test `crates/apb-recorder/tests/suite/draft_test.rs`:
```rust
use std::collections::BTreeMap;
use apb_core::validate::{validate, ValidationContext};
use apb_recorder::draft::{assemble_draft, draft_to_yaml, DraftError};
use apb_recorder::interview::{GoalDraft, InterviewAnswers};
use apb_recorder::model::Confidence;
use apb_recorder::reconstruct::{Reconstruction, ReconstructionSource, Step};

fn recon() -> Reconstruction {
    Reconstruction { source: ReconstructionSource::Model, steps: vec![
        Step { index: 0, label: "Open invoices".into(), description: "go to the invoices page".into(), reads: vec![], enters: vec![], varies: vec![], confidence: Confidence::High, markers: vec![] },
        Step { index: 1, label: "Enter amount".into(), description: "type the amount".into(), reads: vec![], enters: vec!["Amount".into()], varies: vec!["Amount".into()], confidence: Confidence::High, markers: vec![] },
    ] }
}
fn answers() -> InterviewAnswers {
    InterviewAnswers {
        step_answers: BTreeMap::new(),
        goal: Some(GoalDraft { statement: "The invoice is recorded".into(), criteria: vec!["A row with the amount appears in the sheet".into()] }),
    }
}

#[test]
fn assembled_draft_is_a_valid_playbook_with_goal_and_params() {
    let pb = assemble_draft("recorded-invoice", "Recorded invoice", &recon(), &answers()).unwrap();
    assert!(validate(&pb, &ValidationContext::default()).is_valid());
    assert!(pb.goal.is_some());
    assert_eq!(pb.goal.as_ref().unwrap().criteria.len(), 1);
    assert!(pb.params.iter().any(|p| p.name == "Amount"));
    let yaml = draft_to_yaml(&pb).unwrap();
    assert!(yaml.contains("schema: 2"));
    assert!(yaml.contains("Recorded invoice"));
}

#[test]
fn an_empty_reconstruction_is_refused() {
    let empty = Reconstruction { source: ReconstructionSource::Model, steps: vec![] };
    assert!(matches!(assemble_draft("x", "X", &empty, &answers()), Err(DraftError::Empty)));
}
```
   Confirm the `apb_core::schema::Param` field name (`name`) against
   `schema.rs`; adjust the assertion if the field differs. Add the `mod` line to
   `tests/main.rs`.

2. Run to fail. Expected: unresolved import `apb_recorder::draft`.

3. Implement `draft.rs`; add `pub mod draft;` to `lib.rs`. Build the `Playbook`
   struct directly (not via string concatenation) so field names are
   compiler-checked, then validate before returning.

4. Run to pass. Expected: 2 passed.

5. fmt, clippy, code-ranker clean. If `assemble_draft` trips complexity, extract
   `build_nodes`, `build_edges`, `build_goal`, `collect_params`.

6. Commit:
```
git commit -s -m "feat(recorder): assemble a reviewed reconstruction into a draft playbook

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 12: The local recorder store and the native-messaging codec

Files:
- Create: `crates/apb-recorder/src/store.rs`
- Create: `crates/apb-recorder/src/nativemsg.rs`
- Modify: `crates/apb-recorder/src/lib.rs`
- Create: `crates/apb-recorder/tests/suite/store_test.rs`
- Create: `crates/apb-recorder/tests/suite/nativemsg_test.rs`
- Modify: `crates/apb-recorder/tests/main.rs`

Interfaces:
- Produces (`store`):
```rust
pub enum StoreError { Io(String), Json(String) }
pub fn save_session(store_root: &std::path::Path, session: &RecordingSession) -> Result<std::path::PathBuf, StoreError>;
pub fn load_session(store_root: &std::path::Path, id: &str) -> Result<RecordingSession, StoreError>;
```
   `save_session` writes `<store_root>/sessions/<id>.json` via
   `apb_core::fsutil::atomic_write_private` (a recording is privacy-sensitive).
- Produces (`nativemsg`):
```rust
pub enum WireMessage {
    Hello { recorder_version: String },
    SyncPing { at_ms: u128 },
    SessionChunk { session_id: String, events: Vec<RawEvent> },
    SessionEnd { session_id: String },
    Ack { session_id: String, saved: bool },
}
pub fn write_frame<W: std::io::Write>(w: &mut W, msg: &WireMessage) -> std::io::Result<()>;
pub fn read_frame<R: std::io::Read>(r: &mut R) -> std::io::Result<Option<WireMessage>>;
pub fn host_manifest(host_name: &str, exec_path: &str, allowed_extension_ids: &[String]) -> String;
```
   Framing follows Chrome native messaging: a 4-byte native-endian `u32` length
   prefix, then that many bytes of UTF-8 JSON. `read_frame` returns `Ok(None)`
   on a clean EOF at a frame boundary (the port closed), and an error if EOF
   lands mid-frame (never a silent empty). `WireMessage` is
   `#[serde(tag = "type", rename_all = "snake_case")]`, additive fields only.
   `host_manifest` returns the Chrome host-manifest JSON
   (`name`, `description`, `path`, `type: "stdio"`, `allowed_origins` built as
   `chrome-extension://<id>/`).

Steps:

1. Failing tests.

`crates/apb-recorder/tests/suite/store_test.rs`:
```rust
use apb_recorder::raw::RecordingSession;
use apb_recorder::store::{load_session, save_session};

#[test]
fn a_saved_session_loads_back_identically() {
    let dir = tempfile::tempdir().unwrap();
    let session = RecordingSession { id: "sess-9".into(), started_ms: 5, events: vec![] };
    let path = save_session(dir.path(), &session).unwrap();
    assert!(path.exists());
    let back = load_session(dir.path(), "sess-9").unwrap();
    assert_eq!(session, back);
}
```

`crates/apb-recorder/tests/suite/nativemsg_test.rs`:
```rust
use apb_recorder::nativemsg::{host_manifest, read_frame, write_frame, WireMessage};

#[test]
fn a_frame_round_trips_through_the_codec() {
    let msg = WireMessage::SyncPing { at_ms: 123 };
    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &msg).unwrap();
    // 4-byte length prefix plus a non-empty body.
    assert!(buf.len() > 4);
    let mut cursor = std::io::Cursor::new(buf);
    let back = read_frame(&mut cursor).unwrap().unwrap();
    assert!(matches!(back, WireMessage::SyncPing { at_ms: 123 }));
    // A clean EOF at the next frame boundary is None, not an error.
    assert!(read_frame(&mut cursor).unwrap().is_none());
}

#[test]
fn a_truncated_length_prefix_is_an_error_not_a_silent_none() {
    let mut cursor = std::io::Cursor::new(vec![0x01, 0x00]); // 2 of 4 length bytes
    assert!(read_frame(&mut cursor).is_err());
}

#[test]
fn host_manifest_names_the_extension_origin() {
    let m = host_manifest("com.omniteam.apb_recorder", "/path/to/apb", &["abcdefghabcdefghabcdefghabcdefgh".into()]);
    assert!(m.contains("\"type\": \"stdio\""));
    assert!(m.contains("chrome-extension://abcdefghabcdefghabcdefghabcdefgh/"));
}
```
   Add both `mod` lines to `tests/main.rs`.

2. Run to fail. Expected: unresolved imports `apb_recorder::store` /
   `apb_recorder::nativemsg`.

3. Implement `store.rs` (using `apb_core::fsutil::atomic_write_private` and
   `serde_json`) and `nativemsg.rs`. In `read_frame`, read exactly 4 bytes; on
   0 bytes read return `Ok(None)`; on 1..4 bytes return an
   `io::Error` naming "truncated native-messaging length prefix"; then read
   exactly `len` body bytes, erroring on short read. Add `pub mod store;` and
   `pub mod nativemsg;` to `lib.rs`.

4. Run to pass. Expected: 4 passed.

5. fmt, clippy, code-ranker clean.

6. Commit:
```
git commit -s -m "feat(recorder): local session store and native-messaging codec

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 13: Wire the native-messaging host subcommand into the apb CLI

Files:
- Modify: `crates/apb-cli/Cargo.toml`
- Create: `crates/apb-cli/src/recorder_host.rs`
- Modify: `crates/apb-cli/src/main.rs`
- Create: `crates/apb-cli/tests/suite/recorder_host_test.rs`
- Modify: `crates/apb-cli/tests/main.rs`

Interfaces:
- Produces: a hidden `apb recorder-host` subcommand. It reads `WireMessage`
  frames from stdin and writes frames to stdout: on `Hello` it replies `Hello`
  with the recorder version; on each `SessionChunk` it appends to an in-memory
  session buffer; on `SessionEnd` it persists via
  `apb_recorder::store::save_session` under the recorder store root and replies
  `Ack { saved: true }`; on clean EOF it exits 0.
```rust
pub fn run_recorder_host<R: std::io::Read, W: std::io::Write>(store_root: &std::path::Path, input: &mut R, output: &mut W) -> std::io::Result<()>;
```
   The `main.rs` handler calls `run_recorder_host` with stdin/stdout and the
   resolved store root; the pure function is what the test drives.

Steps:

1. Failing test `crates/apb-cli/tests/suite/recorder_host_test.rs`:
```rust
use apb_recorder::nativemsg::{read_frame, write_frame, WireMessage};
use apb_recorder::raw::{ElementRef, RawEvent, RawEventKind};

#[test]
fn host_persists_a_session_and_acks() {
    let dir = tempfile::tempdir().unwrap();
    let mut input: Vec<u8> = Vec::new();
    write_frame(&mut input, &WireMessage::Hello { recorder_version: "x".into() }).unwrap();
    let ev = RawEvent {
        seq: 0, ts_ms: 1, raw_ts_ms: 1, page_url: "u".into(), frame_url: "u".into(),
        kind: RawEventKind::Click { element: ElementRef { tag: "b".into(), xpath: "/b".into(), css: "b".into(), label: "Save".into() } },
    };
    write_frame(&mut input, &WireMessage::SessionChunk { session_id: "s1".into(), events: vec![ev] }).unwrap();
    write_frame(&mut input, &WireMessage::SessionEnd { session_id: "s1".into() }).unwrap();

    let mut cursor = std::io::Cursor::new(input);
    let mut out: Vec<u8> = Vec::new();
    // The subject is pure IO over in-memory buffers: it must return on EOF, not
    // block, so no deadline wrapper is needed (bounded by construction).
    apb::recorder_host::run_recorder_host(dir.path(), &mut cursor, &mut out).unwrap();

    let mut reader = std::io::Cursor::new(out);
    let hello = read_frame(&mut reader).unwrap().unwrap();
    assert!(matches!(hello, WireMessage::Hello { .. }));
    let ack = read_frame(&mut reader).unwrap().unwrap();
    assert!(matches!(ack, WireMessage::Ack { saved: true, .. }));

    let saved = apb_recorder::store::load_session(dir.path(), "s1").unwrap();
    assert_eq!(saved.events.len(), 1);
}
```
   Add the `mod` line to `crates/apb-cli/tests/main.rs`. Confirm the crate path:
   the cli package is `apb`, so the test refers to `apb::recorder_host::...`;
   that requires `recorder_host` to be a public module of the `apb` library
   target. If `apb-cli` is a pure binary with no lib target, instead expose
   `run_recorder_host` from `apb-recorder` and have the test call it there;
   adjust the import accordingly. Verify against `crates/apb-cli/src/main.rs`
   before writing.

2. Run to fail. Expected: unresolved path `apb::recorder_host` (or the
   `apb-recorder` fallback path).

3. Implement:
   - Add `apb-recorder = { path = "../apb-recorder" }` to `apb-cli`'s
     `[dependencies]`.
   - Add `crates/apb-cli/src/recorder_host.rs` with `run_recorder_host`,
     looping `read_frame` until `Ok(None)`.
   - Register a hidden `recorder-host` subcommand in `main.rs` that resolves the
     store root and calls `run_recorder_host(&root, &mut stdin.lock(), &mut
     stdout.lock())`.

4. Run to pass: `cargo test -p apb --test main recorder_host_test::`. Expected:
   1 passed.

5. fmt, clippy, code-ranker clean. Confirm no import cycle: `apb-cli ->
   apb-recorder -> apb-core` is acyclic.

6. Commit:
```
git commit -s -m "feat(cli): apb recorder-host native-messaging subcommand

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 14: The MV3 browser extension (capture)

Files:
- Create: `extension/package.json`
- Create: `extension/wxt.config.ts`
- Create: `extension/tsconfig.json`
- Create: `extension/vitest.config.ts`
- Create: `extension/entrypoints/background.ts`
- Create: `extension/entrypoints/content.ts`
- Create: `extension/entrypoints/popup/index.html`
- Create: `extension/entrypoints/popup/main.ts`
- Create: `extension/lib/events.ts`
- Create: `extension/lib/capture.ts`
- Create: `extension/lib/nativemsg.ts`
- Create: `extension/lib/capture.test.ts`
- Create: `extension/lib/events.test.ts`

Interfaces:
- `extension/lib/events.ts` mirrors the Rust raw serde contract exactly:
```ts
export interface ElementRef { tag: string; xpath: string; css: string; label: string }
export type RawEventKind =
  | { type: 'click'; element: ElementRef }
  | { type: 'key_down'; key: string; element: ElementRef | null }
  | { type: 'text_input'; element: ElementRef; value: string }
  | { type: 'change'; element: ElementRef; value: string }
  | { type: 'navigate'; url: string }
  | { type: 'tab_activated'; tab_id: number }
  | { type: 'tab_closed'; tab_id: number }
  | { type: 'scroll'; x: number; y: number }
  | { type: 'clipboard_write'; text_len: number }
  | { type: 'pointer_move'; x: number; y: number }
  | { type: 'capture_gap'; reason: string }
export interface RawEvent { seq: number; ts_ms: number; raw_ts_ms: number; page_url: string; frame_url: string; kind: RawEventKind }
```
- `extension/lib/capture.ts` pure helpers (no chrome APIs, DOM only):
```ts
export function isRedactedField(el: Element): boolean // input[type=password] -> true
export function semanticLabel(el: Element): string    // aria-label | placeholder | nearby text | textContent
export function cssSelector(el: Element): string
export function xpath(el: Element): string
export function captureClick(el: Element): RawEventKind // { type: 'click', element }
export function captureInput(el: HTMLInputElement): RawEventKind | null // null for redacted fields
```
- `extension/lib/nativemsg.ts` wraps `chrome.runtime.connectNative` (Chrome does
  the framing; the extension posts and receives JS objects), exporting
  `connectRecorderHost(): chrome.runtime.Port`.

Steps:

1. Scaffold with WXT and write the failing vitest specs first.

`extension/package.json`:
```json
{
  "name": "apb-recorder-extension",
  "private": true,
  "type": "module",
  "scripts": {
    "dev": "wxt",
    "build": "wxt build",
    "test": "vitest run"
  },
  "devDependencies": {
    "wxt": "^0.20.0",
    "vitest": "^4.1.10",
    "happy-dom": "^15.0.0",
    "typescript": "~6.0.2"
  }
}
```

`extension/vitest.config.ts`:
```ts
import { defineConfig } from 'vitest/config'
export default defineConfig({ test: { environment: 'happy-dom' } })
```

`extension/lib/events.test.ts`:
```ts
import { describe, it, expect } from 'vitest'
import type { RawEvent } from './events'

describe('events contract', () => {
  it('serializes with the snake_case tag the Rust core expects', () => {
    const ev: RawEvent = {
      seq: 0, ts_ms: 1, raw_ts_ms: 1, page_url: 'u', frame_url: 'u',
      kind: { type: 'click', element: { tag: 'button', xpath: '/b', css: 'b', label: 'Save' } },
    }
    const json = JSON.parse(JSON.stringify(ev))
    expect(json.kind.type).toBe('click')
    expect(json.kind.element.label).toBe('Save')
  })
})
```

`extension/lib/capture.test.ts`:
```ts
import { describe, it, expect } from 'vitest'
import { captureInput, isRedactedField, semanticLabel } from './capture'

describe('capture', () => {
  it('never captures a password field', () => {
    const input = document.createElement('input')
    input.type = 'password'
    input.value = 'hunter2'
    expect(isRedactedField(input)).toBe(true)
    expect(captureInput(input)).toBeNull()
  })

  it('captures a normal field value with its semantic label', () => {
    const input = document.createElement('input')
    input.type = 'text'
    input.setAttribute('aria-label', 'Amount')
    input.value = '100'
    document.body.appendChild(input)
    expect(isRedactedField(input)).toBe(false)
    expect(semanticLabel(input)).toBe('Amount')
    const ev = captureInput(input)
    expect(ev).not.toBeNull()
    if (ev && ev.type === 'text_input') {
      expect(ev.value).toBe('100')
      expect(ev.element.label).toBe('Amount')
    }
  })
})
```

2. Run to fail: `cd extension && bun install && bun run test`. Expected: module
   `./capture` / `./events` not found.

3. Implement `events.ts` and `capture.ts` (pure, DOM-only, no chrome APIs), then
   `wxt.config.ts` (MV3 manifest: permissions `activeTab`, `scripting`,
   `nativeMessaging`; NOT the `incognito` permission, so the extension is
   never granted incognito access), `background.ts` (recording lifecycle;
   refuses to attach when `sender.tab?.incognito` or the window is incognito;
   buffers events; delete-last-30s trims events by `ts_ms`; delete-all clears
   the buffer; on stop, streams `SessionChunk` then `SessionEnd` over the native
   port), `content.ts` (listens for click/input/keydown/scroll, builds
   `RawEvent`s via `capture.ts`, skips redacted fields, forwards to background),
   `nativemsg.ts`, and the popup (a visible recording indicator plus start,
   stop, delete-last-30s, delete-all controls).

4. Run to pass: `cd extension && bun run test`. Expected: both specs green.
   Confirm a production build works: `cd extension && bun run build`.

5. The extension is standalone (not embedded via rust-embed and not part of
   `web/dist`), so it does not affect the Rust release. No Rust gates apply
   here; run `bun run test` and the build only.

6. Commit:
```
git commit -s -m "feat(extension): MV3 capture with incognito and password exclusion

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 15: End-to-end, a recorded fixture drives a draft that runs under the engine

Files:
- Create: `crates/apb-recorder/tests/suite/e2e_test.rs`
- Create: `crates/apb-recorder/tests/fixtures/e2e_invoice_session.json`
- Modify: `crates/apb-recorder/tests/main.rs`
- Modify: `crates/apb-recorder/tests/suite/common/mod.rs` (add a stub-agent helper if needed)

Interfaces:
- Consumes: the fixture session, `FakeModelPass` with a canned reconstruction,
  `apb_recorder::draft::assemble_draft`, `apb_core::versioning::create_version`
  (to write the draft into a temp registry), and `apb_engine::scheduler::run`
  (public entry at `crates/apb-engine/src/scheduler/entry.rs:90`, re-exported as
  `apb_engine::scheduler::run` with `RunOptions`) to drive the drafted playbook
  against an emulated scenario using a stub agent shell script.

Steps:

1. Failing test `crates/apb-recorder/tests/suite/e2e_test.rs`. It (a) loads the
   fixture session, (b) reconstructs it with a `FakeModelPass` (no real model),
   (c) assembles a draft with a goal, (d) writes it to a temp registry via
   `create_version`, (e) drives it with the engine using a stub agent, (f)
   asserts the run reached a success outcome. Follow the engine suite's
   stub-agent pattern (a shell script registered through a `GlobalConfig` agent
   entry; see `crates/apb-engine/tests/suite/common/mod.rs` and
   `crates/apb-engine/tests/suite/profile_run_test.rs` for the exact wiring).
   The stub simply reports success and emits the expected node output. Skeleton:
```rust
use apb_recorder::draft::{assemble_draft, draft_to_yaml};
use apb_recorder::interview::{GoalDraft, InterviewAnswers};
use apb_recorder::model::{Confidence, FakeModelPass, ModelReconstruction, ModelStep};
use apb_recorder::raw::RecordingSession;
use apb_recorder::reconstruct::reconstruct;
use crate::common::fixture_path;
use std::collections::BTreeMap;

#[test]
fn a_recorded_session_becomes_a_draft_that_runs_to_success() {
    let text = std::fs::read_to_string(fixture_path("e2e_invoice_session.json")).unwrap();
    let session: RecordingSession = serde_json::from_str(&text).unwrap();

    let canned = ModelReconstruction { steps: vec![ModelStep {
        segment_index: 0, name: "Record invoice".into(), description: "record it".into(),
        reads: vec![], enters: vec!["Amount".into()], varies_per_run: vec!["Amount".into()],
        confidence: Confidence::High, unexplained_choice: false, limited_context: false,
    }] };
    let recon = reconstruct(&session, &FakeModelPass { result: Ok(canned) });
    let answers = InterviewAnswers {
        step_answers: BTreeMap::new(),
        goal: Some(GoalDraft { statement: "Invoice recorded".into(), criteria: vec!["Row present".into()] }),
    };
    let pb = assemble_draft("recorded-invoice", "Recorded invoice", &recon, &answers).unwrap();
    let yaml = draft_to_yaml(&pb).unwrap();

    // Temp registry root; write the draft as a first version.
    let root = tempfile::tempdir().unwrap();
    let version = apb_core::versioning::create_version(root.path(), "recorded-invoice", &yaml, None, true).unwrap();
    assert!(!version.is_empty());

    // Drive it under the engine with a stub agent. Bounded by construction: the
    // stub returns immediately; the engine `run` returns a terminal RunResult.
    // Wire the stub via the engine suite's documented pattern. Assert the run
    // outcome is success (RunStatus::Succeeded).
    // let result = apb_engine::scheduler::run(RunOptions { .. })?;
    // assert!(matches!(result.outcome, RunStatus::Succeeded));
}
```
   Complete the commented drive block using the engine's public `run` API and a
   stub agent; keep every wait bounded and name what it waited on. Add the
   fixture and the `mod` line to `tests/main.rs`.

2. Run to fail: `cargo test -p apb-recorder --test main e2e_test::`. Expected:
   compile error on the incomplete drive block, then a real assertion failure
   until the draft-to-run path is correct.

3. Make it pass by completing the drive wiring. If the drafted `agent_task`
   prompt needs a profile, register a minimal stub profile in the temp registry
   the same way the engine suite does; keep the stub deterministic and offline.

4. Run to pass. Expected: 1 passed. Then run the full recorder suite once:
   `cargo test -p apb-recorder --test main`.

5. Run `cargo nextest run -p apb-recorder -p apb-core` and confirm no test is
   reported SLOW. fmt, clippy across the workspace, code-ranker clean.

6. Commit:
```
git commit -s -m "test(recorder): end-to-end recorded session to a draft that runs under the engine

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Spec coverage map

- Browser-only MV3 capture, recording indicator, incognito never recorded,
  password fields never recorded: Task 14.
- Record-on-demand, delete-last-30s, delete-all: Task 14 (background + popup).
- Native-messaging transport, local-by-default persistence: Tasks 12, 13, 14.
- Deterministic consolidation (fold typing, drop pointer moves, coalesce
  clicks, aggregate scrolls, dedup): Task 3.
- Heuristic segmentation (navigation, tab, clipboard, commit): Task 4.
- Compact segmented document the model consumes, never the raw stream: Task 5.
- Single model pass, late, as a trait with a deterministic fake; real adapter
  never called in tests: Task 6.
- Honesty rules (capture-quality / limited-context marking, halt-do-not-guess as
  a question, per-step confidence, unexplained-choice markers, irreversible-step
  marking): Tasks 6, 7, 8, 9.
- Model-pass failure falls back to a deterministic step list; empty recording
  produces no draft: Tasks 7, 11.
- Show protocol (human-readable step list): Task 8.
- Clarifying interview, including the goal-criteria elicitation question: Task 9.
- Explicit goal plus verifiable, machine-checkable goal criteria as first-class
  schema, validator-checked (V41): Task 10.
- Draft playbook assembly into the apb schema, params for varying values, branch
  points captured as explicit notes, handed to the registry: Tasks 11, 15.
- End-to-end on an emulated scenario, no live users: Task 15.

Out-of-scope items (absent by construction): desktop capture, replay /
smart-repeat, always-on observation, vision/OCR, automatic PII masking,
per-site exclusion lists, cloud-redaction detail, multi-user collaboration, and
the interactive review-screen UI (review is driven by a structured
`InterviewAnswers` value in this emulation-first MVP; see the decisions section).
The
supervisor hard-rule ("may repair the process but never the goal criteria") is
a runtime rule of the existing engine and supervisor tooling, not a recorder
deliverable; this plan makes the goal a first-class, validated part of the
playbook (Task 10) so that rule has something concrete to protect, but does not
re-implement the supervisor.
