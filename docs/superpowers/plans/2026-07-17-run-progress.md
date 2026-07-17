# Run Progress Percentage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show a live, honest "percent done" for every run (runs list + run view), driven by per-node `expected_duration` estimates, cycle-aware, updating without a page reload.

**Architecture:** A new optional `expected_duration` field on every `Node` (parsed to seconds via a single `apb-core::duration` module with a named 120s default for agent_task/script) feeds a pure fold `apb_engine::progress::compute(playbook, events)` that returns `{ percent, label, waiting_on }`. Reporting agents scale cycle groups by posting a `run_progress_report` MCP tool, which travels the existing supervisor-command channel (`Control::Progress`) so drive stays the single writer of `events.jsonl` (materializing `EventPayload::RunProgress`). Server run endpoints and MCP `run_status` embed the summary; the Svelte dashboard renders a bar that already live-updates through `watch.rs`.

**Tech Stack:** Rust workspace (edition 2024; apb-core, apb-engine, apb-mcp, apb-server), serde/serde_yaml_ng, rmcp, axum + rust-embed; Svelte 5 runes, shadcn-svelte + Tailwind v4, bun + vite + vitest.

## Global Constraints
- No em-dashes (U+2014) and no exclamation marks anywhere in docs, strings, or prose. No CJK. Machine-facing fields are English.
- New `EventPayload` fields (and the new variant's fields) use `#[serde(default)]` only.
- State files are written atomically via `apb_core::fsutil` (temp + rename, 0600 on unix).
- Single-writer invariant: only the `drive` loop appends to `events.jsonl`. Tools post `Control::*` commands; drive materializes events.
- V15 is historically unused and MUST NOT be reused. New validator codes: V19 (warning), V20 (error).
- The 120s agent_task/script default lives in exactly one place: `apb_core::duration::DEFAULT_TASK_SECONDS`.
- TDD: every task writes a failing test first, then the minimal implementation.
- Gates before commit: `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`; for web changes `cd web && bun run check` and `bun run test`. Warm the code-ranker cache with `cargo metadata --format-version 1 >/dev/null`, then `code-ranker check .` must pass.
- Commit messages: conventional (`feat:` / `test:` / `docs:`), ending with the trailer `Co-Authored-By: Claude <noreply@anthropic.com>`. No AI-authorship marker in prose.

---

### Task 1: apb-core `expected_duration` field, duration parser, per-kind defaults

**Files:**
- Create: `crates/apb-core/src/duration.rs`
- Modify: `crates/apb-core/src/lib.rs` (add `pub mod duration;`)
- Modify: `crates/apb-core/src/schema.rs` (add `ExpectedDuration`, `Node.expected_duration`, `Node::expected_seconds`)
- Test: unit tests inside `crates/apb-core/src/duration.rs` and `crates/apb-core/src/schema.rs`

**Interfaces:**
- Produces `apb_core::duration::DEFAULT_TASK_SECONDS: u64 = 120`
- Produces `apb_core::duration::parse_duration_str(s: &str) -> Option<u64>`
- Produces `apb_core::schema::ExpectedDuration` enum: `Seconds(u64)`, `Text(String)`; `#[serde(untagged)]`; derives `Debug, Clone, PartialEq, Deserialize, Serialize`; method `pub fn parsed(&self) -> Option<u64>`
- Produces `apb_core::schema::Node.expected_duration: Option<ExpectedDuration>` (`#[serde(default)]`)
- Produces `apb_core::schema::Node::expected_seconds(&self) -> u64` (parsed value, else the per-kind default: agent_task/script = 120, all others = 0)

Steps:

- [ ] Write failing test. Append to a new `#[cfg(test)] mod tests` in `crates/apb-core/src/duration.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_integer_seconds_and_single_unit() {
        assert_eq!(parse_duration_str("90"), Some(90));
        assert_eq!(parse_duration_str("30s"), Some(30));
        assert_eq!(parse_duration_str("5m"), Some(300));
        assert_eq!(parse_duration_str("2h"), Some(7200));
        assert_eq!(parse_duration_str("  45s "), Some(45));
    }

    #[test]
    fn rejects_bad_values() {
        assert_eq!(parse_duration_str(""), None);
        assert_eq!(parse_duration_str("5x"), None);
        assert_eq!(parse_duration_str("1.5m"), None);
        assert_eq!(parse_duration_str("5m30s"), None);
        assert_eq!(parse_duration_str("m"), None);
    }
}
```
- [ ] Run it, expect a compile failure (module does not exist yet): `cargo test -p apb-core --lib duration`. Expected: `error[E0583]` / unresolved `duration` until the file and `pub mod duration;` exist; then the asserts drive the parser.
- [ ] Create `crates/apb-core/src/duration.rs`:
```rust
//! Node `expected_duration` parsing and per-kind defaults. The 120s
//! agent_task/script default lives here and nowhere else.

/// Default estimated wall time for an agent_task or script node with no
/// explicit `expected_duration`.
pub const DEFAULT_TASK_SECONDS: u64 = 120;

/// Parses an `expected_duration` scalar: a plain integer count of seconds
/// (`90`), or an integer with a single unit suffix (`30s`, `5m`, `2h`).
/// Returns None for anything else (empty, float, multi-unit, unknown suffix).
pub fn parse_duration_str(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<u64>() {
        return Some(n);
    }
    let (num, mult) = match s.as_bytes()[s.len() - 1] {
        b's' => (&s[..s.len() - 1], 1u64),
        b'm' => (&s[..s.len() - 1], 60u64),
        b'h' => (&s[..s.len() - 1], 3600u64),
        _ => return None,
    };
    let n: u64 = num.trim().parse().ok()?;
    n.checked_mul(mult)
}
```
- [ ] Add `pub mod duration;` to `crates/apb-core/src/lib.rs` (alphabetical among the existing `pub mod` lines).
- [ ] Run: `cargo test -p apb-core --lib duration`. Expected: both tests pass.
- [ ] Add the field and helpers to `crates/apb-core/src/schema.rs`. Insert the enum just above `pub struct Node`:
```rust
/// Estimated wall time of ONE execution of a node (spec 2026-07-17). Accepts
/// an integer count of seconds or a string with a single unit suffix; the
/// parse is validated (V20) and the value is read via `parsed()`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ExpectedDuration {
    Seconds(u64),
    Text(String),
}

impl ExpectedDuration {
    /// Seconds if the value parses, else None (an invalid string is caught by
    /// validator V20; callers fall back to the per-kind default).
    pub fn parsed(&self) -> Option<u64> {
        match self {
            ExpectedDuration::Seconds(n) => Some(*n),
            ExpectedDuration::Text(s) => crate::duration::parse_duration_str(s),
        }
    }
}
```
- [ ] Add the field to `Node` (after `title`, before `#[serde(flatten)] pub kind`):
```rust
    /// Estimated time of ONE execution (spec 2026-07-17). Absent -> the per-kind
    /// default (see `expected_seconds`). Additive to schema 2; no migration.
    #[serde(default)]
    pub expected_duration: Option<ExpectedDuration>,
```
- [ ] Add the `expected_seconds` method in `schema.rs` inside a new `impl Node` block (place it after the `impl Playbook` block):
```rust
impl Node {
    /// Expected seconds for progress weighting: the parsed `expected_duration`
    /// if present and valid, otherwise the per-kind default (agent_task/script
    /// = `duration::DEFAULT_TASK_SECONDS`, every other kind = 0).
    pub fn expected_seconds(&self) -> u64 {
        if let Some(ed) = &self.expected_duration
            && let Some(s) = ed.parsed()
        {
            return s;
        }
        match self.kind {
            NodeKind::AgentTask { .. } | NodeKind::Script { .. } => {
                crate::duration::DEFAULT_TASK_SECONDS
            }
            _ => 0,
        }
    }
}
```
- [ ] Write a failing schema test in the existing/`#[cfg(test)]` block of `schema.rs` (add one if none):
```rust
#[test]
fn expected_seconds_uses_value_or_kind_default() {
    let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi, profile: x }
  - { id: b, type: agent_task, prompt: hi, profile: x, expected_duration: 5m }
  - { id: c, type: agent_task, prompt: hi, profile: x, expected_duration: 90 }
  - { id: p1, type: prompt, prompt: hi }
  - { id: f, type: finish, outcome: success }
edges: []
"#;
    let pb = Playbook::from_yaml(yaml).unwrap();
    assert_eq!(pb.node("a").unwrap().expected_seconds(), 120);
    assert_eq!(pb.node("b").unwrap().expected_seconds(), 300);
    assert_eq!(pb.node("c").unwrap().expected_seconds(), 90);
    assert_eq!(pb.node("p1").unwrap().expected_seconds(), 0);
}
```
- [ ] Run: `cargo test -p apb-core --lib`. Expected: new test passes; existing tests still pass.
- [ ] Gates: `cargo fmt --all -- --check` and `cargo clippy -p apb-core --all-targets -- -D warnings`.
- [ ] Commit:
```
git add crates/apb-core/src/duration.rs crates/apb-core/src/lib.rs crates/apb-core/src/schema.rs
git commit -m "feat(core): add expected_duration node field and duration parser

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 2: Validator V19 warning and V20 invalid-value error

**Files:**
- Modify: `crates/apb-core/src/validate.rs` (add `check_expected_duration`, wire into `validate`)
- Test: `crates/apb-core/src/validate.rs` `#[cfg(test)]` (or the existing validator test file if present)

**Interfaces:**
- Consumes `apb_core::schema::Node::expected_duration` and `ExpectedDuration::parsed()` (Task 1), `apb_core::duration::DEFAULT_TASK_SECONDS` (Task 1)
- Produces validator issue `V19` (Warning): an `agent_task`/`script` node without `expected_duration`
- Produces validator issue `V20` (Error): a node whose `expected_duration` does not parse

Steps:

- [ ] Write failing test. Add to the `#[cfg(test)]` block of `validate.rs`:
```rust
#[test]
fn v19_warns_on_task_without_expected_duration_and_does_not_block() {
    let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: f }
"#;
    let pb = Playbook::from_yaml(yaml).unwrap();
    let r = validate(&pb, &ValidationContext::default());
    assert!(r.is_valid(), "V19 must be a warning, not an error");
    assert!(r.issues.iter().any(|i| i.code == "V19" && i.node.as_deref() == Some("a")));
}

#[test]
fn v20_errors_on_unparsable_expected_duration() {
    let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi, expected_duration: "5x" }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: f }
"#;
    let pb = Playbook::from_yaml(yaml).unwrap();
    let r = validate(&pb, &ValidationContext::default());
    assert!(!r.is_valid());
    assert!(r.issues.iter().any(|i| i.code == "V20" && i.node.as_deref() == Some("a")));
}
```
- [ ] Run: `cargo test -p apb-core --lib v19 v20` (or `cargo test -p apb-core --lib expected_duration`). Expected: fails (no V19/V20 emitted).
- [ ] Add `check_expected_duration` to `validate.rs`:
```rust
/// V19 (warning): an agent_task or script node without `expected_duration`
/// (nudges authors; never blocks). V20 (error): an `expected_duration` value
/// that cannot be parsed to seconds.
fn check_expected_duration(playbook: &Playbook, r: &mut ValidationReport) {
    for n in &playbook.nodes {
        match &n.expected_duration {
            Some(ed) if ed.parsed().is_none() => {
                r.error(
                    "V20",
                    Some(&n.id),
                    format!(
                        "node `{}` has an unparsable expected_duration; use seconds like `90` or a single unit like `30s`, `5m`, `2h`",
                        n.id
                    ),
                );
            }
            None if matches!(n.kind, NodeKind::AgentTask { .. } | NodeKind::Script { .. }) => {
                r.warn(
                    "V19",
                    Some(&n.id),
                    format!(
                        "node `{}` has no expected_duration; progress will use the {}s default",
                        n.id,
                        crate::duration::DEFAULT_TASK_SECONDS
                    ),
                );
            }
            _ => {}
        }
    }
}
```
- [ ] Wire it into `validate` unconditionally (so a V20 error surfaces even alongside structural issues). Add this line right after the `check_unique_ids(playbook, &mut r);` call at the top of `validate`:
```rust
    check_expected_duration(playbook, &mut r); // V19, V20
```
- [ ] Run: `cargo test -p apb-core --lib`. Expected: new tests pass; existing tests unaffected.
- [ ] Gates: `cargo fmt --all -- --check`; `cargo clippy -p apb-core --all-targets -- -D warnings`.
- [ ] Commit:
```
git add crates/apb-core/src/validate.rs
git commit -m "feat(core): validate expected_duration with V19 warning and V20 error

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 3: `EventPayload::RunProgress`, `Control::Progress`, and drive drain handling

**Files:**
- Modify: `crates/apb-engine/src/event.rs` (add `RunProgress` variant)
- Modify: `crates/apb-engine/src/state.rs` (add no-op fold arm)
- Modify: `crates/apb-engine/src/control.rs` (add `Progress` variant)
- Modify: `crates/apb-engine/src/scheduler.rs` (top-of-loop drain + `await_control` call site)
- Modify: `crates/apb-engine/src/scheduler/supervisor.rs` (`await_control` signature + Progress arm)
- Test: `crates/apb-engine/tests/control_test.rs`

**Interfaces:**
- Produces `apb_engine::event::EventPayload::RunProgress { node_id: String, done: u64, total: u64, label: Option<String> }` (all four fields `#[serde(default)]`)
- Produces `apb_engine::control::Control::Progress { done: u64, total: u64, label: Option<String> }` (`#[serde(tag = "cmd", rename_all = "snake_case")]`, so it serializes as `{"cmd":"progress",...}`; `label` is `#[serde(default)]`)
- Changes `apb_engine::scheduler::supervisor::await_control` signature to `await_control(run_dir: &Path, log: &mut EventLog, cursor: Option<u64>, current_node: &str)` — drive stamps `node_id` from the node currently in hand, since the MCP tool does not carry it.

Steps:

- [ ] Write failing test. Append to `crates/apb-engine/tests/control_test.rs`:
```rust
#[test]
fn progress_control_serializes_with_cmd_tag() {
    use apb_engine::control::Control;
    let c = Control::Progress { done: 3, total: 14, label: Some("chapter 3 of 14".into()) };
    let s = serde_json::to_string(&c).unwrap();
    assert!(s.contains("\"cmd\":\"progress\""), "got {s}");
    assert!(s.contains("\"done\":3"));
    assert!(s.contains("\"total\":14"));
}
```
- [ ] Run: `cargo test -p apb-engine --test control_test progress_control_serializes_with_cmd_tag`. Expected: fails (no `Progress` variant).
- [ ] Add the `Control::Progress` variant to `crates/apb-engine/src/control.rs` (after `ContextAppend`):
```rust
    Progress {
        done: u64,
        total: u64,
        #[serde(default)]
        label: Option<String>,
    },
```
- [ ] Add the `EventPayload::RunProgress` variant to `crates/apb-engine/src/event.rs` (place after `ContextCompacted`, before `EnvironmentDriftAccepted`):
```rust
    /// An explicit cycle-progress report (spec 2026-07-17): the current
    /// iteration `done` of `total` for the cycle group anchored at `node_id`.
    /// Written by drive when it drains a `Control::Progress` command, never by a
    /// tool (single-writer). Fields default so old logs read unchanged.
    RunProgress {
        #[serde(default)]
        node_id: String,
        #[serde(default)]
        done: u64,
        #[serde(default)]
        total: u64,
        #[serde(default)]
        label: Option<String>,
    },
```
- [ ] Add the no-op fold arm to `RunState::fold` in `crates/apb-engine/src/state.rs` (progress does not affect run state; grouped with the other audit-only arms):
```rust
                EventPayload::RunProgress { .. } => {}
```
- [ ] Run `cargo build -p apb-engine` to surface any other exhaustive `match` on `EventPayload` (there should be none beyond `state::fold`; `inspect.rs` uses `_ => None`). Fix any that appear with a no-op arm.
- [ ] Update `await_control` in `crates/apb-engine/src/scheduler/supervisor.rs`: change the signature to add `current_node: &str`, and add a Progress arm that applies in place (like `ContextAppend`):
```rust
pub(crate) fn await_control(
    run_dir: &Path,
    log: &mut EventLog,
    cursor: Option<u64>,
    current_node: &str,
) -> Result<(Control, u64), EngineError> {
    let mut cursor = cursor;
    loop {
        for entry in read_control_after(run_dir, cursor)? {
            match entry.cmd {
                Control::ContextAppend { note } => {
                    log.append(EventPayload::SupervisorAction {
                        action: "context_append".into(),
                        node: None,
                        detail: note,
                    })?;
                    rebuild_context_md(run_dir)?;
                    cursor = Some(entry.seq);
                }
                Control::Progress { done, total, label } => {
                    log.append(EventPayload::RunProgress {
                        node_id: current_node.to_string(),
                        done,
                        total,
                        label,
                    })?;
                    cursor = Some(entry.seq);
                }
                other => return Ok((other, entry.seq)),
            }
        }
        std::thread::sleep(AWAIT_CONTROL_POLL);
    }
}
```
- [ ] Update the `await_control` call site in `crates/apb-engine/src/scheduler.rs` (around line 962) to pass the node in hand: `let (cmd, seq) = await_control(run_dir, log, control_cursor, &current)?;`. Add a defensive arm to the `match cmd` block (next to the `Control::ContextAppend { .. } => continue,` arm): `Control::Progress { .. } => continue,`.
- [ ] Add the top-of-loop Progress drain in `crates/apb-engine/src/scheduler.rs`. In the `for entry in read_control_after(run_dir, control_cursor)?` match, add this arm before the `Control::Retry { .. } | Control::ContinueFrom { .. } => { break; }` arm:
```rust
                Control::Progress { done, total, label } => {
                    log.append(EventPayload::RunProgress {
                        node_id: current.clone(),
                        done,
                        total,
                        label,
                    })?;
                    control_cursor = Some(entry.seq);
                }
```
- [ ] Run: `cargo test -p apb-engine --test control_test progress_control_serializes_with_cmd_tag`. Expected: passes.
- [ ] Run the full engine suite to confirm no regression in existing control/supervised tests: `cargo test -p apb-engine`.
- [ ] Gates: `cargo fmt --all -- --check`; `cargo clippy -p apb-engine --all-targets -- -D warnings`.
- [ ] Commit:
```
git add crates/apb-engine/src/event.rs crates/apb-engine/src/state.rs crates/apb-engine/src/control.rs crates/apb-engine/src/scheduler.rs crates/apb-engine/src/scheduler/supervisor.rs crates/apb-engine/tests/control_test.rs
git commit -m "feat(engine): RunProgress event and Control::Progress drain

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 4: Engine progress fold module (cycle scaling, clamping, measured durations)

**Files:**
- Create: `crates/apb-engine/src/progress.rs`
- Modify: `crates/apb-engine/src/lib.rs` (`pub mod progress;`)
- Test: unit tests in `crates/apb-engine/src/progress.rs`

**Interfaces:**
- Consumes `apb_core::schema::{Playbook, NodeKind}`, `Node::expected_seconds` (Task 1), `apb_engine::event::{Event, EventPayload}` incl. `RunProgress` (Task 3), `apb_engine::state::{RunState, NodeStatus, RunStatus}`
- Produces `apb_engine::progress::ProgressSummary { pub percent: u8, pub label: Option<String>, pub waiting_on: Option<String> }` (derives `Debug, Clone, Serialize`)
- Produces `apb_engine::progress::compute(playbook: &Playbook, events: &[Event]) -> ProgressSummary`
- Produces `apb_engine::progress::node_durations_seconds(events: &[Event]) -> std::collections::BTreeMap<String, u64>` (measured seconds per node, last occurrence wins)

Semantics fixed here (from the spec, do not re-derive elsewhere):
- Counted nodes = all playbook nodes whose latest status is not `Skipped`/`Cancelled`; each weighs `Node::expected_seconds()`.
- Nodes are grouped by strongly-connected component (Tarjan). A group is cyclic if it has more than one member or a self-loop edge.
- Non-cyclic singleton node `n`: `total += w`; `done += w` when its status is `Succeeded`.
- Cyclic group with one-pass seconds `P` (= sum of counted-member weights):
  - With a `RunProgress` report bound to any member (latest wins; `total==0` -> treat as 1; `done` clamped to `<= total`): `total += t*P`, `done += d*P`.
  - Without a report: `total += P`, `done += min(sum of Succeeded-member weights, P)`.
- `percent = min(100, round_down(100*done/total))`, `0` when `total==0`; forced to `100` when `run_status == Succeeded`.
- `label` = the latest `RunProgress.label` seen (any group).
- `waiting_on` = id of a node whose kind is `human_review`/`wait` and whose latest status is `Running`, else `None`.

Steps:

- [ ] Write failing tests. Create `crates/apb-engine/src/progress.rs` with the implementation stubbed to `todo!()` in `compute`, plus this `#[cfg(test)]` block:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, EventPayload, now_millis};
    use apb_core::schema::Playbook;

    fn ev(seq: u64, payload: EventPayload) -> Event {
        Event { seq, ts: now_millis(), payload }
    }

    fn linear_pb() -> Playbook {
        Playbook::from_yaml(
            r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi, expected_duration: 100 }
  - { id: b, type: agent_task, prompt: hi, expected_duration: 300 }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: b }
  - { from: b, to: f }
"#,
        )
        .unwrap()
    }

    #[test]
    fn weights_by_expected_seconds() {
        let pb = linear_pb();
        let events = vec![
            ev(0, EventPayload::RunStarted { playbook: "p".into(), version: "1.0.0".into() }),
            ev(1, EventPayload::NodeFinished { node: "a".into(), status: "succeeded".into(), attempt: 1, output: String::new() }),
        ];
        // done=100 of total=400 (a+b; start/finish weigh 0) -> 25
        assert_eq!(compute(&pb, &events).percent, 25);
    }

    #[test]
    fn retry_does_not_move_backward() {
        let pb = linear_pb();
        let mut events = vec![
            ev(0, EventPayload::RunStarted { playbook: "p".into(), version: "1.0.0".into() }),
            ev(1, EventPayload::NodeFinished { node: "a".into(), status: "succeeded".into(), attempt: 1, output: String::new() }),
        ];
        let before = compute(&pb, &events).percent;
        events.push(ev(2, EventPayload::RetryStarted { node: "b".into(), attempt: 2 }));
        assert_eq!(compute(&pb, &events).percent, before);
    }

    #[test]
    fn skipped_leaves_denominator() {
        let pb = linear_pb();
        let events = vec![
            ev(0, EventPayload::RunStarted { playbook: "p".into(), version: "1.0.0".into() }),
            ev(1, EventPayload::NodeFinished { node: "a".into(), status: "succeeded".into(), attempt: 1, output: String::new() }),
            ev(2, EventPayload::NodeFinished { node: "b".into(), status: "skipped".into(), attempt: 1, output: String::new() }),
        ];
        // b left the denominator; a is all remaining work -> 100
        assert_eq!(compute(&pb, &events).percent, 100);
    }

    fn cyclic_pb() -> Playbook {
        Playbook::from_yaml(
            r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: work, type: agent_task, prompt: hi, expected_duration: 100 }
  - { id: gate, type: condition, max_loops: 20 }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: work }
  - { from: work, to: gate }
  - { from: gate, to: work, condition: { type: node_status, node: work, equals: failure } }
  - { from: gate, to: f, condition: { type: node_status, node: work, equals: success } }
