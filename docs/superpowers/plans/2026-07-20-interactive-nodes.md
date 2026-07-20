# Interactive Nodes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an `agent_task` node ask the user a question mid-run. A node marked `interactive: true` can raise a question; the run parks until an answer arrives through any facade (supervisor chat, CLI, or web UI); the answer reaches the agent and execution continues. One agent-agnostic Q&A core (journal events plus two file channels, modeled on the human_review machinery), two ask transports selected per agent (`live` blocking MCP tool, or `resume`/`reprompt` marker plus re-invocation), one shared answer path. Ships as 0.8.0. Full spec: `docs/superpowers/specs/2026-07-20-interactive-nodes-design.md`.

**Architecture:** All changes ride existing rails. The append-only event journal gains two variants (`QuestionAsked`, `QuestionAnswered`) plus one field (`AttemptFinished.session`). Two new channel files (`questions.jsonl`, `answers.jsonl`) mirror `reviews.jsonl` exactly: decision makers append, only `drive` writes events. The drive loop gains an interactive-question park-and-poll branch mirroring the `HumanReview` branch (count-based consumption, `AWAIT_CONTROL_POLL`). The adapter gains a stdout marker scan and per-agent transports. A hidden `apb __ask-server` subcommand re-execs the apb binary as a one-tool stdio MCP sidecar the way `apb __drive-run` already does. Phasing follows the spec: Tasks 1-9 are phase 1 (core plus resume/reprompt, works with every agent from day one); Tasks 10-11 are phase 2 (live transport for claude); Tasks 12-13 are the web facade and documentation.

**Tech Stack:** Rust workspace edition 2024, no new dependencies (the sidecar reuses the `rmcp` dependency apb-mcp already has). Web: svelte + vitest (panel plus tolerance tests).

## Global Constraints

- Branch: `feat/interactive-nodes`. One PR. Never push (the controller handles push and release).
- Every commit: `git commit --signoff` (the DCO bot blocks PRs without a `Signed-off-by` line), message ends with the `Co-Authored-By` trailer for the acting model.
- No em-dashes (U+2014), no exclamation marks, no CJK anywhere. English machine-facing text; user-facing chat messages are written in the user's chat language.
- Gates per task before DONE: `cargo fmt --all -- --check` clean; scoped `cargo clippy -p <crate> --all-targets -- -D warnings` clean while iterating, `cargo clippy --workspace --all-targets -- -D warnings` clean at task close; scoped `cargo test -p <crate>` while iterating and one `cargo test --workspace` before any commit that touched `apb-core` or `apb-engine` (per `docs/BUILD-OPTIMIZATION.md` rules 1 and 2). One cargo invocation at a time.
- Testing rules (`docs/TESTING-GUIDELINES.md`): integration tests live only in the crate's single binary (`tests/main.rs` + `tests/suite/<name>.rs` + one `mod` line); unit tests inline in `src`. Tempdirs only, no real network. Stub agents come from the existing `tests/suite/common` helpers, never a hand-rolled stub. Every wait is bounded by a deadline and fails with a message naming what it waited on; no bare `sleep` unless timing IS the subject; build RAII reapers/guards before the first thing that can panic.
- New `EventPayload` fields only with `#[serde(default)]` (CLAUDE.md rule). State files written atomically via `apb_core::fsutil`.

### Exact schema changes (`crates/apb-core/src/schema.rs`)

`NodeKind::AgentTask` gains four fields:

```rust
/// Interactive node (spec 2026-07-20): the agent may ask the user a
/// question mid-run. Only meaningful on `agent_task`. Default false.
#[serde(default)]
interactive: bool,
/// Who may answer an interactive node's questions. `human`: the
/// supervisor must relay the question to the user and relay the answer
/// back verbatim. `supervisor`: the supervisor may answer on its own
/// judgment. Default `human`. Serialized only when non-default.
#[serde(default, skip_serializing_if = "AnswerBy::is_default")]
answer_by: AnswerBy,
/// Wait budget for a pending question. `None` (default) waits forever,
/// like human_review. On expiry the engine answers with `default_answer`
/// (`answered_by: "timeout"`); expiry without a `default_answer` fails
/// the attempt.
#[serde(default, skip_serializing_if = "Option::is_none")]
question_timeout_seconds: Option<u64>,
/// The answer supplied automatically when `question_timeout_seconds`
/// elapses. Requires `question_timeout_seconds` (validator V32).
#[serde(default, skip_serializing_if = "Option::is_none")]
default_answer: Option<String>,
```

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerBy {
    #[default]
    Human,
    Supervisor,
}

impl AnswerBy {
    /// For `skip_serializing_if`: the default (`Human`) is omitted from
    /// serialized YAML/JSON, matching the schema's minimal-output style.
    pub fn is_default(&self) -> bool {
        matches!(self, AnswerBy::Human)
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            AnswerBy::Human => "human",
            AnswerBy::Supervisor => "supervisor",
        }
    }
}
```

All four fields participate in the playbook content digest automatically, so an untrusted or drifted playbook cannot gain the ability to interrogate the user without its digest changing and trust being re-established (spec Security section).

### Exact validation (`crates/apb-core/src/validate.rs`)

V30 is the last code in use. Add `check_interactive(playbook, &mut r)` to the `validate()` call list and two codes, following the existing `error(code, node, msg)` / `Issue` convention:

- V31 (error): the interactive companion fields (`answer_by` non-default, `question_timeout_seconds` set, or `default_answer` set) present on an `agent_task` node that is not marked `interactive: true`. Message: `` interactive companion fields (`answer_by`, `question_timeout_seconds`, `default_answer`) require `interactive: true` ``. Only `agent_task` carries these fields, so this is the node-kind guard the spec calls "present on a node that is not an interactive agent_task".
- V32 (error): `default_answer` set without `question_timeout_seconds`. Message: `` `default_answer` requires `question_timeout_seconds` (it is the answer used when the timeout elapses) ``.

### Exact event changes (`crates/apb-engine/src/event.rs`)

Two new `EventPayload` variants, snake_case tags like the rest of the enum; any field added later carries `#[serde(default)]`:

```rust
/// An interactive node's agent asked the user a question (spec
/// 2026-07-20-interactive-nodes). Written by drive when it observes a new
/// `questions.jsonl` entry for the node (single-writer, like
/// `ReviewRequested`). Additive variant: old logs never carry it.
QuestionAsked {
    node: String,
    question: String,
    #[serde(default)]
    options: Vec<String>,
},
/// The N-th answer matched the N-th asked question for a node
/// (count-based consumption, like `ReviewDecided`). `answered_by` is one
/// of `"human"`, `"supervisor"`, `"timeout"`.
QuestionAnswered {
    node: String,
    answer: String,
    answered_by: String,
},
```

`AttemptFinished` gains one field:

```rust
/// Agent session id captured from a finished attempt, for the `resume`
/// transport (spec Transport: resume). `None` when the agent surfaced no
/// session id or the transport does not resume. Additive.
#[serde(default)]
session: Option<String>,
```

### Exact channel files (`runs/<id>/`)

- `questions.jsonl`: entries `{seq, node, attempt, question, options}` (`options` an optional list of suggested answers; free-text answers are always allowed).
- `answers.jsonl`: entries `{seq, node, answer, answered_by}`.

