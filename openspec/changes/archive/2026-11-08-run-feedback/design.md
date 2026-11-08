# Design: run feedback that outlives the run

Read `proposal.md` first. Every decision below names its files. Line
numbers are from origin/main a118ae4e and were checked; find the code by
the names if they moved. This is PR 3 of three: it is implemented after PR 1
`node-contracts` and PR 2 `recurring-routines` are merged, and both moved
lines. Re-find every site by its name.

## D0. Dependency on PR 1, and nothing else

This change reads exactly one thing PR 1 adds: an optional field on
`EventPayload::AttemptFinished` (`crates/apb-engine/src/event.rs` line 223):

```rust
/// A normalized hash of `rejected_for` (PR 1). Optional.
rejection_fingerprint: Option<String>,
```

1. Only D2 reads it, as the grouping key of a `rejection` entry when present.
2. Before section 3 of `tasks.md`, confirm PR 1 is merged and the field
   exists under this name (`rg -n rejection_fingerprint crates/apb-engine/src/event.rs`).
   If it has another name, use that name and say so in the final report. If
   it does not exist, every `rejection` entry falls back to the token key of
   D2 over `rejected_for`, and the final report says the fingerprint path is
   unused.
3. Do not call any PR 1 function, and do not reimplement its normalization:
   when the field is absent on an event (an older run), the token key of D2
   applies. The two keys are told apart by their prefix (`f:` and `t:`), so
   they never collide; a rejection seen in old and new runs may form two
   entries, which is acceptable.

Nothing from PR 2 is used. If PR 2 added a `Transition` variant, a
`GuardedFeature`, an `EventPayload` variant or validator codes, keep them and
add this change's next to them.

## D1. Scenario capture from a finished run

### The scripted timeout (format change)

A real attempt times out when the adapter returns
`AgentFailure { class: ErrorClass::Timeout, .. }`
(`crates/apb-engine/src/adapter/failure.rs` line 7): the attempt loop
journals `attempt_finished.status: "timed_out"`
(`crates/apb-engine/src/scheduler/node.rs` lines 2961 to 2966), and a node
whose last attempt timed out finishes `timed_out` (line 3504). The scenario
format has no way to say this today. Add it to the attempt answer:

`crates/apb-core/src/scenario/format.rs`, `AttemptAnswer` (line 220), after
`read_only` (line 241):

```rust
/// The attempt ends as a timeout at once: the scripted agent reports what
/// an agent killed at its deadline reports, and nothing waits. Left out of
/// the serialized form when false, so the digest of a scenario without it
/// is unchanged.
#[serde(default, skip_serializing_if = "std::ops::Not::not")]
pub timed_out: bool,
```