"#,
        )
        .unwrap()
    }

    #[test]
    fn cycle_scales_with_report_and_clamps() {
        let pb = cyclic_pb();
        // report done=3 of total=14 -> 3/14 of one-pass (100s) over 14*100 total.
        let events = vec![
            ev(0, EventPayload::RunStarted { playbook: "p".into(), version: "1.0.0".into() }),
            ev(1, EventPayload::RunProgress { node_id: "work".into(), done: 3, total: 14, label: Some("chapter 3 of 14".into()) }),
        ];
        let p = compute(&pb, &events);
        assert_eq!(p.percent, 21); // 300 done of 1400 total, rounded down
        assert_eq!(p.label.as_deref(), Some("chapter 3 of 14"));

        // clamp: done>total and total=0 must never panic
        let bad = vec![
            ev(0, EventPayload::RunStarted { playbook: "p".into(), version: "1.0.0".into() }),
            ev(1, EventPayload::RunProgress { node_id: "work".into(), done: 99, total: 0, label: None }),
        ];
        assert_eq!(compute(&pb, &bad).percent, 100);
    }

    #[test]
    fn succeeded_run_pins_to_100() {
        let pb = linear_pb();
        let events = vec![
            ev(0, EventPayload::RunStarted { playbook: "p".into(), version: "1.0.0".into() }),
            ev(1, EventPayload::RunFinished { outcome: "succeeded".into() }),
        ];
        assert_eq!(compute(&pb, &events).percent, 100);
    }
}
```
- [ ] Run: `cargo test -p apb-engine --lib progress`. Expected: fails at `todo!()`.
- [ ] Replace the stub with the real module body in `crates/apb-engine/src/progress.rs`:
```rust
//! Pure run-progress fold (spec 2026-07-17): a percent computed from persisted
//! events plus the playbook version bound to the run, exactly like
//! `RunState::fold`. No mutable state, so resume and a server restart show the
//! same number.

