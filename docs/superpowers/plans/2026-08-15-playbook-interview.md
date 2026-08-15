# Playbook Interview Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a person create an apb playbook by describing the desired process in a guided interview: a first-class `goal` field with verifiable criteria on the playbook schema, an agent-facing interview guide, and a `playbook_interview` MCP tool that delivers it.

**Architecture:** Three independent pieces in existing crates, no new crates. apb-core gains an optional `goal` field on `Playbook` (statement plus criteria, each with a check kind) and validator rule V41. The repository gains `docs/HOWTO-interview.md`, the agent-facing interview instructions, embedded into apb-mcp by a new read-only tool `playbook_interview` exactly the way `playbook_howto` embeds `docs/HOWTO-authoring.md`. The MCP server instructions (TIER0) gain one sentence telling agents when to offer the interview; playbook assembly itself uses only existing tools.

**Tech Stack:** Rust workspace (edition 2024, workspace-inherited), `serde` / `serde_yaml_ng`, rmcp `#[tool_router]` / `#[tool]` macros in apb-mcp.

**Spec:** `docs/superpowers/specs/2026-08-15-playbook-interview-design.md`

## Global Constraints

- Repository docs and all machine-facing strings are English. No em-dashes (U+2014), no exclamation marks, no CJK anywhere in code or prose.
- New serde fields are additive: `#[serde(default)]` so older files keep parsing. The new `goal` field additionally uses `skip_serializing_if = "Option::is_none"` so rewritten playbooks of the goal-less majority do not grow a `goal: null` line.
- The validator code for the new rule is V41. Highest existing code is V40 (`crates/apb-core/src/validate/graph.rs:236`). Before implementing, confirm V41 is still free: `grep -rn '"V41"' crates/apb-core/src/` must return nothing.
- `TIER0` in `crates/apb-mcp/src/instructions.rs` has a hard budget of 1950 bytes (`TIER0_MAX_BYTES`), pinned by the test `tier0_fits_the_host_budget`. The replacement text in Task 5 is pre-measured at 1943 bytes; transcribe it exactly.
- Before each commit: `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` must be clean, and code-ranker must pass (`cargo metadata --format-version 1 >/dev/null` first to warm the cache, then `code-ranker check .`).
- Every commit uses `git commit --signoff` (DCO gate) and ends the message with the acting model's Co-Authored-By trailer.
- Do not push, publish, or open a PR; everything stays local until the owner approves.

---

### Task 1: Goal types on the playbook schema

**Files:**
- Modify: `crates/apb-core/src/schema.rs` (Playbook struct is at lines 14-47; add types near the other auxiliary types such as `Requires`; tests go into the existing `#[cfg(test)]` tests module in the same file, which already has round-trip tests around line 1209)

**Interfaces:**
- Consumes: nothing new; `Playbook::from_yaml` already exists.
- Produces: `pub struct Goal { pub statement: String, pub criteria: Vec<GoalCriterion> }`, `pub struct GoalCriterion { pub description: String, pub check: GoalCheck }`, `pub enum GoalCheck { Manual, Marker { marker: String }, Script { path: String } }`, and `pub goal: Option<Goal>` on `Playbook`. Task 2's validator and Task 4's docs rely on these exact names.

- [ ] **Step 1: Write the failing round-trip tests**

Add to the existing `#[cfg(test)]` tests module in `crates/apb-core/src/schema.rs` (next to the other round-trip tests):

```rust
#[test]
fn goal_fields_round_trip() {
    let yaml = r#"
schema: 2
id: demo
name: Demo
version: 1.0.0
goal:
  statement: the invoice is recorded and sent for approval
  criteria:
    - description: a row with the invoice amount appears in the sheet
      check: { kind: marker, marker: INVOICE_ROW_ADDED }
    - description: the email is in Sent
    - description: a script confirms the ledger balance
      check: { kind: script, path: checks/ledger.sh }
nodes: []
edges: []
"#;
    let p = Playbook::from_yaml(yaml).unwrap();
    let goal = p.goal.clone().unwrap();
    assert_eq!(goal.statement, "the invoice is recorded and sent for approval");
    assert_eq!(goal.criteria.len(), 3);
    assert_eq!(
        goal.criteria[0].check,
        GoalCheck::Marker { marker: "INVOICE_ROW_ADDED".into() }
    );
    assert_eq!(goal.criteria[1].check, GoalCheck::Manual);
    assert_eq!(
        goal.criteria[2].check,
        GoalCheck::Script { path: "checks/ledger.sh".into() }
    );

    let back = serde_yaml_ng::to_string(&p).unwrap();
    let again = Playbook::from_yaml(&back).unwrap();
    assert_eq!(again.goal, p.goal);
}

#[test]
fn playbook_without_goal_serializes_without_goal_key() {
    let yaml = "schema: 2\nid: demo\nname: Demo\nversion: 1.0.0\nnodes: []\nedges: []\n";
    let p = Playbook::from_yaml(yaml).unwrap();
    assert!(p.goal.is_none());
    let back = serde_yaml_ng::to_string(&p).unwrap();
    assert!(!back.contains("goal"), "goal-less playbook grew a goal key: {back}");
}
```