YAML: `- { timed_out: true }`, optionally with `output` (the text the
attempt left, as a real timeout's partial output). Add it to the worked
example of `format.rs` tests.

Rules, in `crates/apb-core/src/scenario/validate.rs`:

1. V146 (`attempt_shape`, line 123): `timed_out: true` together with
   `status`, `failure_kind`, `outputs`, `check`, `read_only` or `question`
   is an error (`a timed-out attempt carries only an optional output`).
   It may stand as the `then` of a question.
2. `counted_attempts` (line 445) counts a timed-out attempt the way the
   drive spends a retry on it. Read the attempt loop in `scheduler/node.rs`
   around the timeout continuation (`failure_class::TIMEOUT_CONTINUATION_ACTION`,
   `failure_class.rs` line 379) first: if a timeout is continued or retried
   without spending a retry, leave it out of the count as transient and
   quota are; otherwise count it. Say which in the final report.
3. Update the module docs (V146 list).

The adapter, `crates/apb-engine/src/scenario/adapter.rs`, in `answer`
right after the question branch (line 78) and before the status match
(line 86):

```rust
if a.timed_out {
    let detail = TIMEOUT_DETAIL; // "scenario: the attempt reached its deadline"
    let message = match a.output.as_deref() {
        Some(o) if !o.is_empty() => format!("{detail}: {o}"),
        _ => detail.to_string(),
    };
    return Err(AgentFailure::new(ErrorClass::Timeout, message));
}
```

Everything after the attempt (the `timed_out` status, the node's final
status, retries, fallbacks, the timeout continuation, the supervisor wake
`NodeTimeout`) is the real drive's, exactly as for a real timeout. No sleep,
no deadline watcher: the adapter returns at once.

Capture rule (below, point 1): an `attempt_finished` with `status:
"timed_out"` is captured as `{ timed_out: true }` (with `partial_output`,
scrubbed, as `output` when present), whatever its `failure_kind`.

Docs: `docs/EVALS.md`, the attempt answer fields.

Wrong implementations to avoid:

1. A new `ScriptedStatus::TimedOut` (the enum is shared with child and
   publish answers, which then accept a status they cannot have) or a
   `failure_kind: timeout` (a timeout is a status, not a failure class:
   `failure_class::classify` would read the detail as transient). Test: a
   child answer with `status: timed_out` is still a parse error.
2. `timed_out` serialized when false. Test: the digest of the worked
   example without the field equals a constant taken before the change,
   and `serde_json::to_value` of a default `AttemptAnswer` has no
   `timed_out` key.
3. The adapter returning `ErrorClass::ProcessExit` with a timeout-looking
   message (the attempt is then `failed`). Test: a quick scenario whose only
   attempt is `{ timed_out: true }` on a node with `max_retries: 0` ends
   with `expect.status: { node: timed_out }` met.
4. Sleeping until the node's timeout. Test: that scenario plays in well
   under the node's `timeout_seconds` (set it to 600 and bound the test).
5. A timed-out attempt accepted with `status` or `outputs`. Test: V146.

### Who builds it

New file `crates/apb-engine/src/scenario/capture.rs`, declared `pub mod
capture;` in `crates/apb-engine/src/scenario/mod.rs` next to `pub mod bind;`
(line 16). It is a pure fold of what a run left plus one read of its run
config; it writes nothing.

```rust
pub struct CaptureRequest {
    /// The scenario name (`apb_core::scenario::is_valid_name`).
    pub name: String,
    /// Nodes whose `expect.status` and `expect.visits` are asserted; empty:
    /// every node that has a `node_finished`.
    pub nodes: Vec<String>,
}

pub struct Capture {
    pub scenario: apb_core::scenario::Scenario,
    /// `yaml_serde::to_string(&scenario)`: the exact text a save writes.
    pub yaml: String,
    /// `apb_core::scenario::scenario_digest(&scenario)`.
    pub digest: String,
    /// The playbook id and the version the run executed.
    pub playbook: String,
    pub version: String,
    /// Texts the scrubber changed, counted (never listed).
    pub redacted: usize,
    /// One line per fact a person should know before saving (the run's
    /// version is not `current`, a param was withheld).
    pub warnings: Vec<String>,
}

pub struct Gap { pub code: &'static str, pub node: Option<String>, pub detail: String }

pub enum CaptureError {
    /// The run cannot be captured; every gap, sorted by node then code.
    NotCapturable(Vec<Gap>),
    Engine(EngineError),
}

pub fn capture(root: &Path, run_dir: &Path, req: &CaptureRequest) -> Result<Capture, CaptureError>
```

### Refusals before any reading

Each is a `Gap` with `node: None`; all are collected, not the first only:

| code | when |
|---|---|
| `not_finished` | `RunState::fold` (`state.rs`) is not `Succeeded` or `Failed` |
| `aborted` | the run ended with `run_aborted`: a scenario has no answer that stops a run |
| `journal_unreadable` | `crate::event::read_all` fails: an event type this apb does not know after the last checkpoint, a torn or broken line |
| `unknown_events` | `crate::event::read_journal` reports any skipped event of an unknown type, wherever it sits (its meaning cannot be replayed) |
| `not_production` | the manifest origin is not `RunOrigin::Production` (`manifest.rs` line 407, `RunOrigin::of_read`) |
| `secrets_unresolved` | `RunRedactor::for_run(root, run_dir).scrubber()` (`run_redact.rs` lines 499, 666) is `Err` |
| `host_mode` | the journal has a `host_task_requested` |
| `baseline_enforced` | the run's playbook snapshot (`legacy_snapshot::load_run_playbook`, line 55) has `baseline_check.enforce: true` |
| `directive` | a `supervisor_action` whose `action` is `node_retry` or `run_continue_from`, a `patch_applied`, a `profile_rebound` or a `nodes_revived` |
| `param_withheld` | a param `run_params::sensitive` flags (`crates/apb-core/src/run_params.rs` line 41) is declared without a default |

The journal is read with the strict `crate::event::read_all` (`event.rs`
line 3433), never `read_all_lossy_tail`: that reader skips unknown events
and forgives a torn last line (line 3537), which would silently drop facts
from a capture that must be whole. `read_journal` is called once as well,
only to see whether it skipped any unknown event (`unknown_events`).

### Answers, per node kind

Visits are the execution ordinals of `crate::identity::index(&events)`
(`identity.rs` line 85, `ExecutionIndex::executions`, `Execution.ordinal`),
written as `Visit::Nth(ordinal)`. Attempts of an execution are its
`attempt_finished` events in journal order whose seq lies in the execution
(`ExecutionIndex::containing`, line 134).

1. `agent_task` (not a list node) and `finish` with a prompt: one
   `AttemptAnswer` per `attempt_finished`:
   - `status: succeeded` with no `rejected_for`: `status: succeeded`. When
     the execution's `node_finished.output` parses as a JSON object, that
     object goes into `outputs` and `output` is left out (the scripted agent
     writes it to the status file and the engine stores it as compact JSON,
     `scenario/adapter.rs` `write_verdict`); otherwise `output` is the text.
     A success that apb accepted ends the execution, so it is always the
     execution's last attempt, and `node_finished.output` is its output.
     A reply that is a JSON object without a status file replays through
     `outputs` too: the fields a template reads are the same, only the key
     order of the stored text may differ.
   - `rejected_for` set: `status: succeeded`, `output` the attempt's
     `rejected_output` when present, `check: reject`.
   - `rejected_output` set but `rejected_for` absent: a fatal rejection
     (`rejected_for` is written only for a non-fatal one). The format has
     no fatal check answer: gap `fatal_rejection`.
   - `status: failed` with `failure_kind` `transient`, `quota`, `auth`,
     `budget` or `agent`: `status: failed`, `failure_kind` the same word
     (`ScriptedFailure`, `format.rs`), no output.
   - `status: failed` with no `failure_kind`: `status: failed` (the agent
     reported a failure).
   - a `read_only_violated` before the attempt's `attempt_finished` (same
     node, same execution): `read_only: { dirty: <its paths> }` on the
     attempt, the other fields as above.
   - a `question_asked` inside the attempt: the attempt becomes
     `{ question: { text, answer | timeout: true }, then: <the rest> }`,
     answer from the matching `question_answered` (`answered_by: timeout`
     gives `timeout: true`). Nested questions nest through `then`.
   - `status: timed_out`: `{ timed_out: true }`, with the attempt's
     `partial_output` (scrubbed) as `output` when present (see "The
     scripted timeout").
   - any other status (`interrupted`, `cancelled`, an unknown word): gap
     `attempt_status` naming the node and the word.
2. `script`: `ScriptAnswer { exit, stdout }` from the execution's
   `node_finished`: `exit: 0` for `succeeded`, `1` for `failed`,
   `stdout` the output. `timed_out`: gap `attempt_status`.
3. `human_review`: `ReviewAnswer { decision, note }` from the execution's
   `review_decided` (note left out when empty). A `review_withdrawn`
   instead: gap `directive`.
4. `judge` node: `JudgeAnswer::Answers` from the execution's
   `decision_made` with `use_site` of the judge node use, each
   `DecisionAnswer` as `QuestionAnswer { value, p, confidence }`; an
   `error` of `unavailable`, `timeout`, `rate_limited`, `auth`, `budget` or
   `invalid` gives `JudgeAnswer::Unavailable` with that kind; `cancelled`
   gives gap `judge_cancelled`. Confirm the judge node's `use_site` string
   in `crates/apb-engine/src/scheduler/judge.rs` before matching on it.
5. Judge edges of any node: `judge_edges` keyed by the edge address from
   `decision_made.edge_addresses` (question id to address), each answer's
   `p`. A decision without `edge_addresses` (an older journal): gap
   `judge_edge_address`.
6. `wait`: `wait_timeout` gives `timed_out: true`; otherwise `elapsed:
   true` for a timer wait; a signalled wait (`wait_signalled`) gives
   `signal: <node_finished.output as JSON, or as a string>`. First confirm
   in `crates/apb-engine/src/scheduler/` (the wait node arm) that a signalled
   wait's `node_finished.output` is the signal payload; if it is not, a
   signalled wait is gap `wait_signal`.
7. `playbook` (child): `ChildAnswer::Result { status, output }` from the
   execution's `node_finished` (`output` parsed as JSON, else a string).
8. `publish`: `PublishAnswer { decision, status }` from the node's
   `review_decided` (the one with `plan_digest`) and its `node_finished`.
9. A list node (`node.list.is_some()`): gap `list_node`.
10. An execution settled from the node cache (a `node_cache_hit` in it,
   `scheduler.rs` line 1678: the node finishes with no attempt): gap
   `cache_hit`. A scenario run bypasses the cache (`BoundaryKind::NodeCache`),
   so the replay would ask for an attempt the journal does not hold.
11. `start`, `finish` without prompt: no answer.

Baseline: `baseline: skip` when the playbook has a `baseline_check` (its
comparisons are not asserted). Goal: `goal` from each `goal_checked` whose
`check` is `script` (`passed` gives `pass`, else `fail`).

### Run fields

- `schema: 1`, `id` and `title: "Captured from run <run id>"`.
- `instruction`, `params` and `unattended` from `crate::run_config::read_run_config`
  (`run_config.rs` line 322; fields `instruction` line 148, `params` line
  146, `unattended` line 255). A param that `run_params::sensitive` flags,
  or that the playbook declares `secret`, is left out with a warning.
- `decisions`: the `mode` of the run's first judge node and judge edge
  `decision_made`, when present.
- `on_missing: error`, `on_extra: error`. No `versions`, no
  `playbook_digest`: the scenario is meant for later versions. When the
  run's version is not `current`, a warning says so.

### Expectations

- `expect.run.outcome`: `[succeeded]` or `[failed]` from the fold.
- `expect.finish`: the finish node the run ended at, when one finished.
- `expect.status`: per node in scope, the `node_finished.status` of its last
  execution (the words V150 accepts).
- `expect.visits`: per node in scope, the number of executions.
- `expect.templates_empty: report`: a real run may legitimately render an
  empty reference; the capture does not turn that into a failure.
- No `route.path` (settlement order of a parallel batch is not stable), no
  `edges`, no `joins`.

Nodes in scope: `req.nodes` when not empty (each must exist in the
playbook, else gap `unknown_node`), else every node with a `node_finished`.

### Scrubbing and redaction

1. Every string that goes into the scenario (outputs, review notes,
   question texts and answers, rejected outputs, the instruction, param
   values, script stdout) goes through the run's scrubber from step
   "Refusals" (`Scrubber::scrub`, `crates/apb-core/src/scrub.rs`), and every
   JSON value through `Scrubber::redact_value` (line 411) before it is
   placed. Count each text the scrubber changed in `redacted`.
2. After building, `Scrubber::refuses_value` (line 445) over
   `serde_json::to_value(&scenario)`: anything still flagged is the gap
   `credential_shaped` (no path or text in the detail, only the node).
3. The YAML is produced only by `yaml_serde::to_string`, never by string
   concatenation, so captured text cannot add keys to the file.
4. Then `apb_core::scenario::parse(&yaml, &name)` must succeed and
   `validate_scenario(&name, &scenario, &playbook)` against the run's
   playbook snapshot must report no error; an error is a bug, returned as
   `CaptureError::Engine(EngineError::Invalid(..))` naming the codes.

A redaction can change routing (an `output_field` edge that read the
redacted value). The preview says so in a warning whenever `redacted > 0`.

### Saving

New function in `crates/apb-core/src/scenario/mod.rs`:

```rust
/// Writes a new scenario file `<playbook_dir>/evals/scenarios/<name>.yaml`
/// atomically. Refuses an invalid name, an existing file, and a link below
/// `playbook_dir`. Never overwrites.
pub fn write_new(playbook_dir: &Path, name: &str, yaml: &str) -> Result<PathBuf, String>
```

It uses `crate::fsutil::ensure_no_symlink_below` (as `read_capped` does)
and creates the directories. It must not use `fsutil::atomic_write`: its
rename replaces an existing target, so two saves of one name would
overwrite each other. Add to `crates/apb-core/src/fsutil.rs`:

```rust
/// Writes `bytes` to `path` only if `path` does not exist: a synced
/// temporary file in the same directory, then `std::fs::hard_link` to
/// `path` (fails with `AlreadyExists` when the target exists), then the
/// temporary removed and the directory synced. The file mode is the
/// default (not private). Never replaces a file.
pub fn write_new_no_clobber(path: &Path, bytes: &[u8]) -> io::Result<()>
```

`AlreadyExists` is the refusal `scenario_exists`.

### The tool layer

New file `crates/apb-mcp/src/tools/scenario_capture.rs`, declared in
`tools/mod.rs`:

```rust
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
pub struct CaptureArgs {
    pub name: String,
    #[serde(default)] pub nodes: Vec<String>,
    /// Write the scenario. Requires `digest`. Refused on MCP.
    #[serde(default)] pub save: bool,
    /// The digest of the preview the person read.
    #[serde(default)] pub digest: Option<String>,
}

#[derive(Serialize)] #[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CapturePreview {
    pub run_id: String, pub playbook: String, pub version: String,
    pub name: String, pub yaml: String, pub digest: String,
    pub redacted: usize, pub warnings: Vec<String>,
    /// Set after a save: where the file was written.
    #[serde(skip_serializing_if = "Option::is_none")] #[cfg_attr(feature = "ts", ts(optional))]
    pub saved: Option<String>,
}

pub fn capture(root: &Path, run_id: &str, args: &CaptureArgs, step: &StepCaller)
    -> Result<CapturePreview, ToolError>
```

0. `capture` takes `channel: SaveChannel` (`Person` for the CLI on a
   terminal or with `--yes`, and the dashboard; `Agent` for MCP). `save:
   true` from `Agent` is refused `save_by_person` (409): an agent outside a
   run can read the digest, so a digest proves nothing about consent.
1. A preview is allowed anywhere except a run step:
   `outside_run(step, "capturing a scenario")` (`tools/mod.rs` line 72)
   first, for preview and save alike (the capture reads run text that an
   agent step must not be handed back as a tool answer to plant a file).
2. `save: true` recomputes the capture, compares `digest` with the new
   digest (`ct_eq_str` is not needed: digests are public), and refuses
   `stale_preview` (409) when they differ or `digest` is missing. Then
   `write_new` in the playbook directory the run's playbook resolves to
   (`Registry` of the run's scope: project, or the global store for a
   global playbook, `apb_core::scenario::scenarios_dir`).
3. `NotCapturable` maps to `ToolError::Conflict` whose message starts with
   `not_capturable:` and lists `code (node): detail` per gap.

Surfaces, all through this one function:

1. CLI: new `crates/apb-cli/src/scenario_capture.rs` and a top-level
   subcommand in `crates/apb-cli/src/main.rs` (next to `Eval`, line 625):

   ```text
   apb scenario capture <run-id> --name <slug> [--node <id> ...] [--save] [--yes] [--json]
   ```

   Without `--save` it prints the preview (YAML, warnings, the redacted
   count, the digest) and, on a terminal, asks `Save as <path>? [y/N]`; a
   yes saves with the preview's digest. `--save` without a terminal needs
   `--yes`. Exit 0 saved or previewed, 2 refused, 1 an error.
2. MCP: `run_scenario_capture { run_id, name, nodes? }` in
   `crates/apb-mcp/src/server/scenario.rs`, preview only (the args type
   for MCP has no `save` or `digest`). Its description says: show the
   person the YAML; they save it with `apb scenario capture` or on the
   dashboard. It writes nothing, so it is `ReadOnlyUse::Full` in
   `crates/apb-mcp/src/server/read_only.rs` `READ_ONLY` (line 28).
3. HTTP: `POST /api/runs/{id}/scenario-capture` with `CaptureArgs` as the
   body, in `crates/apb-server/src/routes/scenario.rs`; 409 for a refusal,
   422 for an unknown field.
4. Dashboard: on a finished production run's page, a "Capture as scenario"
   button opens a box: a name field (the grammar checked as typed), the
   preview YAML in a `<pre>`, the warnings, then "Save" which posts with
   the digest. New `web/src/lib/ScenarioCaptureBox.svelte` and its test.
   Plain CSS only.

TypeScript: register `CapturePreview` and `CaptureArgs` in the manual
list of `crates/apb-server/src/ts_contract.rs` (from line 44), then
regenerate.

Docs: `docs/EVALS.md`, a section "Capturing a run as a scenario" after
"Scenario runs": the command, what is captured, the gap codes, that the
capture asserts what the run did and the person edits `expect` to what the
playbook should do.

Wrong implementations to avoid:

1. Writing the file on the first call (no preview, or a preview not bound
   by digest). Test: `save: true` with a digest of another capture is
   refused and no file exists.
2. Capturing part of a run and leaving the rest to `on_missing`. Test: a
   run with one list node and one agent task is refused with `list_node`
   and writes nothing.
3. A rejected attempt scripted as `status: failed`. Test: a run whose
   success check rejected attempt 1 replays (quick level) with the same
   node status and visit count, and its attempt 1 answer has `check:
   reject`.
4. The node output put in `output` when it is a JSON object (the replay
   then stores the text, not the fields). Test: a later node's template
   that reads `nodes.<id>.output.<field>` renders the same value in the
   replay.
5. Unscrubbed text. Test: a run whose output holds a configured secret
   value and a credential-shaped token: the YAML holds neither, `redacted`
   is at least 1.
6. YAML assembled by `format!`. Test: an output holding `"\nexpect:\n  run:
   ...` round-trips as a string value and the parsed scenario's `expect`
   is the captured one.
7. Visits counted from attempts. Test: an agent task with two attempts in
   one execution has `visits: 1` and one `VisitAnswer` with two attempts.
8. A save that overwrites. Test: a second save of the same name is
   `scenario_exists`.
9. A timed-out attempt refused or scripted as `failed`. Test: a run whose
   agent attempt timed out (a stand-in agent that sleeps past a 1 s node
   timeout) is captured with `{ timed_out: true }`, and its replay ends with
   the node `timed_out`.
10. A lossy journal read. Test: a run journal with an unknown event type
   (before its last checkpoint) is refused `unknown_events`; one with a
   torn last line is refused `journal_unreadable`.
11. An aborted run or a cache hit captured. Test: a stopped run is
   `aborted`; a run whose node was served from the cache is `cache_hit`; a
   fatal check rejection is `fatal_rejection`.
12. A save that checks existence then renames. Test: two threads save the
   same name at once; exactly one succeeds, the other is `scenario_exists`,
   and the file holds the winner's bytes.
13. MCP that saves. Test: the tool layer with `SaveChannel::Agent` and
   `save: true` is `save_by_person`, and no file exists.

## D2. The corrections ledger

### One signed store for both ledgers

New file `crates/apb-core/src/signed_store.rs`, declared in
`crates/apb-core/src/lib.rs`. It is the read, MAC, lock and write half of
`crates/apb-core/src/known_problems/store.rs` lines 334 to 447, made
generic. Move that code; do not copy it.

```rust
pub trait SignedDoc: Serialize + DeserializeOwned + Clone {
    /// The purpose the MAC is bound to (`apb-known-problems-v1`).
    const MAC_DOMAIN: &'static str;
    /// The schema this build reads and writes.
    const SCHEMA: u32;
    /// The directory name under `.apb/` (`problems`, `corrections`).
    const DIR: &'static str;
    /// The key of the array whose length an unsigned file reports.
    const ENTRIES_KEY: &'static str;
    fn fresh(scope: Scope, playbook: &str) -> Self;
    fn identity(&self) -> (&str, &str);   // (scope, playbook)
    fn mac(&self) -> Option<&str>;
    fn set_mac(&mut self, mac: Option<String>);
    fn set_schema(&mut self, schema: u32);
}

pub struct Loaded<T> { pub file: T, pub signed: bool, pub unsigned_entries: usize }

#[derive(Debug, thiserror::Error)]
pub enum StoreError { InvalidId(String), NewerSchema(u32), Io(io::Error) }

pub fn dir(root: &Path, dir: &str, scope: Scope) -> PathBuf;
pub fn read<T: SignedDoc>(root: &Path, scope: Scope, playbook: &str) -> Result<Loaded<T>, StoreError>;
pub fn update<T: SignedDoc, R, E: From<StoreError>>(
    root: &Path, scope: Scope, playbook: &str,
    change: impl FnOnce(&mut T) -> Result<(R, bool), E>,
) -> Result<R, E>;
```

Semantics are exactly today's (`store.rs` module docs, lines 1 to 15): the
MAC covers the canonical JSON without `mac` (`bytes_of`, `canonical`,
lines 396 and 401), an unsigned or misplaced file reads as a fresh empty
one and is replaced on the next write, a newer schema refuses a write,
the lock is `lock_dir(<dir>, "<id>.json.lock")`, the write is
`atomic_write_private`.

`known_problems/store.rs` keeps its public API (`read`, `update`, `dir`,
`ProblemFile`, `Loaded`, `ProblemsError`, `PersonDecision`) as thin
wrappers over `signed_store`, with `impl SignedDoc for ProblemFile`
(`MAC_DOMAIN = "apb-known-problems-v1"`, `DIR = "problems"`,
`ENTRIES_KEY = "problems"`) and `impl From<StoreError> for ProblemsError`.
Every existing known-problems test must pass unchanged, and a file written
before this change must still verify (the bytes covered do not change).

### Types

New module `crates/apb-core/src/corrections/` (`mod.rs` the types and the
fold, `normalize.rs` the key, `store.rs` the `SignedDoc` impl), declared in
`lib.rs`.

```rust
pub const MIN_RUNS: u32 = 2;          // proposed at this many runs (fixed, not configurable)
pub const REOPEN_AFTER_RUNS: u32 = 3; // runs on the fix version before it is judged
pub const TEXT_MAX: usize = 300;
pub const LINE_MAX: usize = 400;      // a draft prompt line
pub const EVIDENCE_KEPT: usize = 10;
pub const OBSERVED_KEPT: usize = 1000;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")] #[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum CorrectionKind { ReviewNote, NodeNote, HumanAnswer, Rejection }

#[serde(rename_all = "snake_case")]
pub enum CorrectionStatus { Open, Proposed, Fixed, Reopened, Dismissed }

pub struct Sighting {          // one correction of one run, built by the engine
    pub kind: CorrectionKind,
    pub node: Option<String>,  // the node a draft fix would change
    pub gate: Option<String>,  // the review gate, for a review note
    pub key: String,           // `f:<fingerprint>` or `t:<16 hex>`
    pub text: Option<String>,  // stored text; None only for an `f:` key (see "What a run contributes")
    pub run_id: String, pub seq: u64, pub at_ms: u64, pub version: String,
    /// The supervisor channel does not tell an agent from a person.
    pub via_supervisor: bool,
}

pub struct Correction {
    pub id: String,                         // `c-` + first 12 hex of sha256(key)
    pub kind: CorrectionKind, pub node: Option<String>, pub gate: Option<String>,
    pub key: String,
    pub text: Option<String>,               // the leading wording
    pub texts: Vec<ReportedText>,           // known_problems::ReportedText
    pub runs: u32,
    pub runs_by_version: BTreeMap<String, u32>,  // runs of this entry per version
    pub first_seen_ms: u64, pub last_seen_ms: u64,
    pub evidence: Vec<EvidenceRef>,         // { run_id, seq }, the latest EVIDENCE_KEPT
    pub via_supervisor: bool,               // any sighting came through the supervisor channel
    pub status: CorrectionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub fix: Option<FixRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub decided: Option<Decided>,
}

/// A person's statement that a version carries the fix, with the base
/// counts taken at that moment, so the reopen rule never needs history the
/// file no longer holds.
pub struct FixRecord {
    pub version: String,      // the version the person names as the fix
    pub base_version: String, // the version of the entry's latest sighting then
    pub base_runs: u32,       // file.runs_by_version[base] then
    pub base_hits: u32,       // entry.runs_by_version[base] then
    pub marked: Decided,
}

pub struct ObservedRun { pub run_id: String, pub version: String, pub started_ms: u64 }

pub struct CorrectionFile {
    pub schema: u32, pub scope: String, pub playbook: String,
    pub observed: Vec<ObservedRun>,                 // the latest OBSERVED_KEPT, by started_ms
    /// The `started_ms` of the newest run dropped from `observed`: a run
    /// that started at or before it is never counted again.
    #[serde(default)] pub observed_floor_ms: u64,
    pub runs_by_version: BTreeMap<String, u32>,     // every counted run per version
    pub entries: Vec<Correction>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub mac: Option<String>,
}
```

`impl SignedDoc for CorrectionFile`: `MAC_DOMAIN = "apb-corrections-v1"`,
`SCHEMA = 1`, `DIR = "corrections"`, `ENTRIES_KEY = "entries"`. Reuse
`known_problems::{ReportedText, leading, TEXTS_KEPT, Decided}`; extract the
wording-keeping half of `KnownProblem::add_wording`
(`known_problems/store.rs` line 261) into
`pub fn keep_wording(texts: &mut Vec<ReportedText>, text: &str, at_ms: u64)`
in `known_problems/mod.rs`, called from both. Add `"corrections/"` to
`LOCAL_ENTRIES` in `crates/apb-core/src/workspace.rs` (line 27) and to its
test list (line 240).

### The key

`crates/apb-core/src/corrections/normalize.rs`:

```rust
pub fn tokens(text: &str) -> Vec<String>
pub fn key_of(kind: CorrectionKind, node: Option<&str>, text: &str) -> String
pub fn fingerprint_key(fingerprint: &str) -> Option<String>
```

1. `tokens`: lowercase (`str::to_lowercase`), split on every char that is
   not `char::is_alphanumeric` (Unicode, so non-English text works), drop
   tokens that hold an ASCII digit (ids, counts, line numbers, hashes) or
   are a single char, keep the first 24 distinct in text order, then sort.
2. `key_of`: `t:` + the first 16 hex of
   `sha256("<kind>|<node or empty>|<tokens joined by one space>")`. With
   fewer than 2 tokens the joined text is the whole scrubbed text trimmed,
   so a one-word correction still groups only with itself. It is only ever
   called on text that `stored_text` returned (already scrubbed and not
   flagged).
3. `fingerprint_key`: `f:<fp>` when the fingerprint is 8 to 128 chars of
   `[0-9a-f]` (a validated engine hash), else `None`.

`"Please add the parser tests"` and `"add the parser tests, please"` get
one key, while `"Please add tests for the parser"` is another (it holds
`for`); `"fix line 12"` and `"fix line 40"` get one key.

### What a run contributes

New module `crates/apb-engine/src/corrections/` (`mod.rs` the entry
points, `extract.rs` the sightings). `extract::sightings(run_id, version,
events, scrubber) -> Vec<Sighting>`, one pass over the journal:

1. `review_decided` with `decided_by: None` (`event.rs` line 674), a
   `decision` other than `approve`, and a non-empty `note`: kind
   `review_note`, `gate` the node. `node` is the node that delivered into
   this gate visit: the `from` of the `edge_traversed` events with `to ==
   gate` between the gate's previous `review_decided` (or the run start)
   and this visit's `review_requested`. Exactly one distinct `from` that is
   an `agent_task` or a `finish` with a prompt gives `node`; none, several
   (a join of parallel branches) or another kind gives `None` (no draft).
   A `plan_digest` (a publish consent) is skipped.
2. `supervisor_action` with `action == supervisor_action::CONTEXT_APPEND`
   and `node: Some(n)`: kind `node_note`, `node` n, `via_supervisor: true`.
   The supervisor channel records no person (`apb note` and an agent
   supervisor post the same action), so a `node_note` entry is shown to
   people but never offers a draft (see "Draft").
3. `question_answered` with `answered_by == "human"`: kind
   `human_answer`, `node` the node, text `"<question> -> <answer>"` from
   the matching `question_asked` (count-based, as the event docs say).
4. `attempt_finished` with `rejected_for: Some(r)` or
   `rejection_fingerprint: Some(_)`: kind `rejection`, `node` the node,
   text `r`.

Text and key, per sighting:

1. `scrubber.stored_text(text, TEXT_MAX)` (`scrub.rs` line 395).
2. Some text: the key is `fingerprint_key` for a rejection that carries a
   valid fingerprint, else `key_of` over the stored text.
3. `None` (the text held a secret or a credential shape): the sighting is
   kept, with `text: None`, only when `fingerprint_key` gives a key; else it
   is dropped whole. Nothing derived from the raw text (no hash, no token,
   no length) is ever stored.

`version` is `run_started.version`. Retry advice (`decision_made` with
`retry_advice`) is not a correction.

### Folding into the file

`CorrectionFile::observe(&mut self, run: ObservedRun, sightings, at_ms) -> bool`
in `corrections/mod.rs`:

1. A run whose id is in `observed`, or whose `started_ms <=
   observed_floor_ms`, changes nothing (returns `false`). The floor makes
   the dedup exact although `observed` is capped: a dropped run is older
   than every kept one and can never count again.
2. The run is added to `observed` (sorted by `started_ms`; when over
   `OBSERVED_KEPT`, the oldest is dropped and `observed_floor_ms` becomes
   its `started_ms`) and `runs_by_version[version] += 1`, even with no
   sighting: that is the denominator of the reopen rule.
3. Sightings are grouped by key; each key counts once per run: `runs += 1`,
   `runs_by_version[version] += 1`, `last_seen_ms`, `evidence` (latest
   `EVIDENCE_KEPT`), `keep_wording` per distinct text. A new key creates an
   entry with status `open`.
4. Status after the fold, per entry (`settle_status`):
   - `dismissed` stays `dismissed`;
   - `open` with `runs >= MIN_RUNS` becomes `proposed`;
   - `fixed`: see "After the fix".
5. Entries ordered by `runs` (most first), then `last_seen_ms`, then id.

Writing: `crate::corrections::observe(root, run_dir)` reads the journal
with the strict `crate::event::read_all` (a journal this apb reads only in
part, an unknown event after the last checkpoint or a broken line, is not
counted: nothing is stored for that run), returns at once for a run whose
origin is not production, builds the sightings with the run redactor's
scrubber (as `known_problems::observe` does, `known_problems/mod.rs` line
74; no scrubber: store nothing), and calls `signed_store::update`. When the
run has no sighting and the file does not exist, nothing is written (no
file per playbook that never had a correction). `started_ms` is the ts of
`run_started`. Use the same playbook and scope as known problems: make
`known_problems::started` (line 54) `pub(crate)` and call it.

### The one run-end hook

New file `crates/apb-engine/src/run_end.rs`:

```rust
/// Records what an ending run leaves for later runs and people: its known
/// problems and its corrections. Best effort: a fault is reported on stderr
/// and changes nothing about the run.
pub(crate) fn record(run_dir: &Path)
```

It calls `known_problems::observe_ended(run_dir)` and then the corrections
observe (stderr on error, as `observe_ended` does). Replace every call of
`known_problems::observe_ended` with it: `scheduler/finish_hooks.rs`
line 25, `scheduler.rs` lines 1003 and 3451, `stop.rs` line 199. The
scenario guard in `finish_hooks` (line 20) stays in front of it.

### Draft

A `proposed` or `reopened` entry offers a draft for a person to edit and
apply themselves:

```rust
pub struct Draft { pub node: String, pub line: String }
pub fn draft(entry: &Correction, playbook: &Playbook) -> Option<Draft>
```

1. No draft when `node` is `None`, the node is not in the playbook, it has
   no prompt (an `agent_task` or a `finish` with a prompt), the entry is a
   `node_note` (`via_supervisor`), or `text` is `None`.
2. `line`, one line, whitespace collapsed, hidden chars replaced
   (`apb_core::text::replace_hidden`), at most `LINE_MAX` chars:
   - `review_note`: `A person corrected this step in earlier runs: <text>`
   - `human_answer`: `When this step asks a question, people answered in
     earlier runs: <text>`
   - `rejection`: `Earlier attempts of this step were rejected by apb:
     <text>. Make sure the result passes that check before you report
     success.`

apb never applies a draft. The view shows it with the instruction to add
it to the node's prompt through the existing ways of changing a playbook
(the dashboard editor, `apb` version tools, or a supervisor's forward patch
that a person then promotes with `apb candidate`), and then to mark the
version that carries it (below). No candidate is created by the ledger.

### After the fix

A person marks the fix: `mark_fixed(entry, version)`:

1. The entry must be `proposed` or `reopened` (else `not_proposed`), and
   `version` must exist in the playbook's store (else `unknown_version`).
2. `FixRecord { version, base_version: <version of the entry's latest
   sighting>, base_runs: file.runs_by_version[base], base_hits:
   entry.runs_by_version[base], marked }`; status `fixed`.

`settle_status` for a `fixed` entry, with `n_fix =
file.runs_by_version[fix.version]` and `h_fix =
entry.runs_by_version[fix.version]`: once `n_fix >= REOPEN_AFTER_RUNS`,
the entry is `reopened` when `h_fix * base_runs >= base_hits * n_fix` (the
rate per run did not fall; integer cross-multiplication, no float). Below
`REOPEN_AFTER_RUNS` it stays `fixed`. `base_runs` is at least 1 (the base
holds a sighting), so no fallback over earlier history is needed. Only the
fix version itself is counted (a later version is a new definition). The
view shows both rates.

### A person's decisions

`dismiss` (any status but `dismissed`), `reopen` (a `dismissed` or `fixed`
entry back to `proposed` when `runs >= MIN_RUNS`, else `open`, clearing
`fix`) and `mark_fixed` (above). Each keeps `Decided { by, at_ms, reason }`
(reason cleaned like `clean_reason` in `tools/problems.rs`, at most 300
chars).

### Who may read and write

The ledger holds people's words and engine fingerprints and is shown only
to people:

1. No prompt section, no supervisor line, no `run_status` block, no
   template variable reads it.
2. Every entry point, reads included, is refused from a run step
   (`outside_run`, message `reading or deciding corrections`), on every
   surface.
3. MCP lists, dismisses and reopens; `mark_fixed` is a person's statement
   about a version and is offered on the CLI and the dashboard only.

### Surfaces

Tool layer `crates/apb-mcp/src/tools/corrections.rs` (shared by MCP, CLI
and HTTP), with `Target` reused from `tools/problems.rs` (line 56):

```rust
pub fn corrections_view(root, t: &Target, step: &StepCaller) -> Result<CorrectionsView, ToolError>
pub fn correction_decide(root, t: &Target, entry: &str, decision: Decision, by: &str, reason: Option<&str>, step: &StepCaller) -> Result<CorrectionView, ToolError>
```

`Decision` is `Dismiss`, `Reopen` or `MarkFixed { version }`; the MCP
server never constructs `MarkFixed`. `CorrectionsView { playbook, scope,
signed, unsigned_entries, entries: Vec<CorrectionView> }`, `CorrectionView
{ correction, draft: Option<Draft>, rate: Option<FixRate> }` with
`FixRate { base_runs, base_hits, fix_runs, fix_hits }`.

1. CLI `crates/apb-cli/src/corrections.rs`, `Corrections(CorrectionsArgs)`
   in `main.rs` next to `Problems` (line 575):
   `apb corrections <playbook> [--scope project|global] [--json]`,
   `apb corrections <playbook> show <entry>` (the draft line printed as
   data, with the node), `fixed <entry> --version X.Y.Z [--reason]`,
   `dismiss <entry> [--reason]`, `reopen <entry> [--reason]`.
   Text line per entry: `c-1a2b3c4d5e6f proposed review_note at node
   implement (gate review): 3 runs, last <run>: "<text>"`.
2. MCP in a new `crates/apb-mcp/src/server/corrections.rs`:
   `playbook_corrections { id, scope?, entry? }` (classified
   `ReadOnlyUse::Full` in `READ_ONLY`, `read_only.rs` line 28) and
   `correction_decide { id, scope?, entry, decision: dismiss|reopen,
   reason? }` (`REMOVED_IN_READ_ONLY`). Descriptions: decide only on the
   person's explicit word; never put a draft into a playbook on your own.
3. HTTP `crates/apb-server/src/routes/corrections.rs`:
   `GET /api/playbooks/{id}/corrections`,
   `POST /api/playbooks/{id}/corrections/{entry}/{action}` with `action`
   `fixed` (`{ version, reason? }`), `dismiss` or `reopen` (`{ reason? }`);
   409 for a refusal and from a run step, 422 for an unknown action or
   field. Same query (`workspace`, `scope`) as `routes/problems.rs`.
4. Dashboard: `web/src/lib/CorrectionsCard.svelte` on the playbook page
   next to `KnownProblemsCard.svelte`: entries by status, the leading text,
   runs, `via the supervisor channel` when set, the draft line with a copy
   button and the node it is for, "Mark fixed in version" (a version
   picker, confirm box), Dismiss, Reopen, and for `fixed`/`reopened` both
   rates. With `corrections.test.ts`. Plain CSS only.

TypeScript: register `CorrectionKind`, `CorrectionStatus`, `Correction`,
`FixRecord`, `ReportedText` (if not yet), `Draft`, `FixRate`,
`CorrectionView` and `CorrectionsView` in the manual type list of
`crates/apb-server/src/ts_contract.rs` (from line 44), then regenerate.

Docs: new `docs/CORRECTIONS.md` (what is collected, the key, statuses, the
draft and how a person applies it, marking a fix, the reopen rule, who may
read), a line in `docs/KNOWN-PROBLEMS.md` pointing to it, `docs/MCP.md` for
the two tools.

Wrong implementations to avoid:

1. A second copy of the MAC and lock code. Test: a known-problems file
   written by the old code (fixture bytes) still verifies, and both stores
   are served by `signed_store` (code-ranker DRY plus one unit test that a
   `CorrectionFile` signed under the problems domain reads unsigned).
2. Counting a run twice. Test: observing the same run twice leaves `runs`
   and `runs_by_version` as after once; after 1001 further runs, observing
   the first one again (now below the floor) changes nothing.
3. Denominator only from runs with corrections. Test: base snapshot 2 of 2,
   three runs on the fix version with the correction in one: stays
   `fixed`; in three: `reopened`.
4. Grouping on raw text. Test: two notes differing in a secret value are
   one entry, and the stored text holds no secret.
5. A flagged text kept or hashed. Test: a review note that is only a
   credential-shaped token adds no entry, and the file's bytes hold neither
   the token nor `sha256` of it nor of its tokens.
6. The draft shown to an agent. Test: `playbook_corrections` from a run
   step (`APB_RUN_ID` set) is refused, and a run of the playbook after a
   correction has no ledger text in its rendered prompt.
7. A wrong delivering node. Test: a gate fed by a `join: all` of two agent
   tasks gives `node: None` and no draft; a gate fed by one agent task
   names it even when another branch finished later.
8. A supervisor note worded as a person's. Test: a `context_append` entry
   offers no draft.
9. The ledger applying its draft. Test: after `fixed`, the playbook store
   holds no new version and no candidate pointer.
10. `retry_advice` decisions counted. Test: a run with an applied retry
    advice and no person adds no entry.
11. `approve` notes counted. Test: `approve` with note "ok" adds nothing.
12. A lossy read counted. Test: a journal with an unknown event after its
    last checkpoint adds nothing and writes no file.

## D3. The follow-up check

### Schema

`crates/apb-core/src/follow_up.rs` (new, declared in `lib.rs`):

```rust
pub const FOLLOW_UP_SITE: &str = "@follow_up";
pub const MIN_AFTER_SECONDS: u64 = 60;
pub const MAX_AFTER_SECONDS: u64 = 30 * 24 * 3600;
pub const DEFAULT_GRACE_SECONDS: u64 = 24 * 3600;
pub const MAX_GRACE_SECONDS: u64 = 7 * 24 * 3600;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FollowUp {
    /// How long after the run's successful end the check is read.
    pub after: String,
    /// How late a read may still be taken; later it is `missed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace: Option<String>,
    /// One check in the baseline check's shape (`crate::baseline::BaselineCheck`).
    pub check: crate::baseline::BaselineCheck,
}