Both are written atomically via the same append primitive `reviews.jsonl` uses (`OpenOptions::new().create(true).append(true)` then `writeln!` + `flush`, matching `crates/apb-engine/src/review.rs`). Only `drive` writes `events.jsonl`; the channels are the pre-event record every facade appends to.

### Exact marker protocol (`resume`/`reprompt` transports)

A stdout line containing exactly `<<<apb:question>>>` (constant `QUESTION_MARKER`), and on the next line a JSON object `{"question": "...", "options": ["...", ...]}` (`options` optional). The marker is parsed only for interactive nodes; on non-interactive nodes the literal text has no effect. Malformed JSON after the marker fails the attempt with a clear error naming the node (never a silent parse skip): a half-parsed question would park the run on a question nobody can read.

### Exact transport selection (`crates/apb-core/src/config.rs`, `crates/apb-engine/src/invocation.rs`)

`InvocationDef` gains one field:

```rust
/// Ask-transport ceiling for interactive nodes (spec 2026-07-20). The
/// engine treats it as a ceiling, not a promise: it downgrades at runtime
/// when the preferred transport fails to initialize.
#[serde(default)]
pub interaction: Interaction,
```

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Interaction {
    Live,
    Resume,
    #[default]
    Reprompt,
}
```

Built-in defaults set in `invocation::builtin` per agent: claude / claude-code = `Live`, codex = `Resume`, opencode = `Resume`, hermes = `Resume`, agy = `Reprompt`. Config-defined agents may override via `agents.<id>.invocation`. Runtime downgrade order: `Live` falls back to `Resume`, `Resume` falls back to `Reprompt`, whenever the preferred transport fails to initialize (missing session id, sidecar spawn failure). Every downgrade is journaled as `SupervisorAction { action: "interaction_downgraded", node: Some(<node>), detail: <reason> }`.

### Exact answer semantics

- `answered_by` values: `"human"`, `"supervisor"`, `"timeout"`.
- Policy: a node declaring `answer_by: human` rejects an answer arriving through the supervisor-token path (`answered_by == "supervisor"`) with an error instructing the supervisor to relay the question to the user. `answer_by: supervisor` accepts both. `answered_by: "timeout"` is always accepted (it is drive's own default-answer path).
- Node `timeout_seconds` clock excludes pending-question intervals: the asked-at and answered-at instants come from the journal (`QuestionAsked.ts` to `QuestionAnswered.ts`), and their sum is subtracted from the elapsed handed to `check_cancel_timeout`. A node that budgets 300 s of agent work must not be killed because a human took an hour to answer.

### Exact sidecar (live transport)

Hidden subcommand `apb __ask-server --run <id> --node <node> --attempt <n>`, a stdio MCP server (rmcp, the dependency apb-mcp already uses) exposing exactly one tool `ask_user(question: string, options?: string[]) -> string`. It appends to `questions.jsonl` via `post_question`, then polls `answers.jsonl` via `read_answers_after` for the matching answer, sending an MCP progress notification every 60 s so Claude Code's 30-minute stdio idle timer never fires; it returns the answer text. It resolves its run dir from `APB_RUN_DIR` (set at every agent spawn by `ConnectorEnvPolicy::apply`) and asserts the dir's basename equals `--run` and that `run`/`node` are safe segments. It never reads secrets and never sees the prompt. Injected for claude via `--mcp-config '<inline JSON>'` built with the current-exe path (same resolution `apb __drive-run` uses, `std::env::current_exe()`); the per-server `"timeout"` is set from `question_timeout_seconds` (in ms) or a very large value (about 28 h) when none is set. No `--strict-mcp-config`: the agent keeps its own configured servers.

### Exact test layout

New engine tests go to `crates/apb-engine/tests/suite/<name>.rs` plus one `mod` line in `crates/apb-engine/tests/main.rs`; same pattern for `apb-core`, `apb-mcp`, `apb-cli`, `apb-server`. Stub agents via the existing `tests/suite/common` helpers. Every wait bounded with a message naming what it waited for; bounded by construction, no bare sleeps unless timing is the subject.

### Version

Bump to 0.8.0 with `docs/release-notes/v0.8.0.md` titled `apb 0.8.0: interactive nodes` (Task 13).

---

### Task 1: Schema fields and V31/V32 validation

**Files:**
- Modify: `crates/apb-core/src/schema.rs` (four `AgentTask` fields per Global Constraints; the `AnswerBy` enum + `impl`)
- Modify: `crates/apb-core/src/validate.rs` (`check_interactive`, register in `validate()` after `check_edges`)
- Test: `crates/apb-core/tests/suite/` (existing schema/validate suite files; add cases, no new binary)

**Interfaces:**
- Produces: `apb_core::schema::AnswerBy` (with `is_default`, `as_str`) and the four `NodeKind::AgentTask` fields. Consumed by Tasks 2 (answer policy reads `answer_by`), 3 (progress surfaces `answer_by`), 4-6 (drive parking reads `interactive`, `question_timeout_seconds`, `default_answer`), 7 (transport selection), 13 (docs).
- Consumes: the existing `Issue`/`ValidationReport::error` convention (`crates/apb-core/src/validate.rs:16-77`).

- [ ] **Step 1: Write failing tests.** In the schema suite: an `agent_task` YAML with `interactive: true`, `answer_by: supervisor`, `question_timeout_seconds: 120`, `default_answer: "proceed"` round-trips (`Playbook::from_yaml` then re-serialize) with `interactive` true, `answer_by` == `AnswerBy::Supervisor`, both options set; a plain `agent_task` with none of the fields serializes without any of the four keys (assert `answer_by` is omitted when `Human`, via a string contains check on the serialized YAML). In the validate suite: (a) an `agent_task` with `answer_by: supervisor` but no `interactive: true` yields a V31 error whose message mentions `interactive: true`; (b) `interactive: true` with `default_answer: "x"` and no `question_timeout_seconds` yields V32 whose message mentions `question_timeout_seconds`; (c) a well-formed interactive node (`interactive: true`, `question_timeout_seconds: 60`, `default_answer: "x"`) produces neither V31 nor V32. Use the crate's existing `codes(&pb)` helper to assert `(code, Severity)` pairs.
- [ ] **Step 2: Run to verify failure** - `cargo test -p apb-core` (the fields and codes do not exist yet, so the suite fails to compile or the assertions miss).
- [ ] **Step 3: Implement.** Add the enum, the four fields, and `check_interactive`:

```rust
/// V31/V32: interactive-node companion fields (spec 2026-07-20). Only
/// `agent_task` carries `interactive`/`answer_by`/`question_timeout_seconds`/
/// `default_answer`, so the node-kind guard is implicit: a non-agent_task node
/// can never set them.
fn check_interactive(playbook: &Playbook, r: &mut ValidationReport) {
    for n in &playbook.nodes {
        if let NodeKind::AgentTask {
            interactive,
            answer_by,
            question_timeout_seconds,
            default_answer,
            ..
        } = &n.kind
        {
            let has_companion = !answer_by.is_default()
                || question_timeout_seconds.is_some()
                || default_answer.is_some();
            if !interactive && has_companion {
                r.error(
                    "V31",
                    Some(&n.id),
                    "interactive companion fields (`answer_by`, `question_timeout_seconds`, `default_answer`) require `interactive: true`".to_string(),
                );
            }
            if default_answer.is_some() && question_timeout_seconds.is_none() {
                r.error(
                    "V32",
                    Some(&n.id),
                    "`default_answer` requires `question_timeout_seconds` (it is the answer used when the timeout elapses)".to_string(),
                );
            }
        }
    }
}
```

  Add `check_interactive(playbook, &mut r); // V31, V32` to `validate()` alongside the other `check_*` calls.