If the tests module does not already import the new names, extend its `use` line (the module uses `use super::*;` in this file; verify and keep whatever pattern is there).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p apb-core --lib goal_fields_round_trip`
Expected: COMPILE ERROR, `Goal`/`GoalCheck` not found and no `goal` field on `Playbook`.

- [ ] **Step 3: Implement the types and the field**

In `crates/apb-core/src/schema.rs`, near the other auxiliary playbook types (for example right after the `Requires` definition), add:

```rust
/// How a goal criterion is verified after a run (spec 2026-08-15).
/// `Script` execution is not wired into run verdicts yet; the variant
/// records the contract for later engine work.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GoalCheck {
    /// A person confirms the criterion by hand.
    #[default]
    Manual,
    /// The marker string is expected in the run result.
    Marker { marker: String },
    /// A check script confirms the criterion.
    Script { path: String },
}

/// One verifiable fact confirming the goal was reached.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct GoalCriterion {
    pub description: String,
    #[serde(default)]
    pub check: GoalCheck,
}

/// The playbook's goal in the owner's words plus verifiable criteria.
/// Parsing is permissive; completeness is enforced by validator rule V41.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Goal {
    pub statement: String,
    #[serde(default)]
    pub criteria: Vec<GoalCriterion>,
}
```

On the `Playbook` struct, insert after the `requires` field and before `effects`:

```rust
    /// The goal this playbook exists to reach, with verifiable criteria
    /// (spec 2026-08-15). Agents and supervisors may adapt the process but
    /// must never change the criteria; only a person may.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<Goal>,
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p apb-core --lib goal_`
Expected: both new tests PASS.

- [ ] **Step 5: Run the crate suite to catch regressions**

Run: `cargo test -p apb-core`
Expected: PASS (the field is additive; nothing else constructs `Playbook` literals outside `schema.rs` tests).

- [ ] **Step 6: Gates and commit**

Run: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo metadata --format-version 1 >/dev/null && code-ranker check .`

```bash
git add crates/apb-core/src/schema.rs
git commit --signoff -m "feat(core): goal field with verifiable criteria on the playbook schema"
```

---

### Task 2: Validator rule V41 and authoring docs for goal

**Files:**
- Modify: `crates/apb-core/src/validate/nodes.rs` (add `check_goal` after `check_trigger`, which ends around line 44)
- Modify: `crates/apb-core/src/validate/mod.rs` (import at lines 29-32, call site around line 140)
- Create: `crates/apb-core/tests/suite/validate_goal_test.rs`
- Modify: `crates/apb-core/tests/main.rs` (add the module line)
- Modify: `docs/HOWTO-authoring.md` (top-level field list around line 18; new section after `## effects`, line 885, before `## Secrets`, line 893)

**Interfaces:**
- Consumes: `Playbook.goal: Option<Goal>` from Task 1; `ValidationReport::error(code, node, msg)` and the `validate()` entry point in `validate/mod.rs`.
- Produces: `pub(crate) fn check_goal(playbook: &Playbook, r: &mut ValidationReport)` emitting code `"V41"` with severity Error. Task 3's guide and Task 4's docs reference rule V41 by that code.

- [ ] **Step 1: Write the failing validator tests**

Create `crates/apb-core/tests/suite/validate_goal_test.rs`:

```rust
use apb_core::schema::Playbook;
use apb_core::validate::{Severity, ValidationContext, validate};

const VALID: &str = include_str!("../fixtures/valid.yaml");

fn ctx() -> ValidationContext {
    ValidationContext {
        profiles: vec!["architect".into(), "fullstack".into()],
        ..Default::default()
    }
}

fn error_codes(yaml: &str) -> Vec<&'static str> {
    let playbook = Playbook::from_yaml(yaml).unwrap();
    validate(&playbook, &ctx())
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .map(|i| i.code)
        .collect()
}

fn with_goal(goal_yaml: &str) -> String {
    format!("{goal_yaml}\n{VALID}")
}

#[test]
fn complete_goal_passes() {
    let yaml = with_goal(
        "goal:\n  statement: the invoice is recorded and sent\n  criteria:\n    - description: a row appears in the sheet\n",
    );
    assert!(!error_codes(&yaml).contains(&"V41"));
}

#[test]
fn v41_empty_statement() {
    let yaml = with_goal(
        "goal:\n  statement: \"  \"\n  criteria:\n    - description: a row appears\n",
    );
    assert!(error_codes(&yaml).contains(&"V41"));
}

#[test]
fn v41_no_criteria() {
    let yaml = with_goal("goal:\n  statement: the invoice is recorded\n  criteria: []\n");
    assert!(error_codes(&yaml).contains(&"V41"));
}

#[test]
fn v41_empty_criterion_description() {
    let yaml = with_goal(
        "goal:\n  statement: the invoice is recorded\n  criteria:\n    - description: \"\"\n",
    );
    assert!(error_codes(&yaml).contains(&"V41"));
}

#[test]
fn playbook_without_goal_has_no_v41() {
    assert!(!error_codes(VALID).contains(&"V41"));
}
```

Wire it into the single test binary. In `crates/apb-core/tests/main.rs`, next to the other validator modules, add:

```rust
#[path = "suite/validate_goal_test.rs"]
mod validate_goal_test;
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p apb-core --test main v41`
Expected: FAIL, the three `v41_*` tests find no `"V41"` code (the rule does not exist yet). `complete_goal_passes` and `playbook_without_goal_has_no_v41` pass vacuously.

- [ ] **Step 3: Implement the rule**

In `crates/apb-core/src/validate/nodes.rs`, after `check_trigger`, add:

```rust
/// V41: a goal, when present, must be complete: a non-empty statement, at
/// least one criterion, and a description on every criterion. An empty goal
/// is worse than none, because agents and supervisors treat the goal as the
/// contract of the run.
pub(crate) fn check_goal(playbook: &Playbook, r: &mut ValidationReport) {
    let Some(g) = &playbook.goal else { return };
    if g.statement.trim().is_empty() {
        r.error("V41", None, "goal.statement is empty".to_string());
    }
    if g.criteria.is_empty() {
        r.error(
            "V41",
            None,
            "goal.criteria is empty, at least one criterion is required".to_string(),
        );
    }
    for (i, c) in g.criteria.iter().enumerate() {
        if c.description.trim().is_empty() {
            r.error("V41", None, format!("goal.criteria[{i}].description is empty"));
        }
    }
}
```

In `crates/apb-core/src/validate/mod.rs`: add `check_goal` to the existing `use nodes::{...}` import list (lines 29-32), and in `validate()` call it immediately after the `check_trigger(playbook, &mut r); // V17` line (around line 140):

```rust
    check_goal(playbook, &mut r); // V41
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p apb-core --test main validate_goal`
Expected: all five tests PASS.

- [ ] **Step 5: Document the field in the authoring guide**

In `docs/HOWTO-authoring.md`:

1. In the top-level field list (around line 18), change the line
   `- trigger, requires, effects (see below)` to
   `- trigger, requires, effects, goal (see below)`.
2. After the `## effects` section (line 885) and before `## Secrets` (line 893), insert:

```markdown
## goal (target and criteria)

Optional. The goal this playbook exists to reach, in the owner's words, plus
verifiable criteria. When present, the validator (V41) requires a non-empty
statement, at least one criterion, and a description on every criterion.

- `statement` (string): the goal in plain words, e.g. "the invoice is
  recorded in the tracking sheet and sent for approval".
- `criteria` (list): each `{ description, check? }`.
  - `check: { kind: manual }` (default when omitted): a person confirms the
    criterion.
  - `check: { kind: marker, marker: <string> }`: the marker string is
    expected in the run result.
  - `check: { kind: script, path: <relative path> }`: a check script
    confirms the criterion. Script execution is not wired into run verdicts
    yet; the field records the contract.

The goal is the contract of the run: agents and supervisors may adapt the
process, but must never weaken or rewrite the criteria; only a person may
change them.
```