impl FollowUp {
    pub fn after_seconds(&self) -> Option<u64>   // duration::parse_duration_str
    pub fn grace_seconds(&self) -> Option<u64>   // default DEFAULT_GRACE_SECONDS
}
```

`Playbook` (`schema.rs` line 138) gets, after `known_problems` (line 197):

```rust
/// One fact read once `after` a successful run ended (see
/// [`crate::follow_up`]). Additive to schema 2; no migration.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub follow_up: Option<crate::follow_up::FollowUp>,
```

YAML:

```yaml
follow_up:
  after: 3d
  check:
    id: deal_won
    description: the deal moved to won
    connector: { name: crm, function: get_deal, args: { id: "{{params.deal_id}}" } }
    observe:
      - { id: stage, type: field, path: deal.stage, expect: { equals: won } }
```

One follow-up per playbook (a single block), so one per run. Durations use
`apb_core::duration::parse_duration_str` (`duration.rs` line 14), the
grammar of `expected_duration`.

### Rule check without a before value

`crates/apb-core/src/baseline/rule.rs`, next to `compare` (line 123):

```rust
/// One observation checked under `rule` with no before value (a
/// follow-up): `equals`, `at_most`, `at_least`, `empty` and `contains` only;
/// a `change` rule is unknown. `regressed` is always `None`; `detail` is
/// the observed value as people read it.
pub fn check(observation: &Observation, rule: &Rule) -> Comparison
```

Implement it through `compare(Some(observation), observation, rule)` for
the non-`change` rules, then set `regressed: None` and `detail:
Some(value.to_string())`; `Observation::Unknown` gives `Verdict::Unknown`
with the reason. Unit tests: each rule met and not met on number, list and
field, `change` unknown, unknown observation unknown.

### Validator V183 to V186

New `crates/apb-core/src/validate/follow_up.rs`, declared in
`validate/mod.rs`, called after the baseline line (line 363) with a
`// V183-V186` comment. First make the baseline rule helpers reusable:
`check_structure`, `check_source`, `check_observations`, `check_templates`
in `validate/baseline.rs` (lines 53, 146, 187, 255) take a `label: &str`
("baseline check", "follow-up check") used in place of the literal in
their `at` string, and become `pub(super)`. The follow-up check runs all
four on `follow_up.check` (their codes V62 to V65 stay, with the follow-up
label), then:

| Code | Severity | Rule |
|---|---|---|
| V183 | error | `follow_up.after` is not a duration, or is under `MIN_AFTER_SECONDS` or over `MAX_AFTER_SECONDS` |
| V184 | error | `follow_up.grace` is not a duration, or is under 60 s or over `MAX_GRACE_SECONDS` |
| V185 | error | the check's source is `project_checks` (a follow-up reads a connector or a script) |
| V186 | error | an observation's rule is a `change` rule (a follow-up has no before value) |

V187 to V190 are reserved for this change and stay unused. Verify the
codes are free first: `git grep -ohE '"V1[0-9][0-9]"' crates | sort -u |
tail -3`. Add `FOLLOW_UP_SITE` to the engine sites: in
`crates/apb-core/src/connector/sites.rs`, `connector_sites` (line 40)
chains `(FOLLOW_UP_SITE, [binding])` when the check reads a connector
(`baseline_binding`, line 129), `site_label` (line 109) names it `the
follow-up check`, and the module docs list it with `BASELINE_SITE` and
`NOTIFY_SITE`. Through that one walk the gate, the manifest grants,
preflight, effects and V23 to V26 see it with no other change; V68 already
refuses a node id starting with `@`.

### The event

`crates/apb-engine/src/event.rs`, a new variant next to `BaselineCompared`
(line 1741):