use std::collections::{BTreeMap, BTreeSet};

use apb_core::schema::{NodeKind, Playbook};
use serde::Serialize;

use crate::event::{Event, EventPayload};
use crate::state::{NodeStatus, RunState, RunStatus};

/// The run-progress summary surfaced by the server and MCP `run_status`.
#[derive(Debug, Clone, Serialize)]
pub struct ProgressSummary {
    pub percent: u8,
    pub label: Option<String>,
    pub waiting_on: Option<String>,
}

/// Strongly-connected components of the playbook graph (iterative Tarjan),
/// returned as groups of node ids. Node order follows `playbook.nodes`.
fn sccs(playbook: &Playbook) -> Vec<Vec<String>> {
    let ids: Vec<&str> = playbook.nodes.iter().map(|n| n.id.as_str()).collect();
    let index_of: BTreeMap<&str, usize> = ids.iter().enumerate().map(|(i, s)| (*s, i)).collect();
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); ids.len()];
    for e in &playbook.edges {
        if let (Some(&a), Some(&b)) = (index_of.get(e.from.as_str()), index_of.get(e.to.as_str())) {
            adj[a].push(b);
        }
    }
    let n = ids.len();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut counter = 0usize;
    let mut out: Vec<Vec<String>> = Vec::new();
    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        let mut call: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(&(v, ei)) = call.last() {
            if ei == 0 {
                index[v] = counter;
                low[v] = counter;
                counter += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if ei < adj[v].len() {
                call.last_mut().expect("frame exists").1 += 1;
                let w = adj[v][ei];
                if index[w] == usize::MAX {
                    call.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                if low[v] == index[v] {
                    let mut comp = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        comp.push(ids[w].to_string());
                        if w == v {
                            break;
                        }
                    }
                    out.push(comp);
                }
                call.pop();
                if let Some(&(parent, _)) = call.last() {
                    low[parent] = low[parent].min(low[v]);
                }
            }
        }
    }
    out
}