- [ ] **Step 6: Run the full crate suite**

Run: `cargo test -p apb-core`
Expected: PASS.

- [ ] **Step 7: Gates and commit**

Run: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo metadata --format-version 1 >/dev/null && code-ranker check .`

```bash
git add crates/apb-core/src/validate/nodes.rs crates/apb-core/src/validate/mod.rs \
  crates/apb-core/tests/suite/validate_goal_test.rs crates/apb-core/tests/main.rs \
  docs/HOWTO-authoring.md
git commit --signoff -m "feat(core): validator rule V41 for goal completeness"
```

---

### Task 3: The interview guide document

**Files:**
- Create: `docs/HOWTO-interview.md`

**Interfaces:**
- Consumes: the `goal` field semantics from Tasks 1-2 (referenced in prose).
- Produces: the file `docs/HOWTO-interview.md` whose exact path Task 4 embeds via `include_str!`, containing the heading `# Playbook interview (tier 2)` and a section titled `### 4. Goal and criteria (mandatory)` that Task 4's content test asserts on.

- [ ] **Step 1: Write the guide**

Create `docs/HOWTO-interview.md` with exactly this content:

```markdown
# Playbook interview (tier 2)

This is the on-demand guide an agent pulls via `playbook_interview` when the
user wants to automate a process by describing it in conversation. It is the
counterpart of `HOWTO-authoring.md`: that document covers the playbook YAML
format, this one covers the conversation with a person. Pull
`playbook_howto` as well before assembling the result.

## When to run an interview

Offer an interview when the user describes a recurring process they want
automated and no existing playbook covers it (check the catalog first; if a
match exists, offer the existing playbook instead of creating a duplicate).
The interview replaces nothing the user already did: if they just performed
the action by hand, `playbook_capture` is the right path, not an interview.

## Ground rules

- The conversation happens in the user's chat language. This document is
  English; your questions and summaries are not.
- Questions are for an ordinary employee: plain language, about the process
  itself. Never ask the person about nodes, profiles, models, or connectors.
  You make those technical decisions yourself from what is already configured
  in the project, and surface them only inside the final playback. When the
  person answers in technical terms, use their precision; there is no
  separate mode.
- One question per message. If a topic needs more, split it into several
  messages.
- Never invent a step the person did not name. A gap is a question, not a
  guess. Anything unclear is asked again, not filled in.
- Every point where the person chose among alternatives by feel is a choice
  with an unknown rule: clarify it, or record it as an explicit
  ask-a-person gate.

## Question flow

Work through these blocks in order.

### 1. The task and its trigger

What task to automate, how often it occurs, what starts it. Example opening:
"Tell me about the task you want to automate. How often does it come up, and
what usually kicks it off?"

### 2. Data sources

Which emails, files, sheets, pages are involved; what the person opens and
reads; where results are written.

### 3. Steps as a story

"Walk me through it as if teaching a new colleague." Listen to the whole
story, then probe the gaps. Do not interrupt with structure.

### 4. Goal and criteria (mandatory)

"How do you yourself know you did this right?" The answers become the
playbook's `goal` field: a statement in the owner's words plus verifiable
criteria (a row appears in the sheet, the email is in Sent). Each criterion
gets a check kind: `manual` when only a person can confirm it, `marker` when
the run result can carry a marker string, `script` when a check script could
confirm it. An interview without this block is not finished and must not
produce a draft.

### 5. Human gates

What the person would never trust to run without confirmation: sending,
paying, deleting, anything irreversible. These become `human_review` gates in
the playbook.

### 6. Variables vs constants

What changes run to run (an invoice number) versus what is fixed (the
recipient). Variables become playbook params.

### 7. Exceptions

"Does it ever go differently? What do you do then?" Each answer becomes a
branch, or an honest ask-a-person gate when the rule is unclear.

## Playback and confirmation

Before building anything, play the understood process back in plain language:
the steps, the goal and its criteria, the points where the playbook will ask
the person. Ask for an explicit yes. If the person corrects anything, update
and play back again. Only after the yes do you assemble the playbook.

## Assembling the draft

1. Reuse profiles: call `profile_list` and pick fitting ones. Create a new
   profile through the existing profile flow only if none fits.
2. Create the draft with `playbook_create`. Machine fields (ids, trigger,
   effects) are English; display names and descriptions follow the user's
   language. Include the `goal` field from block 4.
3. Validate. If validation fails, fix the playbook yourself and revalidate;
   validator errors are never the person's problem.
4. Offer a trial through the normal trust path. The draft is never run
   without the person going through the standard trial and approval
   mechanics.

## Edge cases

- **Interrupted interview.** The interview can stop and resume later; on
  resume, replay what was already established and continue from the first
  unanswered block.
- **Too vague to automate.** If after blocks 3 and 7 the process has no
  stable shape, say so honestly and offer to narrow the scope to the stable
  core, rather than produce a playbook of guesses.
- **Already covered.** If at any point the described process turns out to be
  covered by an existing playbook, say so and offer it.
```