```rust
/// The playbook's `follow_up` read once after a successful run ended.
/// Appended only after a `run_finished { outcome: succeeded }`, at most
/// once per run. A checkpoint (see `is_checkpoint`). Additive variant.
FollowUpObserved {
    #[serde(default)] check: String,
    /// `observed`, `unknown` (the source could not be read, `reason` says
    /// why) or `missed` (read later than `grace`; nothing was read).
    #[serde(default)] status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] reason: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    observations: BTreeMap<String, apb_core::baseline::Observation>,
    /// One per observation, in `observe` order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    results: Vec<FollowUpResult>,
    /// `met` (every result met), `not_met` (any not met) or `unknown`.
    #[serde(default)] verdict: String,
    /// The seq of the `run_finished` it follows.
    #[serde(default)] finished_seq: u64,
    #[serde(default)] due_at_ms: u64,
    #[serde(default)] observed_at_ms: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")] input_digest: String,
},
```

`FollowUpResult { observation, rule, verdict, detail: Option<String> }`
(serde default on every field, `rule` as `Rule::describe`, verdict as
`Verdict::as_str`). Unknown never counts as met: `verdict` is `met` only
when every result is `met`, and `missed` and `unknown` statuses give
`unknown`.

`is_checkpoint` (line 3333) adds `EventPayload::FollowUpObserved { .. }`.
Every exhaustive `match` on `EventPayload` the compiler lists gets an arm
(`state.rs` fold: no state change). Add one line to
`crates/apb-engine/tests/fixtures/journal/known_events.jsonl` only if that
fixture is regenerated from the serializer (follow the existing test).