/// Computes the progress summary from events + the run's playbook version.
pub fn compute(playbook: &Playbook, events: &[Event]) -> ProgressSummary {
    let state = RunState::fold(events);
    let status = |id: &str| state.nodes.get(id).copied().unwrap_or(NodeStatus::Pending);
    let counted = |id: &str| !matches!(status(id), NodeStatus::Skipped | NodeStatus::Cancelled);

    let groups = sccs(playbook);
    let mut group_of: BTreeMap<&str, usize> = BTreeMap::new();
    for (gi, g) in groups.iter().enumerate() {
        for id in g {
            group_of.insert(id.as_str(), gi);
        }
    }
    let self_loops: BTreeSet<&str> = playbook
        .edges
        .iter()
        .filter(|e| e.from == e.to)
        .map(|e| e.from.as_str())
        .collect();
    let cyclic: Vec<bool> = groups
        .iter()
        .map(|g| g.len() > 1 || (g.len() == 1 && self_loops.contains(g[0].as_str())))
        .collect();

    // Latest report per group and the latest label overall.
    let mut report_of: BTreeMap<usize, (u64, u64)> = BTreeMap::new();
    let mut label: Option<String> = None;
    for e in events {
        if let EventPayload::RunProgress { node_id, done, total, label: lbl } = &e.payload {
            if let Some(&gi) = group_of.get(node_id.as_str()) {
                let total_c = if *total == 0 { 1 } else { *total };
                let done_c = (*done).min(total_c);
                report_of.insert(gi, (done_c, total_c));
            }
            if lbl.is_some() {
                label = lbl.clone();
            }
        }
    }

    let mut total: u128 = 0;
    let mut done: u128 = 0;
    for (gi, g) in groups.iter().enumerate() {
        let one_pass: u64 = g
            .iter()
            .filter(|id| counted(id))
            .filter_map(|id| playbook.node(id))
            .map(|n| n.expected_seconds())
            .sum();
        if one_pass == 0 {
            continue;
        }
        if cyclic[gi] {
            if let Some(&(d, t)) = report_of.get(&gi) {
                total += (t as u128) * one_pass as u128;
                done += (d as u128) * one_pass as u128;
            } else {
                total += one_pass as u128;
                let succ: u64 = g
                    .iter()
                    .filter(|id| status(id) == NodeStatus::Succeeded)
                    .filter_map(|id| playbook.node(id))
                    .map(|n| n.expected_seconds())
                    .sum();
                done += succ.min(one_pass) as u128;
            }
        } else {
            let id = &g[0];
            if let Some(n) = playbook.node(id) {
                let w = n.expected_seconds() as u128;
                total += w;
                if status(id) == NodeStatus::Succeeded {
                    done += w;
                }
            }
        }
    }

    let mut percent: u8 = if total == 0 {
        0
    } else {
        (done.saturating_mul(100) / total).min(100) as u8
    };
    if matches!(state.run_status, RunStatus::Succeeded) {
        percent = 100;
    }

    let waiting_on = playbook
        .nodes
        .iter()
        .find(|n| {
            matches!(n.kind, NodeKind::HumanReview { .. } | NodeKind::Wait { .. })
                && status(&n.id) == NodeStatus::Running
        })
        .map(|n| n.id.clone());

    ProgressSummary { percent, label, waiting_on }
}