- [ ] **Step 2: Verify prose conventions**

Run: `grep -n $'—' docs/HOWTO-interview.md; grep -cn '!' docs/HOWTO-interview.md`
Expected: no em-dashes; `!` count 0.

- [ ] **Step 3: Commit**

```bash
git add docs/HOWTO-interview.md
git commit --signoff -m "docs: playbook interview guide (tier 2)"
```

---

### Task 4: The playbook_interview MCP tool

**Files:**
- Modify: `crates/apb-mcp/src/tools/playbook.rs` (add next to `playbook_howto`, lines 82-86)
- Modify: `crates/apb-mcp/src/server/playbook.rs` (add next to the `playbook_howto` tool method, lines 64-70)
- Modify: `crates/apb-mcp/src/server/tests.rs` (the exhaustive registration test at lines 96-164, plus one new content test)

**Interfaces:**
- Consumes: `docs/HOWTO-interview.md` from Task 3 (embedded via `include_str!`); the existing `to_call_tool_result` helper and `#[tool_router(router = playbook_router)]` impl block.
- Produces: MCP tool named `playbook_interview`, tool-layer function `pub fn playbook_interview() -> Result<Value, ToolError>` returning `{ "guide": <markdown> }`. Task 5's instructions text names this tool.

- [ ] **Step 1: Write the failing tests**

In `crates/apb-mcp/src/server/tests.rs`:

1. In `tool_router_registers_all_read_run_write_and_supervisor_tools`, add `"playbook_interview",` to the `expected` array right after `"playbook_howto",`. The trailing `assert_eq!(names.len(), expected.len(), ...)` makes this test fail until the tool is registered.
2. Add a content test (match the file's existing import style; the tools module is reachable as `crate::tools`):

```rust
#[test]
fn playbook_interview_returns_the_embedded_guide() {
    let value = crate::tools::playbook_interview().unwrap();
    let guide = value["guide"].as_str().unwrap();
    assert!(guide.contains("# Playbook interview"));
    assert!(guide.contains("### 4. Goal and criteria (mandatory)"));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p apb-mcp --lib tool_router_registers && cargo test -p apb-mcp --lib playbook_interview_returns`
Expected: the first FAILS on the length assertion (expected list now longer than registered tools); the second FAILS to compile (`playbook_interview` not found). A compile error in the test target fails both invocations; that is fine, it is the red state.

- [ ] **Step 3: Implement the tool function and register it**

In `crates/apb-mcp/src/tools/playbook.rs`, after `playbook_howto`:

```rust
/// Tier 2 (spec 2026-08-15): the guided-interview guide. Pulled only when
/// the user wants a playbook built from a process they describe.
pub fn playbook_interview() -> Result<Value, ToolError> {
    Ok(json!({ "guide": include_str!("../../../../docs/HOWTO-interview.md") }))
}
```

In `crates/apb-mcp/src/server/playbook.rs`, after the `playbook_howto` tool method, inside the same `#[tool_router(router = playbook_router, vis = "pub(crate)")] impl WfMcp` block:

```rust
    #[tool(
        description = "Interview guide (tier 2): how to build a playbook by interviewing the user about a process they describe. Pull when the user wants to automate a process no playbook covers.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn playbook_interview(&self) -> CallToolResult {
        to_call_tool_result(tools::playbook_interview())
    }
```

No other wiring: the `#[tool]` macro auto-registers the method into `playbook_router()`, which `tool_router()` in `server/mod.rs` already merges.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p apb-mcp --lib`
Expected: PASS, including both edited/new tests.

- [ ] **Step 5: Gates and commit**

Run: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo metadata --format-version 1 >/dev/null && code-ranker check .`

```bash
git add crates/apb-mcp/src/tools/playbook.rs crates/apb-mcp/src/server/playbook.rs \
  crates/apb-mcp/src/server/tests.rs
git commit --signoff -m "feat(mcp): playbook_interview tool serving the interview guide"
```

---

### Task 5: Server instructions mention the interview

**Files:**
- Modify: `crates/apb-mcp/src/instructions.rs` (the `TIER0` constant at lines 10-25 and the `tier0_keeps_the_load_bearing_rules` phrase list in the tests module)

**Interfaces:**
- Consumes: the `playbook_interview` tool name from Task 4.
- Produces: updated TIER0 text; nothing downstream.

- [ ] **Step 1: Extend the failing phrase test**

In `tier0_keeps_the_load_bearing_rules`, add two entries to the phrase array:

```rust
            "playbook_interview",
            "offer a short interview",
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p apb-mcp --lib tier0_keeps`
Expected: FAIL, TIER0 lacks the new phrases.

- [ ] **Step 3: Replace TIER0 with the pre-measured text**

The current TIER0 is 1948 bytes of the 1950 cap, so the interview sentence requires trimming elsewhere. The replacement below is pre-measured at 1943 bytes and keeps every phrase the tests pin. Replace the entire `TIER0` string so its paragraphs (separated by blank lines, in the existing escaped-string style with `\n\n` between paragraphs) read exactly:

```text
Discovery: call playbook_catalog once per task that names a doable action, before acting. Skip chit-chat. It returns trigger, effects, trust and suppressed_suggestions.

Offering to save: if you just completed a multi-step repeatable action, or the user asks for one recurring by nature, and no playbook matched, you MUST offer once to save it with playbook_capture: one short question offering project or global scope, project first if project-specific. First compare it with suppressed_suggestions by synopsis meaning, not slug (empty synopsis: by slug); a covering record means no offer. One offer per session.

Declines: when the user declines without saying never, call suggestion_dismiss with kind soft, project scope and a one-sentence synopsis. Reserve kind hard for an explicit never-again, global scope for everywhere-wording. Never ask about scope.

Interview: when the user describes a process to automate, offer a short interview and pull playbook_interview.

Using a match: on a confident match to an active, trusted playbook, name it in one line and run it. One short question if ambiguous; confirm first for another project.

Running policy: the server refuses drafts and untrusted playbooks until trial or acknowledgement. Effects beyond the request (network, secrets, deploys, irreversible) need confirmation.

Human gates: run_status, supervisor_wait_event and supervisor_run_inspect return pending_review at a human_review gate. Relay its instruction in the user's language with the options, then record it with review_decide. Frozen until then; repeat while pending.

Profiles: a node binds its executor only through a profile (agent, model, fallbacks, role prompt, skills). Call profile_list to reuse one, profile_howto for format.

Lifecycle: update, clone, version and delete playbooks; pull playbook_howto when authoring. Call projects_list for another workspace. Machine fields are English; speak the user's language.
```

Diffs from the current text, so the reviewer can see nothing load-bearing was lost: paragraph 1 drops `, scope` from the return-fields list; paragraph 2 shortens `recommended first (project if project-specific)` to `project first if project-specific` and `compare the action with` to `compare it with`; paragraph 3 drops `; the server escalates the silence`; the Interview paragraph is new; Using a match drops `here or global`; Human gates drops `The moment you see it you MUST relay` in favor of `Relay`; Lifecycle drops `you may `.

- [ ] **Step 4: Run the instructions tests to verify they pass**

Run: `cargo test -p apb-mcp --lib tier0`
Expected: PASS, including `tier0_fits_the_host_budget` (1943 <= 1950), `tier0_keeps_the_load_bearing_rules`, and `tier0_follows_the_prose_conventions`.

- [ ] **Step 5: Run the full workspace suite**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Gates and commit**

Run: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo metadata --format-version 1 >/dev/null && code-ranker check .`

```bash
git add crates/apb-mcp/src/instructions.rs
git commit --signoff -m "feat(mcp): server instructions offer the playbook interview"
```