### Interplay with `read_all` and `read_all_for_stop`

1. This apb: the event is known and a checkpoint, so `read_all` (line
   3433), `read_all_for_stop` (line 3450) and `EventLog::open` read the
   journal as before. `check_unknown_settled` (line 3487) settles an
   unknown event only when a checkpoint comes after it: an event type a
   later apb wrote between `run_finished` and `follow_up_observed` is
   settled by it; one written after `follow_up_observed` is not, and
   `read_all` refuses it, as for every checkpoint.
2. An apb older than this change: the event is unknown and no checkpoint
   follows it. Its read-only surfaces (`read_journal`) skip and count it;
   its stop path allows it; its `read_all` and `EventLog::open` refuse with
   the "written by a newer apb" message, so a resume `--from-node`, a
   continue-from and a notify sweep of that run refuse there. `apb resume
   --all` of that older apb skips the run (`resume_all.rs` line 197). This
   is the accepted cost; the release notes say it.
3. An older apb that starts or drives a run of a playbook with
   `follow_up` drops the block on load (`Playbook` takes unknown top-level
   fields, `schema.rs` line 138) and arms nothing. The run's snapshot still
   holds the block, so this apb's `apb resume --all` finds the follow-up
   pending and reads it (within `grace`) or journals it `missed`. No
   `GuardedFeature` is added: the follow-up acts only after the run ended,
   so refusing an older apb mid-run would protect nothing. The docs say
   that a follow-up is guaranteed only when this apb (or a later one) runs
   `apb resume --all` or drives the run's end.