/// Measured wall time per node in whole seconds, from NodeStarted to
/// NodeFinished; the last completion of a node wins (loops overwrite).
pub fn node_durations_seconds(events: &[Event]) -> BTreeMap<String, u64> {
    let mut start: BTreeMap<String, u128> = BTreeMap::new();
    let mut out: BTreeMap<String, u64> = BTreeMap::new();
    for e in events {
        match &e.payload {
            EventPayload::NodeStarted { node, .. } => {
                start.insert(node.clone(), e.ts);
            }
            EventPayload::NodeFinished { node, .. } => {
                if let Some(s) = start.get(node) {
                    out.insert(node.clone(), (e.ts.saturating_sub(*s) / 1000) as u64);
                }
            }
            _ => {}
        }
    }
    out
}
```
- [ ] Add `pub mod progress;` to `crates/apb-engine/src/lib.rs` (alphabetical, after `pub mod prepare`/`pub mod proc` region; it sits between `pub mod prepare`-family and `pub mod review`). Add a re-export line to the `pub use` region so callers can use a short path:
```rust
pub use progress::{ProgressSummary, compute as run_progress, node_durations_seconds};
```
- [ ] Run: `cargo test -p apb-engine --lib progress`. Expected: all progress tests pass.
- [ ] Gates: `cargo fmt --all -- --check`; `cargo clippy -p apb-engine --all-targets -- -D warnings`.
- [ ] Commit:
```
git add crates/apb-engine/src/progress.rs crates/apb-engine/src/lib.rs
git commit -m "feat(engine): progress fold with cycle scaling and measured durations

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 5: MCP `run_progress_report` tool and progress in `run_status`

**Files:**
- Modify: `crates/apb-mcp/src/tools.rs` (`run_progress_report`, add progress to `run_status`)
- Modify: `crates/apb-mcp/src/server/args.rs` (`ProgressReportArgs`)
- Modify: `crates/apb-mcp/src/server/run.rs` (register the tool)
- Test: `crates/apb-mcp` (add to `crates/apb-mcp/src/server/tests.rs` tool-name assertion + a `tools.rs` behavior test)

**Interfaces:**
- Consumes `apb_engine::control::Control::Progress` (Task 3), `apb_engine::progress::compute` (Task 4), `apb_engine::post_supervisor_command`
- Produces `crate::tools::run_progress_report(root: &Path, run_id: &str, done: u64, total: u64, label: Option<String>) -> Result<serde_json::Value, ToolError>` returning `{ "posted_seq": <u64> }`
- Produces `run_status` JSON gaining a `"progress"` key holding `ProgressSummary` (`{ percent, label, waiting_on }`)
- Produces MCP tool `run_progress_report(run_id, done, total, label?, workspace?)`

Steps:

- [ ] Write failing test. Add to `crates/apb-mcp/src/server/tests.rs` in the list of expected tool names (near the existing `"playbook_howto"` entry) the string `"run_progress_report"`. Then add a behavior test to `crates/apb-mcp/src/tools.rs` `#[cfg(test)]` (or the crate's tools test module) that a report posts a command and `run_status` carries the summary. If the crate has no `tools.rs` test module, add one:
```rust
#[cfg(test)]
mod progress_tests {
    use super::*;
    // Assumes a helper that starts a tiny run; if the crate has an existing
    // test harness for runs, reuse it. Otherwise this asserts the posting path.
    #[test]
    fn run_progress_report_posts_a_command() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        // minimal events + playbook so resolve_run_dir + run_status succeed
        std::fs::write(run_dir.join("events.jsonl"),
            "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n").unwrap();
        let out = run_progress_report(tmp.path(), "r1", 2, 5, Some("x".into())).unwrap();
        assert!(out.get("posted_seq").is_some());
        let control = std::fs::read_to_string(run_dir.join("control.jsonl")).unwrap();
        assert!(control.contains("\"cmd\":\"progress\""));
    }
}
```
- [ ] Run: `cargo test -p apb-mcp run_progress_report_posts_a_command` and `cargo test -p apb-mcp --lib` (tool-name test). Expected: both fail (symbol/tool missing).
- [ ] Add `run_progress_report` to `crates/apb-mcp/src/tools.rs` (next to `context_append`):
```rust
/// Reports cycle progress for the run's currently executing node group. Posts
/// a `Control::Progress` command; drive stamps the node and appends the
/// `RunProgress` event (single-writer). Callable by the executing agent or the
/// supervisor.
pub fn run_progress_report(
    root: &Path,
    run_id: &str,
    done: u64,
    total: u64,
    label: Option<String>,
) -> Result<Value, ToolError> {
    let seq = post_supervisor_command(root, run_id, Control::Progress { done, total, label })?;
    Ok(json!({ "posted_seq": seq }))
}
```
- [ ] Add the `"progress"` key to `run_status` in `tools.rs`:
```rust
pub fn run_status(root: &Path, run_id: &str) -> Result<Value, ToolError> {
    let dir = resolve_run_dir(root, run_id)?;
    let events = read_all(&dir).map_err(|e| ToolError::Engine(e.to_string()))?;
    let state = RunState::fold(&events);
    let nodes: BTreeMap<String, String> = state
        .nodes
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().to_string()))
        .collect();
    let progress = std::fs::read_to_string(dir.join("playbook.yaml"))
        .ok()
        .and_then(|y| apb_core::schema::Playbook::from_yaml(&y).ok())
        .map(|pb| apb_engine::progress::compute(&pb, &events));
    Ok(json!({
        "run_id": run_id,
        "run_status": state.run_status.as_str(),
        "nodes": nodes,
        "outputs": state.outputs,
        "progress": progress,
    }))
}
```
- [ ] Add `ProgressReportArgs` to `crates/apb-mcp/src/server/args.rs`:
```rust
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProgressReportArgs {
    pub run_id: String,
    /// Iterations completed in the current cycle group.
    pub done: u64,
    /// Total iterations planned for the current cycle group.
    pub total: u64,
    /// Optional human label shown next to the bar, e.g. "chapter 3 of 14".
    #[serde(default)]
    pub label: Option<String>,
    /// workspace_id of another workspace (spec 7). None - the current one.
    #[serde(default)]
    pub workspace: Option<String>,
}
```
- [ ] Register the tool in `crates/apb-mcp/src/server/run.rs` (add inside the `#[tool_router(...)] impl WfMcp` block, mirroring `run_status`):
```rust
    #[tool(
        description = "Report cycle progress for a run: done of total iterations of the current cycle group, with an optional label. Scales the progress bar for loops with a known amount of work.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn run_progress_report(
        &self,
        Parameters(ProgressReportArgs { run_id, done, total, label, workspace }): Parameters<ProgressReportArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        to_call_tool_result(tools::run_progress_report(&root, &run_id, done, total, label))
    }
```
- [ ] Run: `cargo test -p apb-mcp`. Expected: the posting test and the tool-name test pass.
- [ ] Gates: `cargo fmt --all -- --check`; `cargo clippy -p apb-mcp --all-targets -- -D warnings`.
- [ ] Commit:
```
git add crates/apb-mcp/src/tools.rs crates/apb-mcp/src/server/args.rs crates/apb-mcp/src/server/run.rs crates/apb-mcp/src/server/tests.rs
git commit -m "feat(mcp): run_progress_report tool and progress in run_status

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 6: Server API - progress in runs list and run detail

**Files:**
- Modify: `crates/apb-engine/src/scheduler.rs` (`RunSummary` gains `progress`; `list_runs` computes it)
- Modify: `crates/apb-server/src/lib.rs` (`get_run_handler` adds `"progress"`)
- Test: `crates/apb-engine/tests/` (extend an existing run test or add a `progress_api_test.rs`); server assertion optional via an existing handler test

**Interfaces:**
- Consumes `apb_engine::progress::{compute, ProgressSummary}` (Task 4)
- Changes `apb_engine::scheduler::RunSummary` to add `pub progress: Option<crate::progress::ProgressSummary>` (serialized as `"progress"`); `list_runs` fills it per run from that run's `playbook.yaml`
- Produces `GET /api/runs/{id}` JSON gaining a `"progress"` key holding `{ percent, label, waiting_on }` (or `null` when the snapshot is missing)
- Produces `GET /api/runs` entries gaining `"progress"` (from `RunSummary`)

Steps:

- [ ] Write failing test. Add `crates/apb-engine/tests/progress_api_test.rs`:
```rust
use apb_engine::list_runs;

#[test]
fn run_summary_includes_progress_field() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join(".apb/runs/r1");
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(
        run_dir.join("playbook.yaml"),
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: agent_task, prompt: hi, expected_duration: 100 }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: a }\n  - { from: a, to: f }\n",
    )
    .unwrap();
    std::fs::write(
        run_dir.join("events.jsonl"),
        "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n{\"seq\":1,\"ts\":0,\"type\":\"node_finished\",\"node\":\"a\",\"status\":\"succeeded\",\"attempt\":1,\"output\":\"\"}\n",
    )
    .unwrap();
    let runs = list_runs(tmp.path()).unwrap();
    let r = runs.iter().find(|r| r.run_id == "r1").unwrap();
    let p = r.progress.as_ref().expect("progress present");
    assert_eq!(p.percent, 100);
}
```
- [ ] Run: `cargo test -p apb-engine --test progress_api_test`. Expected: fails (no `progress` field).
- [ ] Add the field to `RunSummary` in `crates/apb-engine/src/scheduler.rs`:
```rust
#[derive(Debug, Serialize)]
pub struct RunSummary {
    pub run_id: String,
    pub playbook: String,
    pub status: String,
    pub started_ts: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<crate::progress::ProgressSummary>,
}
```
- [ ] Fill it in `list_runs` (inside the loop, after `state` is computed; before pushing `RunSummary`):
```rust
        let progress = std::fs::read_to_string(entry.path().join("playbook.yaml"))
            .ok()
            .and_then(|y| apb_core::schema::Playbook::from_yaml(&y).ok())
            .map(|pb| crate::progress::compute(&pb, &events));
```
and add `progress,` to the `RunSummary { .. }` struct literal.
- [ ] Run: `cargo test -p apb-engine --test progress_api_test`. Expected: passes.
- [ ] Add `"progress"` to `get_run_handler` in `crates/apb-server/src/lib.rs`. Replace the `let (playbook_json, playbook_id, version) = { ... };` block so the parsed `Playbook` is retained, then compute progress:
```rust
    // The run's playbook snapshot (may be missing for very old runs).
    let loaded_pb = std::fs::read_to_string(run_dir.join("playbook.yaml"))
        .ok()
        .and_then(|y| apb_core::schema::Playbook::from_yaml(&y).ok());
    let (playbook_json, playbook_id, version) = match &loaded_pb {
        Some(pb) => (
            serde_json::to_value(pb).unwrap_or(serde_json::Value::Null),
            pb.id.clone(),
            pb.version.clone(),
        ),
        None => (serde_json::Value::Null, id.clone(), String::new()),
    };
    let progress = loaded_pb
        .as_ref()
        .map(|pb| apb_engine::progress::compute(pb, &events));