- [ ] **Step 4: Run to green, gates, commit** - `cargo test -p apb-core`, then `cargo test --workspace` (touched apb-core), `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, then `git commit --signoff` - `feat(core): interactive agent_task fields with V31/V32 validation`.

---

### Task 2: Question/answer channels (`question.rs`)

**Files:**
- Create: `crates/apb-engine/src/question.rs` (the channel module, mirroring `crates/apb-engine/src/review.rs`)
- Modify: `crates/apb-engine/src/lib.rs` (declare `pub mod question;` and re-export `post_question`, `post_answer`, `read_questions_after`, `read_answers_after`, `PostedQuestion`, `PostedAnswer`, `AnswerRejected` alongside the existing `post_review` re-exports)
- Test: `crates/apb-engine/tests/suite/question_channel_test.rs` + one `mod` line in `crates/apb-engine/tests/main.rs`

**Interfaces:**
- Produces:
  - `pub struct PostedQuestion { pub seq: u64, pub node: String, pub attempt: u32, pub question: String, #[serde(default)] pub options: Vec<String> }`
  - `pub struct PostedAnswer { pub seq: u64, pub node: String, pub answer: String, pub answered_by: String }`
  - `pub fn post_question(run_dir: &Path, node: &str, attempt: u32, question: &str, options: Vec<String>) -> Result<u64, EngineError>`
  - `pub fn read_questions_after(run_dir: &Path, after_seq: Option<u64>) -> Result<Vec<PostedQuestion>, EngineError>`
  - `pub fn post_answer(run_dir: &Path, node: Option<&str>, answer: &str, answered_by: &str) -> Result<u64, EngineError>` (enforces the answer_by policy, then appends)
  - `pub fn read_answers_after(run_dir: &Path, after_seq: Option<u64>) -> Result<Vec<PostedAnswer>, EngineError>`
- Consumes: `apb_core::schema::AnswerBy` (Task 1) and `apb_engine::progress::load_run_playbook` (existing) to resolve a node's `answer_by` inside `post_answer`.

- [ ] **Step 1: Write failing tests** (unit tests inline in `question.rs` plus the integration suite). (a) `post_question` returns seq 0 then 1; `read_questions_after(None)` returns both; `read_questions_after(Some(0))` returns only the second; fields round-trip including empty `options`. (b) `post_answer` round-trip mirrors reviews. (c) Policy: build a run dir whose loaded playbook snapshot has an interactive node `ask` with `answer_by: human`; `post_answer(dir, Some("ask"), "hi", "supervisor")` returns `Err` whose Display contains `relay` and the word `human`; `post_answer(dir, Some("ask"), "hi", "human")` and `...,"timeout")` both succeed. (d) For a node with `answer_by: supervisor`, both `"human"` and `"supervisor"` succeed. Use a real prepared run dir written via the common helpers so `load_run_playbook` finds the snapshot; bound no waits (pure IO).
- [ ] **Step 2: Run to verify failure** - `cargo test -p apb-engine question_channel_test::` and the inline module (the module does not exist yet).
- [ ] **Step 3: Implement `question.rs`.** Copy the structure of `review.rs` exactly (module doc noting only drive writes events; `create_dir_all`; seq = current count; `OpenOptions` append; `writeln!` + `flush`; `read_*_after` with the `after_seq` filter). Split the posted record into fields directly on the struct (no separate command type is needed since there is no `#[serde(flatten)]` requirement here; keep `seq` first). `post_answer` resolves the target node (when `node` is `None`, the single node with a pending question, found by comparing `read_questions_after` count to `read_answers_after` count per node; error if zero or more than one pending), loads the run playbook via `progress::load_run_playbook(run_dir)`, reads that node's `answer_by`, and rejects when `answer_by == AnswerBy::Human && answered_by == "supervisor"`:

```rust
pub fn post_answer(
    run_dir: &Path,
    node: Option<&str>,
    answer: &str,
    answered_by: &str,
) -> Result<u64, EngineError> {
    let target = resolve_pending_node(run_dir, node)?; // errors when ambiguous/absent
    if answered_by == "supervisor"
        && answer_by_for(run_dir, &target) == AnswerBy::Human
    {
        return Err(EngineError::Invalid(format!(
            "node `{target}` is answer_by: human; relay this question to the user and post their answer verbatim rather than answering as the supervisor"
        )));
    }
    std::fs::create_dir_all(run_dir)?;
    let seq = read_answers_after(run_dir, None)?.len() as u64;
    let entry = PostedAnswer { seq, node: target, answer: answer.to_string(), answered_by: answered_by.to_string() };
    let line = serde_json::to_string(&entry).map_err(|e| EngineError::Yaml(e.to_string()))?;
    let mut f = OpenOptions::new().create(true).append(true).open(run_dir.join("answers.jsonl"))?;
    writeln!(f, "{line}")?;
    f.flush()?;
    Ok(seq)
}
```

  `answer_by_for` loads the playbook once and defaults to `AnswerBy::Human` when the node or snapshot is missing (fail safe: an unknown node cannot be answered by a supervisor).
- [ ] **Step 4: Run to green, gates, commit** - `cargo test -p apb-engine question_channel_test::`, then `cargo test --workspace`, fmt, clippy, `git commit --signoff` - `feat(engine): questions.jsonl and answers.jsonl channels with answer_by policy`.

---

### Task 3: Event variants, AttemptFinished.session, and progress fold

**Files:**
- Modify: `crates/apb-engine/src/event.rs` (`QuestionAsked`, `QuestionAnswered`, `AttemptFinished.session` per Global Constraints)
- Modify: `crates/apb-engine/src/state.rs` (fold the two new variants: they do not change node status, so match them into the no-op arms alongside `WakeRaised`)
- Modify: `crates/apb-engine/src/progress.rs` (`WaitingKind::Question`; `ProgressSummary.pending_question: Option<PendingQuestion>`; `PendingQuestion { node, question, options, answer_by, asked_at }`; fold logic reads the channel files so it surfaces a pending question even before drive journals `QuestionAsked`)
- Test: `crates/apb-engine/tests/suite/` (progress suite) plus inline event round-trip

**Interfaces:**
- Produces: `EventPayload::QuestionAsked`, `EventPayload::QuestionAnswered`, `AttemptFinished.session`; `apb_engine::progress::PendingQuestion` and `WaitingKind::Question`. Consumed by Task 4 (drive emits the events), Task 5 (`asked_at`/pending intervals), Task 7 (`session`), Task 8 (`run_status` surfaces `pending_question`), Task 12 (web reads `pending_question`).
- Consumes: `apb_engine::question::read_questions_after` / `read_answers_after` (Task 2), `apb_core::schema::AnswerBy` (Task 1).

- [ ] **Step 1: Write failing tests.** (a) Event round-trip: a `QuestionAsked` serializes with tag `"type":"question_asked"` and re-reads with the same fields; `QuestionAnswered` with tag `"question_answered"`; an old `attempt_finished` line without `session` deserializes with `session: None`; a new one with `session: Some("abc")` round-trips. (b) Progress fold: build a run dir with a `questions.jsonl` entry for interactive node `ask` (`answer_by: supervisor`) and no matching `answers.jsonl` entry; `from_run_dir` returns `waiting_on == Some("ask")`, `waiting_kind == Some(WaitingKind::Question)`, and `pending_question` == `Some` with the question text, options, `answer_by == "supervisor"`, and `asked_at` set (from the `QuestionAsked` event ts when present, else the channel append order); once a matching answer exists, `pending_question` is `None` and `waiting_kind` clears. Model these on the existing `pending_human_review_waits_with_kind` / `decided_human_review_clears_waiting` tests.
- [ ] **Step 2: Run to verify failure** - `cargo test -p apb-engine progress` and the inline event tests.
- [ ] **Step 3: Implement.** Add the two variants and the field to `event.rs`; add the no-op fold arms in `state.rs`. In `progress.rs`, add `WaitingKind::Question` (serializes `"question"`), the `PendingQuestion` struct (`#[derive(Debug, Clone, Serialize)]`, `answer_by: String`, `asked_at: u128`, `options: Vec<String>`), and `pending_question: Option<PendingQuestion>` on `ProgressSummary`. In the fold that builds `waiting_on`/`waiting_kind`, add a question check parallel to `review_pending`: a node has a pending question when its `read_questions_after(None)` count for the node exceeds its `read_answers_after(None)` count; the pending question is the first unanswered entry. Resolve `answer_by` from the run playbook (`load_run_playbook`), defaulting to `"human"`. Set `asked_at` from the matching `QuestionAsked` event ts if one exists, else `now_millis` at fold is wrong (non-deterministic); instead fall back to the channel entry order and leave `asked_at` as the `QuestionAsked` ts when journaled and `0` before drive journals it (the web treats `0` as "just now"). A pending question takes precedence over a pending review in the same run only if both exist (assert one deterministic order: question first, since a parked question is the tighter interactive wait). Update every existing `ProgressSummary { .. }` constructor site to fill `pending_question: None`.
- [ ] **Step 4: Run to green, gates, commit** - `cargo test -p apb-engine`, then `cargo test --workspace`, fmt, clippy, `git commit --signoff` - `feat(engine): question events, attempt session, pending_question in progress`.

---

### Task 4: Drive parking (reprompt/resume suspension path)

**Files:**
- Modify: `crates/apb-engine/src/scheduler.rs` (fold helpers `question_asked_count` / `question_answered_count` mirroring `review_decided_count`; an interactive-question park-and-poll branch; a `Suspended` internal outcome from the agent-task execution)
- Modify: `crates/apb-engine/src/scheduler/node.rs` (`execute_node` returns a suspension signal when the attempt produced a marker question rather than a finish)
- Modify: `crates/apb-engine/src/adapter.rs` (`AgentReport.question: Option<AskedQuestion>` set when the stream carried the marker; `AskedQuestion { question: String, options: Vec<String> }`; `pub const QUESTION_MARKER: &str = "<<<apb:question>>>"`)
- Test: `crates/apb-engine/tests/suite/interactive_reprompt_test.rs` + `mod` line

**Interfaces:**
- Produces: an internal `enum AttemptOutcome { Finished { status: NodeStatus, output: String, events: Vec<EventPayload> }, Suspended { question: String, options: Vec<String> } }` returned by the agent-task execution path; drive's interactive branch. Consumed by Tasks 5 (timeout expiry hooks into the park loop), 6 (marker scan feeds `AgentReport.question`), 7 (re-invocation on answer).
- Consumes: Task 2 channels, Task 3 events, Task 1 `interactive` field.

- [ ] **Step 1: Write failing tests** (drive a stub agent that emits the marker; the common helpers write executable stub scripts). (a) A one-node interactive playbook whose stub prints `<<<apb:question>>>` then `{"question":"Which DB?","options":["pg","sqlite"]}` and exits: drive parks (does not write `NodeFinished` for the node); assert within a bounded poll (deadline named "waiting for QuestionAsked for node ask") that the journal has exactly one `QuestionAsked` with the text and options and no `NodeFinished` yet. Then `post_answer(run_dir, Some("ask"), "pg", "human")`; assert (bounded, "waiting for QuestionAnswered") the journal gains one `QuestionAnswered {answer:"pg", answered_by:"human"}`, a `WakeRaised`, and then the node finishes. (b) Count-based consumption: a stub that asks twice across two suspensions (marker, then on re-invocation a different marker, then finishes) produces exactly two `QuestionAsked` and two `QuestionAnswered` in order, each answered before the next is asked. Bound the whole test by construction (the stub only proceeds when its answer file exists; no timing assumptions). Run drive on a helper thread with a joined deadline.
- [ ] **Step 2: Run to verify failure** - `cargo test -p apb-engine interactive_reprompt_test::`.
- [ ] **Step 3: Implement.** In `adapter.rs` add `QUESTION_MARKER`, `AskedQuestion`, and `AgentReport.question: Option<AskedQuestion>` (default `None`; the marker scan itself is Task 6 - here just add the field and set it to `None` so the type compiles, and add a minimal scan in the headless path scanning the collected stdout for the marker line so the reprompt test's stub is honored; the full stream-path scan and malformed-JSON failure land in Task 6). In `node.rs`, when the agent-task attempt returns an `AgentReport` with `question: Some(q)` on an interactive node, return `AttemptOutcome::Suspended { question, options }` instead of composing `NodeFinished` (still journal `attempt_started`/`attempt_finished` at spawn/return as today, the attempt genuinely ran). In `scheduler.rs`, add `question_asked_count(events, node)` and `question_answered_count(events, node)` (filter-count, exactly like `review_decided_count`). Add the interactive branch to the drive loop shared tail: when the just-run node returned `Suspended`, `post_question(run_dir, node, attempt, &question, options)` (once per suspension), emit `QuestionAsked` if `question_asked_count <= question_answered_count` (declare-once, loop-resilient, mirroring the human_review `ReviewRequested` guard), raise a `WakeRaised { trigger: Anomaly, node, detail: "interactive question" }` so `supervisor_wait_event` returns (Task 8), then park: `read_answers_after(run_dir, None)` filtered to the node, take the `question_answered_count`-th entry; if present, emit `QuestionAnswered` and re-invoke the node (Task 7 supplies the transcript/resume prompt; for now a fresh re-invocation with the answer appended); if absent, `std::thread::sleep(AWAIT_CONTROL_POLL); continue;` exactly like the `HumanReview` branch. A parked interactive node composes with stop/abort/resume through the same existing paths (it behaves like a waiting human_review).
- [ ] **Step 4: Run to green, gates, commit** - `cargo test -p apb-engine`, `cargo test --workspace`, fmt, clippy, `git commit --signoff` - `feat(engine): interactive question parking in the drive loop`.

---

### Task 5: Timeout semantics

**Files:**
- Modify: `crates/apb-engine/src/scheduler.rs` (question_timeout_seconds expiry in the park loop; `answered_by: "timeout"`)
- Modify: `crates/apb-engine/src/liveness.rs` or `crates/apb-engine/src/progress.rs` (a shared `pending_interval_ms(events, node) -> u128` summing `QuestionAsked`-to-`QuestionAnswered` intervals for a node)
- Modify: `crates/apb-engine/src/adapter.rs` and `crates/apb-engine/src/scheduler/node.rs` (subtract the node's pending interval from the elapsed handed to `check_cancel_timeout`; for the reprompt path this is naturally zero within a single attempt, but the helper is wired so the live path in Task 11 reuses it)
- Test: `crates/apb-engine/tests/suite/interactive_timeout_test.rs` + `mod` line

**Interfaces:**
- Produces: `pub fn pending_interval_ms(events: &[Event], node: &str) -> u128`. Consumed by Task 11 (live attempt timeout exclusion).
- Consumes: Task 3 events, Task 4 park loop, Task 1 `question_timeout_seconds` / `default_answer`.

- [ ] **Step 1: Write failing tests** (timing IS the subject here, so a short real timeout is honest; keep it small). (a) `pending_interval_ms`: a hand-built journal with `QuestionAsked` at ts 1000 and `QuestionAnswered` at ts 4000 for node `ask` returns 3000; two rounds sum; an open (unanswered) question contributes nothing (bounded, pure). (b) Expiry with default: an interactive node with `question_timeout_seconds: 1` and `default_answer: "proceed"`, a stub that asks once and, on re-invocation carrying `proceed`, finishes; do NOT post an answer; assert (bounded deadline named "waiting for timeout QuestionAnswered") the journal gains `QuestionAnswered { answer:"proceed", answered_by:"timeout" }` and the node then succeeds. (c) Expiry without default: `question_timeout_seconds: 1`, no `default_answer`; do not answer; assert the attempt fails with a `NodeFinished` status Failed whose output/error names the node and the question timeout, and no `QuestionAnswered` is written. Use the shortest timeout that distinguishes pass from fail; comment that timing is the subject.
- [ ] **Step 2: Run to verify failure** - `cargo test -p apb-engine interactive_timeout_test::`.
- [ ] **Step 3: Implement.** Add `pending_interval_ms`. In the drive park loop (Task 4), when the node's `question_timeout_seconds` is `Some(secs)` and `now_millis().saturating_sub(asked_at) >= secs*1000` and no answer has arrived: if `default_answer` is `Some(ans)`, `post_answer(run_dir, Some(node), &ans, "timeout")` (the `"timeout"` answered_by bypasses the human policy) and let the next poll iteration consume it normally; if `default_answer` is `None`, stop parking and finish the node as `NodeStatus::Failed` with output `format!("interactive node `{node}` timed out after {secs}s waiting for an answer and has no default_answer")` (a real `NodeFinished`, routed into the usual failure handling). Wire `pending_interval_ms` into the per-attempt elapsed used by `check_cancel_timeout` where the node timeout is enforced (a no-op subtraction in the reprompt path; the live path in Task 11 depends on it).
- [ ] **Step 4: Run to green, gates, commit** - `cargo test -p apb-engine`, `cargo test --workspace`, fmt, clippy, `git commit --signoff` - `feat(engine): question timeout with default_answer and pending-interval exclusion`.

---

### Task 6: Marker protocol and reprompt re-invocation

**Files:**
- Modify: `crates/apb-engine/src/adapter.rs` (full marker scan in both `run_headless` and the stream path; malformed-JSON failure naming the node; the marker is scanned only when the task is interactive)
- Modify: `crates/apb-engine/src/scheduler/node.rs` (append the marker contract paragraph to an interactive node's prompt for `resume`/`reprompt` agents; on re-invocation after an answer, append the full Q&A transcript of this node visit to the original prompt)
- Modify: `crates/apb-engine/src/adapter.rs` (`AgentTask` gains `interactive: bool` and `node: &'a str` so the adapter knows to scan and can name the node in the error)
- Test: `crates/apb-engine/tests/suite/interactive_reprompt_test.rs` (extend), `crates/apb-engine/tests/suite/marker_test.rs` + `mod` line

**Interfaces:**
- Consumes: Task 4 `QUESTION_MARKER` / `AskedQuestion` / `AttemptOutcome::Suspended`.
- Produces: the transcript-carrying re-invocation prompt; the malformed-marker error. Consumed by Task 7 (resume path overrides the transcript with a session resume when available).

- [ ] **Step 1: Write failing tests.** (a) A stub that prints the marker plus valid JSON with options is parsed into `AskedQuestion { question, options }` and suspends (already covered in Task 4; here assert the options survive). (b) A stub that prints the marker plus `{"question":` (truncated JSON) fails the attempt with an error whose text contains the node id and `question`/`marker`; assert no `QuestionAsked` and no park. (c) Non-interactive node: a stub that prints the literal marker line runs to a normal finish with the marker text in its output (no suspension). (d) Re-invocation transcript: after answering `pg`, assert the stub's re-invocation received a prompt containing the original prompt and a quoted `Q: ... A: pg` block (the stub echoes its prompt to a file the test reads). Answers are rendered as a quoted Q&A block with no template expansion (V13 namespaces do not apply inside answer text).
- [ ] **Step 2: Run to verify failure** - `cargo test -p apb-engine marker_test:: interactive_reprompt_test::`.
- [ ] **Step 3: Implement.** Add `interactive: bool` and `node: &'a str` to `AgentTask`; every construction site fills them (interactive from the node, false for internal calls like context compaction). In the adapter, scan collected/streamed stdout for a line equal to `QUESTION_MARKER` only when `task.interactive`; the next non-empty line is parsed as `serde_json::from_str::<AskedQuestion-JSON>` where the JSON shape is `{ question: String, #[serde(default)] options: Vec<String> }`; on parse error return `Err((ErrorClass::Transport, format!("interactive node `{}` printed a malformed question after the {QUESTION_MARKER} marker: {e}", task.node)))`. On success set `AgentReport.question = Some(..)`. In `node.rs`, for an interactive node on a `resume`/`reprompt` agent append the exact marker contract paragraph from the spec to the prompt (the wording in the spec Transport: resume/reprompt block, verbatim). On re-invocation after an answer, build the follow-up prompt as the original rendered prompt followed by a `## prior questions and answers` block listing each `Q: <question>` / `A: <answer>` for this node visit (read from `read_questions_after`/`read_answers_after` filtered to the node), rendered as plain quoted text.
- [ ] **Step 4: Run to green, gates, commit** - `cargo test -p apb-engine`, `cargo test --workspace`, fmt, clippy, `git commit --signoff` - `feat(engine): marker protocol scan and reprompt transcript re-invocation`.

---

### Task 7: Interaction field and resume transport

**Files:**
- Modify: `crates/apb-core/src/config.rs` (`InvocationDef.interaction`; `Interaction` enum per Global Constraints)
- Modify: `crates/apb-engine/src/invocation.rs` (per-agent built-in `interaction` defaults in `builtin`)
- Modify: `crates/apb-engine/src/adapter.rs` (session-id capture from a finished attempt per agent into `AgentReport.session`; resume re-invocation flags per agent)
- Modify: `crates/apb-engine/src/scheduler/node.rs` (on answer, choose resume vs reprompt by the resolved `interaction`; downgrade to reprompt and journal `SupervisorAction { action: "interaction_downgraded", ... }` when capture or resume fails; write `AttemptFinished.session`)
- Test: `crates/apb-engine/tests/suite/interaction_defaults_test.rs`, `crates/apb-engine/tests/suite/resume_capture_test.rs` + `mod` lines; fixtures under `crates/apb-engine/tests/fixtures/`

**Interfaces:**
- Produces: `apb_core::config::Interaction`; `AgentReport.session`; the resume/reprompt decision and downgrade. Consumed by Task 11 (live is the third variant on the same field), Task 13 (docs matrix).
- Consumes: Task 6 re-invocation, Task 3 `AttemptFinished.session`.

- [ ] **Step 1: Write failing tests.** (a) `invocation::builtin("claude").unwrap().interaction == Interaction::Live`; codex/opencode/hermes == `Resume`; agy == `Reprompt`; a config agent with `interaction: reprompt` overrides. (b) Session capture from recorded fixture outputs (not live agents): a claude `stream-json`/`--output-format json` fixture with a `session_id` field yields `AgentReport.session == Some(<id>)`; a codex session-JSONL fixture, an opencode fixture, and a hermes fixture each yield their id; an output with no id yields `None`. (c) Resume vs reprompt: a `resume` stub whose first invocation asks and whose session capture succeeds is re-invoked with the agent's resume flag and the answer as the follow-up prompt (the stub records its argv to a file); assert the resume flag is present and the transcript is NOT appended. (d) Downgrade: a `resume` stub whose output carries no session id causes a `SupervisorAction { action: "interaction_downgraded", node: Some("ask"), detail: <contains "session"> }` and a reprompt re-invocation (transcript appended). Bound every drive on a joined deadline.
- [ ] **Step 2: Run to verify failure** - `cargo test -p apb-engine interaction_defaults_test:: resume_capture_test::`.
- [ ] **Step 3: Implement.** Add `interaction` to `InvocationDef` and `Interaction` to config; extend the `mk` closure in `builtin` with an `interaction` argument and set the five defaults. Add session capture: a `fn capture_session(agent_id: &str, report_raw: &str) -> Option<String>` in the adapter parsing per agent (claude: `session_id` from the JSON/stream-json terminal result; codex: the session id from its exec output; opencode: session id; hermes: session name), returning `None` on absence. Set `AgentReport.session` from it and write it into `AttemptFinished.session`. In `node.rs`, resolve the node's agent `interaction` from the run manifest's resolved invocation; on answer: `Live` (handled in Task 11) or `Resume` with a captured session -> re-invoke with the agent's resume flags (claude `--resume <id>`, codex `exec resume <id>`, opencode `--session <id>`, hermes `--resume <id>`) and the answer as the follow-up prompt; `Resume` without a captured session, or a resume spawn failure -> journal `interaction_downgraded` and fall back to the Task 6 reprompt path; `Reprompt` -> the Task 6 path directly.
- [ ] **Step 4: Run to green, gates, commit** - `cargo test -p apb-engine`, `cargo test --workspace`, fmt, clippy, `git commit --signoff` - `feat(engine): interaction transport ceiling, session capture, resume re-invocation`.

---

### Task 8: MCP run_answer, run_status pending_question, supervisor wake

**Files:**
- Modify: `crates/apb-mcp/src/tools.rs` (`run_answer` per the answer-path spec; `run_status` surfaces `pending_question`)
- Modify: `crates/apb-mcp/src/server/args.rs` (`RunAnswerArgs`)
- Modify: `crates/apb-mcp/src/server/run.rs` (register `run_answer` in `run_router`; the human path takes `run_id`; the supervisor path resolves a session token)
- Modify: `docs/MCP.md` (supervisor contract paragraph for interactive questions)
- Test: `crates/apb-mcp/tests/suite/` (run_answer suite, run_status suite)

**Interfaces:**
- Produces: MCP tool `run_answer`; `run_status.pending_question`. Consumed by Task 9 (CLI shares `post_answer`), Task 12 (web POST answer), the supervisor.
- Consumes: Task 2 `post_answer`, Task 3 `pending_question`, Task 4 wake, the existing `resolve_session` (`crates/apb-mcp/src/server/mod.rs:135`) and `wait_wake` (`crates/apb-engine/src/inspect.rs:44`).

Ambiguity resolution (documented in the final report): the spec names a single `run_answer { run_id, node?, answer }` tool that behaves as `answered_by: human` on the plain run-tool path and `answered_by: supervisor` on the supervisor-token path. rmcp tool names must be unique, so `RunAnswerArgs` carries an optional `token`: when `token` is present the server `resolve_session`s it (capability `observe`) and calls `post_answer(.., answered_by="supervisor")`; otherwise it uses `run_id` and calls `post_answer(.., answered_by="human")`. Exactly one of `run_id`/`token` identifies the run. The `answer_by: human` rejection surfaces from `post_answer` (Task 2), so a supervisor answering a human-only node gets the relay instruction back.

```rust
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunAnswerArgs {
    /// Run to answer (human/operator path). Provide this OR `token`.
    #[serde(default)]
    pub run_id: Option<String>,
    /// Supervisor session token (supervisor path). Provide this OR `run_id`.
    #[serde(default)]
    pub token: Option<String>,
    /// The interactive node; omit when exactly one question is pending.
    #[serde(default)]
    pub node: Option<String>,
    pub answer: String,
    #[serde(default)]
    pub workspace: Option<String>,
}
```

- [ ] **Step 1: Write failing tests.** (a) `run_answer` on a fixture run with one pending question and `run_id` posts an `answers.jsonl` entry with `answered_by == "human"` and returns `{ posted_seq }`. (b) `run_status` on that fixture includes `pending_question: { node, question, options, answer_by, asked_at }` and `progress.waiting_kind == "question"`. (c) The supervisor path: with a minted session token, `run_answer` on an `answer_by: supervisor` node posts `answered_by == "supervisor"`; on an `answer_by: human` node it returns an error whose text contains `relay`. (d) After a `run_answer`, `supervisor_wait_event` (bounded timeout) returns a wake for the question (assert the wake is non-null within the timeout, since Task 4 raised one). Reuse the crate's stub/fixture helpers; bound the wait_event call by its `timeout_ms`.
- [ ] **Step 2: Run to verify failure** - `cargo test -p apb-mcp`.
- [ ] **Step 3: Implement.** Add `tools::run_answer(root, run_id, node, answer, answered_by)` calling `apb_engine::post_answer`; add `pending_question` to the `run_status` JSON (read straight from `apb_engine::progress::from_run_dir`'s `pending_question`, serialized). Register the tool in `run_router` following the `review_decide` handler shape; when `token` is set, `self.resolve_session(&token, "run_answer")` (add `"run_answer" => "observe"` to `capability_for_tool`) and pass `answered_by="supervisor"`, else validate `run_id` and pass `"human"`. In `docs/MCP.md`, add the supervisor contract paragraph: relay `answer_by: human` questions to the user verbatim in the user's chat language, return the answer verbatim, never answer such questions yourself; `answer_by: supervisor` questions may be answered directly; escalate when unsure.
- [ ] **Step 4: Run to green, gates, commit** - `cargo test -p apb-mcp`, then `cargo test --workspace` (apb-engine unaffected here, but run it since apb-core/engine were touched earlier in the branch and the MCP surface consumes them), fmt, clippy, `git commit --signoff` - `feat(mcp): run_answer, pending_question in run_status, supervisor relay contract`.

---

### Task 9: CLI apb answer and waiting-on-question display

**Files:**
- Modify: `crates/apb-cli/src/main.rs` (`Answer` subcommand; dispatch)
- Modify: `crates/apb-cli/src/run.rs` (`answer_cmd`, modeled on `note_cmd`; `runs` listing and `doctor --run` show waiting-on-question state)
- Test: `crates/apb-cli/tests/suite/` (CLI answer suite, runs/doctor suites)

**Interfaces:**
- Consumes: Task 2 `post_answer`, Task 3 `pending_question`. Produces: nothing downstream.

- [ ] **Step 1: Write failing tests.** (a) `apb answer <run> pg` on a fixture run with one pending question exits 0 and appends an `answers.jsonl` entry with `answered_by == "human"`; `apb answer <run> --node ask pg` targets the node explicitly. (b) `apb answer <run> x` when no question is pending exits non-zero with a message naming the run and "no pending question". (c) `apb runs` on a run parked on a question shows a waiting-on-question marker for it; `apb doctor --run <id>` lists a check flagging the pending question (node + text). Use `env!("CARGO_BIN_EXE_apb")` or the crate's existing CLI harness; bound any subprocess wait with a named deadline.
- [ ] **Step 2: Run to verify failure** - `cargo test -p apb-cli`.
- [ ] **Step 3: Implement.** Add the subcommand mirroring `Note`/`Review`:

```rust
/// Answer an interactive node's pending question in a running run
Answer {
    run: String,
    /// The interactive node; omit when exactly one question is pending
    #[arg(long)]
    node: Option<String>,
    text: String,
},
```

  Dispatch `Some(Command::Answer { run, node, text }) => answer_cmd(&root, &run, node.as_deref(), &text)`. `answer_cmd` calls `apb_engine::post_answer(&run_dir, node, text, "human")` and prints `answer posted for {run} (seq {seq})` on success, `answer failed: {e}` to stderr with exit 2 on error (mirroring `note_cmd`). In `runs_cmd` and the per-run doctor, read `progress.pending_question` (or fold the channels) and print the waiting-on-question line (node id and question text, rendered as plain text, no shell interpolation).
- [ ] **Step 4: Run to green, gates, commit** - `cargo test -p apb-cli`, fmt, clippy, `git commit --signoff` - `feat(cli): apb answer and waiting-on-question display`.

---

### Task 10: Live sidecar (`apb __ask-server`)

**Files:**
- Create: `crates/apb-mcp/src/ask_server.rs` (rmcp stdio server with the single `ask_user` tool)
- Modify: `crates/apb-mcp/src/lib.rs` (declare `pub mod ask_server;`)
- Modify: `crates/apb-cli/src/main.rs` (hidden `__ask-server` subcommand + dispatch to `apb_mcp::ask_server::serve`)
- Test: `crates/apb-mcp/tests/suite/ask_server_test.rs` + `mod` line

**Interfaces:**
- Produces: `pub fn serve(run: &str, node: &str, attempt: u32) -> anyhow::Result<()>` (blocking, runs the stdio server); resolves the run dir from `APB_RUN_DIR`. The tool `ask_user(question, options?) -> string`. Consumed by Task 11 (injected into claude).
- Consumes: Task 2 `post_question` / `read_answers_after`.

- [ ] **Step 1: Write failing tests** (direct stdio MCP test, no live agent). Spawn `env!("CARGO_BIN_EXE_apb") __ask-server --run <id> --node ask --attempt 1` with `APB_RUN_DIR` pointed at a tempdir run, connect an rmcp stdio client (or drive the tool over the pipe), call `ask_user("Which DB?", ["pg","sqlite"])` on one thread; on another thread poll `questions.jsonl` (bounded deadline named "waiting for ask_user to post the question"), then `post_answer(run_dir, Some("ask"), "pg", "human")`; assert the tool call returns `"pg"` within a bounded deadline named "waiting for ask_user to return the answer". Reap the child on every path (RAII guard built before the first assertion). A second test asserts a malformed/absent `APB_RUN_DIR` exits non-zero with a message naming `APB_RUN_DIR`.
- [ ] **Step 2: Run to verify failure** - `cargo test -p apb-mcp ask_server_test::`.
- [ ] **Step 3: Implement `ask_server.rs`.** A minimal rmcp server (same `#[tool]`/`tool_router` machinery `crates/apb-mcp/src/server/` uses) exposing one tool. The handler validates `run`/`node` are safe segments, asserts `APB_RUN_DIR`'s basename equals `run`, `post_question(run_dir, node, attempt, question, options)`, then loops: `read_answers_after(run_dir, Some(prev))` filtered to the node for the entry matching this question's index; sleep `Duration::from_millis(200)` between polls; every 60 s send an MCP progress notification through the request's progress token (via the rmcp peer) so the 30-minute idle timer never fires; return the answer text when it arrives. The loop is bounded by the client-side tool timeout (set in the injection JSON, Task 11), not by an internal deadline (the human may genuinely take hours); the test bounds itself by answering promptly. In `main.rs` add `#[command(hide = true, name = "__ask-server")] AskServer { #[arg(long)] run: String, #[arg(long)] node: String, #[arg(long)] attempt: u32 }` and dispatch to `apb_mcp::ask_server::serve(&run, &node, attempt)`.
- [ ] **Step 4: Run to green, gates, commit** - `cargo test -p apb-mcp ask_server_test::`, `cargo test --workspace`, fmt, clippy, `git commit --signoff` - `feat: apb __ask-server live-question MCP sidecar`.

---

### Task 11: Live injection for claude

**Files:**
- Modify: `crates/apb-engine/src/adapter.rs` (`--mcp-config` JSON construction for interactive `live` nodes; the live prompt paragraph; concurrent channel observation during a live attempt journals `QuestionAsked`/`QuestionAnswered` and raises wakes; timeout exclusion via `pending_interval_ms`; downgrade to `resume`/`reprompt` when the agent is not live-capable)
- Modify: `crates/apb-engine/src/scheduler/node.rs` (select the live path when the resolved `interaction` is `Live`; pass run_dir/node/attempt into the adapter)
- Test: `crates/apb-engine/tests/suite/interactive_live_test.rs` + `mod` line

**Interfaces:**
- Consumes: Task 7 `Interaction::Live`, Task 10 sidecar, Task 5 `pending_interval_ms`, Task 2 channels, Task 3 events.
- Produces: the live transport end to end. Consumed by Task 13 (docs).

- [ ] **Step 1: Write failing tests.** (a) Injection JSON: a unit test asserts the constructed `--mcp-config` argument is valid JSON with `mcpServers.apb.command == <current-exe>`, `args` == `["__ask-server","--run",<id>,"--node",<node>,"--attempt",<n>]`, and `timeout` == the `question_timeout_seconds`-derived ms (or the large default when none). (b) Progress-notification cadence: an injection-shaped test asserts that during a bounded simulated wait the sidecar emits a progress notification at the 60 s cadence (drive the sidecar with a short cadence override behind a test-only env or const so the test does not wait 60 s; comment that timing is the subject and the honest value is 60 s). (c) A `live` stub that uses the sidecar (a stub claude that shells `apb __ask-server` and blocks) asks a question; drive (concurrent observation) journals `QuestionAsked` and, after `post_answer`, `QuestionAnswered` and a wake, and the attempt then finishes without a re-invocation (assert exactly one `attempt_started` for the node). Bound every wait by a named deadline. (d) Downgrade: forcing the live path on an agent whose `interaction` resolves below live journals `interaction_downgraded`.
- [ ] **Step 2: Run to verify failure** - `cargo test -p apb-engine interactive_live_test::`.
- [ ] **Step 3: Implement.** When an interactive node's resolved `interaction` is `Live` and the agent is claude/claude-code, build the injection JSON with `serde_json::json!` using `std::env::current_exe()` (falling back to a `SupervisorAction { action: "interaction_downgraded", detail: "current_exe unavailable" }` and the resume/reprompt path on failure) and append it as `--mcp-config <json>` to the claude argv; append the live prompt paragraph (the tool exists, when to use it, and that free-form questions to the user go through it rather than being answered by assumption). Run the live attempt so drive observes the channels concurrently: the attempt spawns the agent (which blocks in the tool), and the surrounding loop polls `read_questions_after`/`read_answers_after`, journaling `QuestionAsked` on a new entry, raising a `WakeRaised`, and journaling `QuestionAnswered` when the answer appears, at `AWAIT_CONTROL_POLL` cadence, until the agent process exits. Exclude `pending_interval_ms(node)` from the elapsed handed to `check_cancel_timeout` so a blocked-on-question agent is not killed as hung (the EOF/exit budgets from the reliability work stay in force: the agent process is alive and its pipes are open while blocked). Do not inject or scan the marker for `live` nodes (the marker is a resume/reprompt concern).
- [ ] **Step 4: Run to green, gates, commit** - `cargo test -p apb-engine`, `cargo test --workspace`, fmt, clippy, `git commit --signoff` - `feat(engine): live interactive transport for claude via the ask-server sidecar`.

---

### Task 12: Web UI (server payload, endpoint, question panel)

**Files:**
- Modify: `crates/apb-server/src/lib.rs` (`get_run_handler` includes `pending_question`; new route `POST /api/runs/{id}/answer` delegating to `apb_engine::post_answer`; `AnswerBody`)
- Modify: `web/src/lib/types.ts` (`ProgressSummary.pending_question`, `PendingQuestion`), `web/src/lib/api.ts` (`postAnswer`), create `web/src/lib/questions.ts` (`pendingQuestions(events)` mirroring `reviews.ts` `pendingReviews`, plus a channel-derived fallback from the run payload's `pending_question`)
- Modify: `web/src/lib/RunProgress.svelte` (a question panel mirroring the human_review panel: question text as plain text, option buttons, a free-text field)
- Test: `web/src/lib/questions.test.ts` (vitest), plus a panel render test alongside the existing review/progress tests; `bun run check`

**Interfaces:**
- Consumes: Task 3 `pending_question`, Task 8 answer semantics.
- Produces: the web answer facade.

- [ ] **Step 1: Write failing tests** (vitest). `pendingQuestions` returns the pending question(s) from a fixture events array (a `question_asked` with no matching `question_answered`) and clears when answered, exactly as `pendingReviews` does; a run payload carrying `pending_question` renders the panel with the question text, an option button per option, and a free-text input (assert via the component test harness the existing review panel uses). `postAnswer(id, { node, answer })` issues `POST /api/runs/{id}/answer` with the right body (assert against the api test mock).
- [ ] **Step 2: Run to verify failure** - `cd web && bun run test`.
- [ ] **Step 3: Implement.** Server: add `pending_question` to the `get_run_handler` JSON (from `apb_engine::progress::from_run_dir`), a `POST /api/runs/{id}/answer` route with `AnswerBody { #[serde(default)] node: Option<String>, answer: String }` calling `apb_engine::post_answer(&run_dir, body.node.as_deref(), &body.answer, "human")` and returning `{ posted_seq }` (mirroring `post_review_handler`). Web: add the types, `postAnswer` in `api.ts` (mirroring `postReview`), `questions.ts`, and the panel in `RunProgress.svelte` gated on `progress?.waiting_kind === 'question'` (mirroring the `human_review` gate at `RunProgress.svelte:27`). Render question text as plain text (no markdown execution), option buttons that post the option verbatim, and a free-text field.
- [ ] **Step 4: Run to green, gates, commit** - `cd web && bun run test && bun run check`; then `cargo test -p apb-server`, fmt, clippy for the rust side; `git commit --signoff` - `feat(web): interactive question panel and answer endpoint`.

---

### Task 13: Documentation, matrix update, and version bump

**Files:**
- Modify: `docs/HOWTO-authoring.md` (interactive-nodes section with a YAML example using `interactive`, `answer_by`, `question_timeout_seconds`, `default_answer`)
- Modify: `README.md` (node capabilities line mentions interactive nodes; command table gains `apb answer`)
- Modify: `docs/INTERACTIVE-AGENTS.md` (flip the status column for what shipped in phases 1-2; mark phase-3 field-verification items - hermes `-z` with `--resume`, opencode session reliability - as still pending)
- Modify: root `Cargo.toml` and every inter-crate `version` pin (`grep -rn '0\.7\.0' Cargo.toml crates/*/Cargo.toml`); refresh `Cargo.lock` via a build
- Create: `docs/release-notes/v0.8.0.md` (heading style copied from `docs/release-notes/v0.7.0.md`, title `## apb 0.8.0: interactive nodes`, one paragraph = one line)
- Test: `crates/apb-mcp/tests/suite/` (a doc-namespace consistency check only if one already exists; otherwise none - docs are prose)

**Interfaces:**
- Consumes: the shipped behavior from Tasks 1-12 (documentation only). Produces: nothing downstream.

- [ ] **Step 1: Write/adjust the interactive YAML example and verify it validates.** Add a failing check to the authoring-docs test if the repo has one that parses fenced YAML examples (the reliability plan's Task 10 established that pattern for `{{run.instruction}}`); otherwise add an `apb-core` test that `Playbook::from_yaml` accepts the exact interactive example block from `HOWTO-authoring.md` and it validates clean (no V31/V32). Run it red first.
- [ ] **Step 2: Write the docs and bump the version.** Author the `HOWTO-authoring.md` interactive section, the README lines, and the `INTERACTIVE-AGENTS.md` status flips. Bump `0.7.0 -> 0.8.0` in `Cargo.toml` and all pins, refresh `Cargo.lock` with a build. Write `docs/release-notes/v0.8.0.md` covering: the feature (interactive `agent_task` nodes ask the user mid-run), the three transports and per-agent defaults, the answer facades (supervisor relay, `apb answer`, web panel), and Known limitations (live transport is claude-only this release; codex/opencode/hermes use resume/reprompt; agy uses reprompt; MCP elicitation not used; script/condition nodes are not interactive). Verify no em-dash, no exclamation marks, no CJK; every documented namespace/field name matches the schema verbatim.
- [ ] **Step 3: Gates, commit.** `cargo test --workspace`, `cd web && bun run test && bun run check`, fmt, clippy, `git commit --signoff` - `docs: interactive nodes authoring guide, matrix update, and 0.8.0 release notes`.

---

## Final verification (controller)

`cargo metadata --format-version 1 >/dev/null && code-ranker check .` (fix every violation, reading `code-ranker docs base <ID>` first); `cargo clippy --release --workspace --all-targets -- -D warnings`; `cargo nextest run --workspace` green with no test reported SLOW (the only local run that turns a hang into a named failure) plus `cargo test --workspace --doc`; `cargo test --workspace` (the plain runner exposes shared-state races); `cd web && bun run test && bun run check`; whole-branch review; PR (`--signoff` DCO satisfied on every commit); merge; tag v0.8.0.