4. A resume of a finished run (`--from-node`) in this apb: the new drive
   appends after the event; the fold ignores it; a later `run_finished`
   does not re-arm a follow-up once one `follow_up_observed` exists.

### When it is due, and who reads it

New module `crates/apb-engine/src/follow_up/` (`mod.rs` the state fold
and entry points, `read.rs` the source read, `waker.rs` the waker body).

```rust
pub enum FollowUpState {
    /// No `follow_up` in the run's playbook snapshot, or the run's latest
    /// end is not a success, or the run is not production.
    None,
    /// Due at `due_at_ms`, for the `run_finished` at `finished_seq`.
    Pending { finished_seq: u64, due_at_ms: u64, grace_ms: u64 },
    Observed(ObservedFollowUp),
}
pub fn state(playbook: Option<&Playbook>, events: &[Event], production: bool) -> FollowUpState
```

`Pending` holds when: the playbook snapshot
(`legacy_snapshot::load_run_playbook`) has `follow_up`, the run is
production (manifest origin), the fold's status is `Succeeded`, the latest
event that is `run_finished` is `succeeded` and no `run_resumed` follows it,
and no `follow_up_observed` exists. `due_at_ms = run_finished.ts +
after_seconds * 1000` (wall clock: a finished run has no paused interval).
For tests only, `APB_FOLLOW_UP_AFTER_MS` replaces `after` through
`crate::env_duration::test_override` (a release build ignores it), exactly
as `APB_NOTIFY_RETRY_GAP_MS` does (`notify/transitions.rs` line 23).

The one reader:

```rust
/// Reads the follow-up of run `run_id` now, if it is due and still
/// pending, under the lease `follow-up-<finished_seq>`, and journals
/// `follow_up_observed`. Returns what it journaled, or `None`.
pub fn observe_due(root: &Path, run_id: &str, now_ms: u64) -> Result<Option<Event>, EngineError>
```

0. Trust first: `crate::gate::check_resume(root, run_id, true)`
   (`gate/mod.rs` line 165), the check every engine resume makes: a run
   directory this machine did not create (`run_origin::verify`, one that
   came with a repository) is refused. A missing manifest reads as
   production (`manifest.rs` line 434), so the origin stamp is what keeps
   an imported run's script from running. A refusal journals nothing,
   reads nothing, and returns `None` (`apb resume --all` prints one stderr
   line). `arm` makes the same check before it spawns a waker.
1. Takes `crate::waker::Lease::try_take(run_dir, WakerFor::FollowUp(seq))`
   (`waker.rs` line 115); a held lease returns `None` (another process
   reads it).
2. Re-reads the journal under the lease; returns `None` when the state is
   not `Pending` for the same `finished_seq`, when a driver is alive
   (`crate::liveness::driver_alive`), or when `now_ms < due_at_ms`.
3. `now_ms > due_at_ms + grace_ms`: journals `status: missed`, `reason:
   "read <late> after its time, past its grace of <grace>; the machine
   was off or apb did not run"`, `verdict: unknown`; the source is not read.
4. Else reads the source once (below) and checks each observation with
   `rule::check` in memory, on the raw values.
4a. Before the append, everything stored is scrubbed with the run
   redactor's scrubber (`RunRedactor::for_run(root, run_dir).scrubber()`):
   each observation through `Scrubber::redact_value` of its JSON form
   (list items and text values), each result `detail` and the `reason`
   through `Scrubber::scrub`. When the scrubber is `Err`
   (secrets unresolved), the event is journaled with `status: unknown`,
   `verdict: unknown`, no observations, no details, and `reason: "the
   run's secret values cannot be read, so the observation was not
   stored"`. Then the event is appended through `EventLog::open`.
5. Then the notify sweep of a finished run:
   `notify::send::Notifier::new(root, run_dir, run_id).sweep_now()` and,
   when `notify::retry::retry_ahead`, `waker::spawn_notify_waker` (the same
   two lines as `scheduler.rs` lines 460 to 464).

The source read reuses `crate::baseline::read::read` (`baseline/read.rs`
line 52) with a `ReadCtx` built from the run:

- `ReadCtx` (`baseline/ctx.rs` line 14) gets `pub site: &'a str`; the
  connector read uses it instead of `apb_core::baseline::BASELINE_SITE`
  (`read.rs` line 217). The drive passes `BASELINE_SITE`
  (`scheduler/baseline.rs` line 61) and so does the list check
  (`scheduler/list/check.rs` line 146): no behaviour change there.
- `run_dir`, `root`; `workdir`: the run's `worktree_resolved` path when it
  is still a directory, else the execution root; a script source whose
  run worktree is gone is `unknown` (`the run's working tree is gone`).
- `params` and `spawn_env` from `read_run_config(run_dir)`;
  `scrub: apb_core::connector::resolve::all_referenced_env_names(root)`
  (as `scheduler.rs` line 713); `cancel` a fresh `AtomicBool`;
  `phase: "follow_up"` (a script gets `APB_BASELINE_PHASE=follow_up`);
  `events` the journal; `guard_tree: true`; `scripts_label: "follow-up
  scripts"`.
- A scenario run never reaches here (it is not production); the
  `scripted` branch of `read` is never taken.

The connector call resolves the `@follow_up` grant the manifest recorded at
start and the live trust of the connector now, as every call does: a
connector revoked or changed since the run is refused, and the snapshot is
`unknown` with that reason.

### Arming

1. The drive: in `drive` (`scheduler.rs`, the `Ok(r)` arm at line 476),
   after `notify_after()`, when `r.outcome == RunStatus::Succeeded`, call
   `crate::follow_up::arm(root, run_dir, &r.run_id)`: when the state is
   `Pending`, spawn `apb __pause-waker --root <root> --run-id <id>
   --follow-up <finished_seq>` through `crate::waker::spawn` (line 143)
   with `WakerFor::FollowUp(seq)`.
2. `WakerFor` (`waker.rs` line 24) gets `FollowUp(u64)`: lease name
   `follow-up-<seq>`, args `--follow-up <seq>`, `what` "a follow-up check
   waits for its time", `instead` "`apb resume --all` picks it up".
3. The waker body `follow_up::waker::run_follow_up_waker(root, run_id,
   seq)`: takes the lease `follow-up-<seq>` (a held one ends it at once, as
   for every waker), acknowledges the hold, polls the journal every
   `pause::waker::POLL_ENV` interval (the same env and default as the
   pause waker) while the state is `Pending` for `seq` and `now <
   due_at_ms`, then drops its lease and calls `observe_due`, which takes
   the lease again and re-checks the state under it. Holding the lease
   into `observe_due` would make that call find it held and journal
   nothing. It ends at once when the state is no longer `Pending` for
   `seq`.