```
Then add `"progress": progress,` to the returned `serde_json::json!({ ... })`.
- [ ] Run: `cargo build -p apb-server` and `cargo test -p apb-server` (if handler tests exist). Expected: builds; existing tests pass.
- [ ] Gates: `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] Commit:
```
git add crates/apb-engine/src/scheduler.rs crates/apb-engine/tests/progress_api_test.rs crates/apb-server/src/lib.rs
git commit -m "feat(server): expose run progress in runs list and run detail

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 7: Trial/run reports expected-vs-measured table + `playbook_howto` guidance

**Files:**
- Modify: `crates/apb-mcp/src/tools.rs` (`build_duration_table` helper; use in `run_report` and `playbook_trial`)
- Modify: `docs/HOWTO-authoring.md` (add expected_duration authoring guidance; `playbook_howto` includes this file verbatim)
- Test: `crates/apb-mcp` (`run_report` includes `duration_table`)

**Interfaces:**
- Consumes `apb_engine::progress::node_durations_seconds` (Task 4), `apb_core::schema::{Playbook, Node, NodeKind}` (`Node::expected_seconds` from Task 1)
- Produces `crate::tools::build_duration_table(run_dir: &std::path::Path, playbook: &Playbook) -> Vec<serde_json::Value>` where each entry is `{ "node": String, "kind": &str, "expected_seconds": u64, "measured_seconds": Option<u64> }`
- Produces `run_report` JSON gaining `"duration_table"`; `playbook_trial` result JSON gaining `"durations"`

Steps:

- [ ] Write failing test. Add to `crates/apb-mcp/src/tools.rs` `#[cfg(test)]`:
```rust
#[test]
fn run_report_includes_duration_table() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join(".apb/runs/r1");
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(run_dir.join("playbook.yaml"),
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: agent_task, prompt: hi, expected_duration: 100 }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: a }\n  - { from: a, to: f }\n").unwrap();
    std::fs::write(run_dir.join("events.jsonl"),
        "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n{\"seq\":1,\"ts\":1000,\"type\":\"node_started\",\"node\":\"a\",\"attempt\":1}\n{\"seq\":2,\"ts\":6000,\"type\":\"node_finished\",\"node\":\"a\",\"status\":\"succeeded\",\"attempt\":1,\"output\":\"\"}\n").unwrap();
    let out = run_report(tmp.path(), "r1").unwrap();
    let table = out.get("duration_table").and_then(|v| v.as_array()).unwrap();
    let a = table.iter().find(|e| e["node"] == "a").unwrap();
    assert_eq!(a["expected_seconds"], 100);
    assert_eq!(a["measured_seconds"], 5);
}
```
- [ ] Run: `cargo test -p apb-mcp run_report_includes_duration_table`. Expected: fails (no `duration_table`).
- [ ] Add the helper and a kind-label function to `crates/apb-mcp/src/tools.rs`:
```rust
fn node_kind_label(kind: &apb_core::schema::NodeKind) -> &'static str {
    use apb_core::schema::NodeKind::*;
    match kind {
        Start => "start",
        AgentTask { .. } => "agent_task",
        Script { .. } => "script",
        Prompt { .. } => "prompt",
        Condition { .. } => "condition",
        HumanReview { .. } => "human_review",
        Wait { .. } => "wait",
        Finish { .. } => "finish",
    }
}

/// Per-node expected vs measured durations for calibration (spec 5). Measured
/// comes from the run's events; expected from the playbook version bound to
/// the run. The maintaining agent uses this to update estimates via
/// playbook_update; the engine never rewrites the playbook.
pub(crate) fn build_duration_table(
    run_dir: &std::path::Path,
    playbook: &apb_core::schema::Playbook,
) -> Vec<Value> {
    let events = read_all(run_dir).unwrap_or_default();
    let measured = apb_engine::progress::node_durations_seconds(&events);
    playbook
        .nodes
        .iter()
        .map(|n| {
            json!({
                "node": n.id,
                "kind": node_kind_label(&n.kind),
                "expected_seconds": n.expected_seconds(),
                "measured_seconds": measured.get(&n.id),
            })
        })
        .collect()
}
```
- [ ] Extend `run_report` in `tools.rs` to attach the table on top of the status summary:
```rust
pub fn run_report(root: &Path, run_id: &str) -> Result<Value, ToolError> {
    let dir = resolve_run_dir(root, run_id)?;
    let mut base = run_status(root, run_id)?;
    let table = std::fs::read_to_string(dir.join("playbook.yaml"))
        .ok()
        .and_then(|y| apb_core::schema::Playbook::from_yaml(&y).ok())
        .map(|pb| build_duration_table(&dir, &pb))
        .unwrap_or_default();
    if let Some(obj) = base.as_object_mut() {
        obj.insert("duration_table".into(), json!(table));
    }
    Ok(base)
}
```
- [ ] Attach the table to `playbook_trial` results. In both terminal-return branches (the FsWrite worktree branch and the network-only branch), compute `let durations = build_duration_table(&run_dir, &loaded.playbook);` where `run_dir` is that branch's run directory, and add `"durations": durations,` to the returned `json!({ ... })`. (In the worktree branch `run_dir = scratch.join(".apb/runs").join(&run_id)`, already in scope; in the unisolated branch use `root.join(".apb/runs").join(&run_id)`.)
- [ ] Run: `cargo test -p apb-mcp run_report_includes_duration_table` and `cargo test -p apb-mcp`. Expected: passes; trial tests still pass.
- [ ] Add authoring guidance to `docs/HOWTO-authoring.md`. Insert a new section right after the `## Node types` section (before `## trigger`):
```markdown
## expected_duration (progress estimates)

Every node may carry an optional `expected_duration`: the estimated wall time
of ONE execution. Give it as integer seconds (`90`) or a single unit suffix
(`30s`, `5m`, `2h`). For a node inside a loop this is the per-iteration time.

When creating or editing a playbook, estimate `expected_duration` for every
`agent_task` and `script` node. A rough guess is fine; the trial and run
reports show expected vs measured durations, and you refine the numbers with
`playbook_update`. Nodes without it fall back to a 120s default, and the
validator emits a V19 warning. Waiting nodes (`human_review`, `wait`) count as
zero work, so leave their estimate at the default.
```
- [ ] Gates: `cargo fmt --all -- --check`; `cargo clippy -p apb-mcp --all-targets -- -D warnings`. Confirm no em-dash/exclamation mark in the new doc text.
- [ ] Commit:
```
git add crates/apb-mcp/src/tools.rs docs/HOWTO-authoring.md
git commit -m "feat(mcp): duration table in run/trial reports and howto guidance

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 8: Web - API types and runs list progress bar

**Files:**
- Modify: `web/src/lib/types.ts` (`ProgressSummary`, add to `RunSummary`/`RunDetail`)
- Create: `web/src/lib/progress.ts` (pure display helpers)
- Create: `web/src/lib/progress.test.ts` (vitest)
- Create: `web/src/lib/RunProgress.svelte` (presentational bar component)
- Modify: `web/src/pages/RunList.svelte` (bar for running runs)

**Interfaces:**
- Produces `ProgressSummary` TS interface `{ percent: number; label: string | null; waiting_on: string | null }`
- Adds `progress?: ProgressSummary | null` to `RunSummary` and `RunDetail`
- Produces `web/src/lib/progress.ts`: `displayPercent(prev: { shown: number; label: string | null }, next: ProgressSummary): number` (monotonic max, except an honest reset when `next.label` differs from `prev.label`); `showBar(status: string): boolean` (true only for non-terminal running states)
- Produces `RunProgress.svelte` props: `{ progress: ProgressSummary | null | undefined; status: string }`; renders a bar + percent, an optional label, and a "waiting" badge; monotonic across refetch via internal `$state`

Steps:

- [ ] Add types to `web/src/lib/types.ts`:
```ts
export interface ProgressSummary {
  percent: number
  label: string | null
  waiting_on: string | null
}
```
Add `progress?: ProgressSummary | null` to `RunSummary` (after `project`) and to `RunDetail` (after `events`).
- [ ] Write failing test. Create `web/src/lib/progress.test.ts`:
```ts
import { describe, expect, it } from 'vitest'
import { displayPercent, showBar } from './progress'

describe('displayPercent', () => {
  it('never decreases within the same plan', () => {
    const p = displayPercent({ shown: 40, label: 'a' }, { percent: 30, label: 'a', waiting_on: null })
    expect(p).toBe(40)
  })
  it('allows an honest drop when the label (plan) changes', () => {
    const p = displayPercent({ shown: 40, label: 'chapter 3 of 5' }, { percent: 21, label: 'chapter 3 of 14', waiting_on: null })
    expect(p).toBe(21)
  })
  it('rises normally', () => {
    const p = displayPercent({ shown: 40, label: null }, { percent: 55, label: null, waiting_on: null })
    expect(p).toBe(55)
  })
})

describe('showBar', () => {
  it('shows for running, hides for terminal', () => {
    expect(showBar('running')).toBe(true)
    expect(showBar('created')).toBe(true)
    expect(showBar('paused')).toBe(true)
    expect(showBar('succeeded')).toBe(false)
    expect(showBar('failed')).toBe(false)
    expect(showBar('aborted')).toBe(false)
  })
})
```
- [ ] Run: `cd web && bun run test progress`. Expected: fails (module missing).
- [ ] Create `web/src/lib/progress.ts`:
```ts
import type { ProgressSummary } from './types'

// Displayed percent is monotonic within one plan of work: it never decreases
// on refetch. The one honest exception is a plan change, signaled by the label
// changing (a RunProgress raising total, or a supervisor patch), which resets
// the baseline to the incoming value.
export function displayPercent(
  prev: { shown: number; label: string | null },
  next: ProgressSummary,
): number {
  if ((next.label ?? null) !== (prev.label ?? null)) return next.percent
  return Math.max(prev.shown, next.percent)
}

// A progress bar is shown only while a run is not terminal. Finished runs show
// status only.
export function showBar(status: string): boolean {
  const s = (status ?? '').toLowerCase()
  return !(
    s.includes('succeed') ||
    s.includes('fail') ||
    s.includes('abort') ||
    s.includes('timed') ||
    s.includes('interrupt')
  )
}
```
- [ ] Run: `cd web && bun run test progress`. Expected: passes.
- [ ] Create `web/src/lib/RunProgress.svelte` (presentational; monotonic via internal state; uses semantic tokens and `gap`, no `space-y-*`):
```svelte
<script lang="ts">
  import type { ProgressSummary } from './types'
  import { displayPercent, showBar } from './progress'
  import { Badge } from '$lib/components/ui/badge'

  let { progress, status }: { progress?: ProgressSummary | null; status: string } = $props()

  let shown = $state(0)
  let lastLabel = $state<string | null>(null)

  $effect(() => {
    if (!progress) return
    shown = displayPercent({ shown, label: lastLabel }, progress)
    lastLabel = progress.label ?? null
  })

  const visible = $derived(showBar(status) && !!progress)
  const waiting = $derived(progress?.waiting_on ?? null)
</script>

{#if visible}
  <div class="flex items-center gap-2">
    <div class="h-1.5 w-full overflow-hidden rounded-full bg-muted">
      <div
        class="h-full rounded-full bg-chart-1 transition-[width] duration-300"
        style={`width: ${waiting ? shown : shown}%`}
      ></div>
    </div>
    <span class="shrink-0 font-mono text-xs text-muted-foreground">{shown}%</span>
    {#if waiting}
      <Badge variant="secondary" class="shrink-0">waiting</Badge>
    {:else if progress?.label}
      <span class="shrink-0 text-xs text-muted-foreground">{progress.label}</span>
    {/if}
  </div>
{/if}
```
- [ ] Wire the bar into `web/src/pages/RunList.svelte`. Import the component (`import RunProgress from '$lib/RunProgress.svelte'`), add a `<Table.Head>Progress</Table.Head>` after the Status head, and a cell in each row:
```svelte
                <Table.Cell class="w-56">
                  <RunProgress progress={r.progress} status={r.status} />
                </Table.Cell>
```
- [ ] Run: `cd web && bun run check` and `bun run test`. Expected: type-checks clean; all vitest pass.
- [ ] Gates recap for web: `cd web && bun run check` and `bun run test` are clean.
- [ ] Commit:
```
git add web/src/lib/types.ts web/src/lib/progress.ts web/src/lib/progress.test.ts web/src/lib/RunProgress.svelte web/src/pages/RunList.svelte
git commit -m "feat(web): progress types, display helpers, and runs list bar

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 9: Web - RunView header bar, waiting badge, vitest coverage

**Files:**
- Modify: `web/src/pages/RunView.svelte` (header bar + percent + label; waiting badge in the sidebar)
- Modify: `web/src/lib/progress.test.ts` (add waiting/terminal display cases)

**Interfaces:**
- Consumes `RunProgress.svelte` (Task 8), `RunDetail.progress` (Task 8), `displayPercent`/`showBar` (Task 8)

Steps:

- [ ] Write failing test. Add to `web/src/lib/progress.test.ts`:
```ts
describe('waiting and terminal display', () => {
  it('keeps the shown value when a report lacks progress movement', () => {
    // waiting nodes report the same percent; monotonic keeps it steady
    const p = displayPercent({ shown: 60, label: null }, { percent: 60, label: null, waiting_on: 'review' })
    expect(p).toBe(60)
  })
  it('hides the bar on a succeeded run regardless of percent', () => {
    expect(showBar('succeeded')).toBe(false)
  })
})
```
- [ ] Run: `cd web && bun run test progress`. Expected: passes immediately if helpers already cover it, or drives any tweak. (These assert the Task 8 helpers; they must stay green.)
- [ ] Add the header bar to `web/src/pages/RunView.svelte`. Import the component: `import RunProgress from '$lib/RunProgress.svelte'`. Below the `<Topbar>` block and above the `<div class="flex min-h-0 flex-1">` main row, add a slim progress strip:
```svelte
{#if detail}
  <div class="border-b border-border px-4 py-2">
    <RunProgress progress={detail.progress} status={detail.run_status} />
  </div>
{/if}
```
- [ ] Confirm the sidebar "Waiting" card already covers human_review/wait via `pendingWaits`; the bar's own `waiting` badge (from `waiting_on`) is the header-level indicator that the bar is frozen. No further change needed there. The frozen/terminal coloring is handled by `showBar` hiding the bar on terminal and the bar staying at its last `shown` value while `waiting_on` is set.
- [ ] Run: `cd web && bun run check` and `bun run test`. Expected: clean, all pass.
- [ ] Manual smoke (optional, if a dev server is available): `cd web && bun run build` succeeds so the embedded frontend builds for release.
- [ ] Gates: `cd web && bun run check`; `bun run test`.
- [ ] Commit:
```
git add web/src/pages/RunView.svelte web/src/lib/progress.test.ts
git commit -m "feat(web): run view header progress bar and waiting badge

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Final verification (run once after Task 9, before release)

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `cd web && bun run check && bun run test && bun run build`
- [ ] `cargo metadata --format-version 1 >/dev/null` then `code-ranker check .` (must exit 0; for any violation read `code-ranker docs base <ID>`, fix, re-run)
- [ ] `cargo clippy --release` clean before the work is considered release-ready
- [ ] Do not commit the release-verification step separately unless something changed; do not push or publish without the owner's explicit approval.

## Notes for implementers (cross-task invariants)
- The MCP `run_progress_report` tool intentionally carries no `node_id`; drive stamps it from the node in hand (`current` at the top-of-loop drain, `current_node` inside `await_control`). Any member of a cycle group maps to the same SCC, so binding by the drive-side current node is sufficient.
- `EventPayload::RunProgress` and `Control::Progress` are additive; old logs and old control files deserialize unchanged because every new event field is `#[serde(default)]`.
- Progress is a pure fold: never store a running percent. Resume, server restart, and a second reader all recompute the identical number from `events.jsonl` + the run's `playbook.yaml`.
- Monotonic display lives only in the web layer (`displayPercent`); the engine returns the honest instantaneous percent, including honest drops when a report raises `total` or a patch changes the plan.
- JSON key is `"progress"` everywhere (runs list entries, run detail, MCP `run_status`); its shape is exactly `{ percent, label, waiting_on }`.