4. CLI: `PauseWaker` in `crates/apb-cli/src/main.rs` (line 877) gets
   `#[arg(long = "follow-up", conflicts_with_all = [...])] follow_up:
   Option<u64>`, added to `required_unless_present_any` of `seq`;
   `run.rs` `WakerFor` (line 1437) gets `FollowUp(u64)` and
   `pause_waker_cmd` (line 1452) maps it to the waker body (an observation
   ends like a waker with nothing to resume: `WakeOutcome::Superseded`).
5. `apb resume --all` (`pause/resume_all.rs` line 204): for a terminal run,
   after `notify::retry::sweep_finished`, call
   `crate::follow_up::sweep_finished(root, &run_dir, &run_id, &events)`: a
   due follow-up is read now through `observe_due` (or journaled `missed`
   past its grace); one still ahead gets a waker through `waker::spawn`
   unless `Lease::held`. Nothing is listed for it (as for the notify
   sweep). Update the module docs list.

### Surfaces

1. Run outcome: `crates/apb-engine/src/run_outcome/follow_up.rs` (new,
   declared in `run_outcome.rs` next to `pub mod baseline;` line 21):

   ```rust
   #[cfg_attr(feature = "ts", derive(ts_rs::TS))]
   pub struct RunFollowUp {
       pub check: String, pub description: String,
       pub source: String,          // connector | script
       /// `pending`, `observed`, `unknown`, `missed`, or `not_scheduled`
       /// (the run did not succeed).
       pub status: String,
       #[serde(skip_serializing_if = "Option::is_none")] #[cfg_attr(feature = "ts", ts(optional))]
       pub due_at_ms: Option<u64>,
       #[serde(skip_serializing_if = "Option::is_none")] #[cfg_attr(feature = "ts", ts(optional))]
       pub observed_at_ms: Option<u64>,
       /// `met`, `not_met`, `unknown`; absent while pending.
       #[serde(skip_serializing_if = "Option::is_none")] #[cfg_attr(feature = "ts", ts(optional))]
       pub verdict: Option<String>,
       #[serde(skip_serializing_if = "Option::is_none")] #[cfg_attr(feature = "ts", ts(optional))]
       pub reason: Option<String>,
       pub results: Vec<FollowUpResultView>,
   }
   pub fn follow_up_from(playbook: Option<&Playbook>, events: &[Event], production: bool) -> Option<RunFollowUp>
   ```

   `RunReadout` (`run_outcome.rs` line 152) gets `pub follow_up:
   Option<RunFollowUp>`, filled in `readout_of` (line 136).
   `OutcomeSummary` (`run_outcome/summary.rs` line 174) gets the same
   field (`skip_serializing_if = "Option::is_none"`, TS optional), and
   `sections()` (line 272) adds a `Follow-up` section after `Baseline`
   (line 316): `pending: due <UTC time>` or `<verdict>: <rule> on <obs>
   (<detail>)` per result, and the reason. A `not_met` follow-up adds a
   `NeedsLookReason::FollowUp` to `needs_look` (new variant, TS mirrored),
   so the run reads "needs a look"; `unknown` and `missed` do not.
   The CLI (`print_outcome`, `crates/apb-cli/src/run.rs` line 2017) and the
   HTML report (`report_html/sections.rs` line 221) get it through
   `sections()` with no other change. MCP `run_status`
   (`crates/apb-mcp/src/tools/run.rs` line 423) adds `follow_up` next to
   `baseline`. Redaction: results' `detail` and observations through the
   run redactor exactly as the baseline block is (follow the baseline
   path in the same files).
2. Dashboard: `web/src/lib/RunVerdictPanel.svelte` (line 113, after the
   baseline block): a `Follow-up` block with the status, due or observed
   time and each result; `web/src/lib/journal.ts` a line for
   `follow_up_observed` (`follow-up <check>: <verdict>`). Vitests.
3. `apb stats`: new `crates/apb-engine/src/run_stats/follow_up.rs`
   (`FollowUpStats { check, observed, met, not_met, unknown, missed,
   pending }`, TS-exported), a field `follow_up: Option<FollowUpStats>` on
   the per-version stats next to `baseline` (`run_stats.rs` line 224), the
   text line `follow-up deal_won: met 3/4 (75%), unknown 1, missed 0,
   pending 2` in `run_stats/render.rs`, and `--compare` gets
   `compare.follow_up_met_rate` (base and against, absent when either has
   no observed run). Met rate is over observed runs; unknown and missed are
   counted apart, never as met. Update `docs/STATS.md`.
4. Notifications: `apb_core::notify::Transition` (`notify/mod.rs` line 25)
   gets `FollowUp` (`follow_up`), in `ALL` (line 42) and not in `DEFAULT`
   (line 54): opt-in, like `host_task`. `open_transitions`
   (`notify/transitions.rs` line 73) opens it at a `follow_up_observed`
   seq and closes it on `run_resumed` / `run_started` like `finished`.
   `notice.rs` (line 66) gets an arm: subject `<playbook>: follow-up
   <verdict>`, one line per result. A follow-up notice goes out for every
   verdict (the person opted in). Regenerate `consts.gen.ts`
   (`NOTIFY_TRANSITIONS`) and `api.gen.ts`. Update `docs/NOTIFY.md`.

TypeScript: register `FollowUpResultView`, `RunFollowUp`,
`FollowUpStats` and the new `NeedsLookReason` variant's type (already
listed) in the manual list of `crates/apb-server/src/ts_contract.rs` (from
line 44); `Transition` and `NOTIFY_TRANSITIONS` regenerate from the
existing entries. Then regenerate.

Docs: `docs/HOWTO-authoring.md`, a section "Follow-up check" after the
baseline check section, with the YAML, the timing, `grace`, what `missed`
means, the V183 to V186 table in the format of the V160 to V162 table, and
the older-apb note.

Wrong implementations to avoid:

1. `unknown` or `missed` counted as met anywhere (verdict, stats, needs a
   look). Test: an observation that cannot be read gives `verdict:
   unknown` and `met: 0` in stats.
2. Reading the source late without limit. Test: `observe_due` 2 days past
   due with the default grace journals `missed` and makes no connector call
   (assert with a connector stand-in that counts calls).
3. Two observations of one run (a waker and `resume --all` at once). Test:
   two `observe_due` calls under a held lease journal one event.
4. A follow-up after a failed run. Test: a failed run's state is `None` and
   no waker is spawned.
5. Due time from `run_started` or from the waker's start. Test: due equals
   the `run_finished` ts plus `after`.
6. Not a checkpoint, or a checkpoint read the wrong way round. Test: a
   journal with `run_finished`, an unknown event type, then
   `follow_up_observed` reads with `read_all`; with the unknown event after
   `follow_up_observed` it is refused.
7. `change` rules accepted. Test: V186 for `change: decrease`.
8. The `@follow_up` grant left out of the site walk (the call is then
   refused at follow-up time). Test: an e2e with a connector stand-in reads
   a value and journals `observed`.
9. A waker that keeps sleeping after a resume. Test: a `run_resumed` after
   the arm makes the waker end without journaling.
10. A follow-up for an imported run. Test: a run directory with no
   origin stamp (copied in, as from a repository) and a script source that
   writes a marker file: `observe_due` and `apb resume --all` journal
   nothing and the marker is absent.
11. Raw values journaled. Test: a script source printing a configured
   secret value in an observed field: the bytes of `events.jsonl` do not
   hold it, and the verdict is still computed from the raw value; with
   unresolved secrets the event is `unknown` with no observations.
12. A waker that keeps its lease into `observe_due`. Test (CLI): the
   waker of a succeeded run with a short `APB_FOLLOW_UP_AFTER_MS` journals
   `follow_up_observed` and exits.

## Release notes

The change is not tagged here. List in the final report: scenario capture
on the CLI, HTTP and the dashboard (MCP previews only); the scripted timeout answer
(`timed_out: true`) in scenario files; the corrections ledger and its git-ignored
`.apb/corrections/`; `follow_up:` with its waker, `apb resume --all` reading
due follow-ups, the opt-in `follow_up` notification, the outcome and stats
blocks; an older apb refuses to resume or continue a run with a
`follow_up_observed`.
