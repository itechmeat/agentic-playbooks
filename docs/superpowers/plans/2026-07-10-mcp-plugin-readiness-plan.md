# Plan 0: MCP Plugin-Readiness (near-term)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring the WF MCP surface to a state suitable for embedding as a plugin/connector (ChatGPT Apps, Claude connectors, Claude Desktop, opencode/Hermes/Pi), by delivering what is useful right now to every tier: a non-blocking async run start, tool safety annotations, and surface curation. Based on the design doc `docs/superpowers/specs/2026-07-10-remote-access-design.md`, section 13.5.

**Architecture:** The existing MCP surface (`crates/wf-mcp`, rmcp 2.2.0) is already nearly async: `run_background`, `run_status`, `run_events`, `review_decide` are implemented and exposed. The only thing left blocking is `workflow_run`. We add it an optional non-blocking mode (backward-compatible), attach safety annotations to all tools, and curate the descriptions. No relay and no networking - only improving the local MCP server.

**Tech Stack:** Rust (edition 2024), rmcp 2.2.0, serde, schemars (JsonSchema), tokio.

## Global Constraints

- Do not use em dashes anywhere: code, comments, documentation. Only a hyphen, comma, or a restructured sentence.
- Do not use exclamation marks in documentation or messages.
- Comments in English (matching the style of the crate's existing files).
- **Backward compatibility:** the existing behavior of `workflow_run` (blocking run that returns the outcome) does NOT change by default. Async is strictly opt-in via a new field.
- Supervisor tools stay behind the session gate (`resolve_session`); their surface is untouched.
- Every task ends with a green `cargo test -p wf-mcp` and `cargo clippy -p wf-mcp --all-targets` with no new warnings.
- Env variables (`WF_AGENT_CMD` etc.) in tests: one test file = one process; tests sharing such a variable should live in the same file, so the env is not raced across test threads of the same binary.

---

## File Structure

- `crates/wf-mcp/src/server.rs` (modify) - `background` field on `WorkflowRunArgs`; branching in the `workflow_run` tool; annotations on all `#[tool]` items (mechanism decided in Task 2).
- `crates/wf-mcp/src/tools.rs` (modify) - function `workflow_run_background`.
- `crates/wf-mcp/tests/async_run_test.rs` (create) - the non-blocking start returns a run_id, the run reaches a terminal status via polling.
- `crates/wf-mcp/tests/tool_annotations_test.rs` (create) - `tools/list` carries annotations (read-only / destructive) on the key tools.
- `docs/MCP.md` (create) - "WF as an MCP server": the curated set of tools, the async model, annotations, connection snippets (Claude Desktop, opencode).

---

## Task 1: Non-blocking async start of a run

**Files:**
- Modify: `crates/wf-mcp/src/tools.rs`, `crates/wf-mcp/src/server.rs`
- Test: `crates/wf-mcp/tests/async_run_test.rs`

**Interfaces:**
- Consumes: `wf_engine::run_background` (already used in `workflow_run_supervised`), `RunOptions`, `RunMode::Autonomous`.
- Produces:
  - `tools::workflow_run_background(root, id, version, params, instruction) -> Result<Value, ToolError>` returns `{ "run_id": ... }` immediately.
  - Field `background: Option<bool>` on `WorkflowRunArgs`.
  - Branching in the `workflow_run` tool: `supervise == "self"` -> supervisor path (unchanged); otherwise `background == Some(true)` -> `workflow_run_background`; otherwise blocking `workflow_run` (unchanged).

- [ ] **Step 1: Write a failing test**

Create `crates/wf-mcp/tests/async_run_test.rs`. For seeding a minimal workflow and a mock agent, rely on the pattern from the existing `wf-mcp` tests (look at how other tests in `server.rs`/`tests/` set up a project and set `WF_AGENT_CMD`). Skeleton:

```rust
use std::time::{Duration, Instant};
use wf_mcp::tools;

/// The only test in this file (separate process) - isolates WF_AGENT_CMD.
#[test]
fn background_run_returns_run_id_without_blocking() {
    let dir = tempfile::tempdir().unwrap();
    // 1) init + a minimal workflow (start -> agent_task -> finish),
    //    as in the existing wf-mcp tests.
    // 2) mock agent: WF_AGENT_CMD pointing to a trivial script that exits successfully.
    //    (use exactly the trick already used in the run tests.)
    seed_minimal_project_and_agent(dir.path()); // replace with a real helper/inline code

    let started = Instant::now();
    let v = tools::workflow_run_background(
        dir.path(), "demo", None, Default::default(), None,
    ).unwrap();
    // Start is non-blocking: returned quickly (seconds, not minutes).
    assert!(started.elapsed() < Duration::from_secs(5));
    let run_id = v["run_id"].as_str().expect("run_id").to_string();

    // Poll status until terminal (reuse the existing run_status).
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let s = tools::run_status(dir.path(), &run_id).unwrap();
        let st = s["run_status"].as_str().unwrap_or("");
        if st == "succeeded" || st == "failed" { break; }
        assert!(Instant::now() < deadline, "run did not finish: {st}");
        std::thread::sleep(Duration::from_millis(50));
    }
}
```

Note for the implementer: if there is no convenient seeding helper, write the project creation and mock agent inline, copying the trick from the closest existing run test in `wf-mcp` (do not invent a new approach).

- [ ] **Step 2: Run - it fails**

Run: `cargo test -p wf-mcp --test async_run_test`
Expected: FAIL - `tools::workflow_run_background` does not exist.

- [ ] **Step 3: Implement the tool function**

In `crates/wf-mcp/src/tools.rs` next to `workflow_run`:

```rust
/// Non-blocking start of a run for a regular (non-supervisor) MCP client:
/// launches the workflow in the background (autonomous) and immediately
/// returns the run_id. The client then polls `run_status`/`run_events` and
/// decides reviews via `review_decide`.
/// Needed because some hosts (e.g. ChatGPT Apps) have a tool-call timeout of
/// about 60s, while a run can take minutes (spec 13.5).
pub fn workflow_run_background(
    root: &Path,
    id: &str,
    version: Option<&str>,
    params: BTreeMap<String, String>,
    instruction: Option<String>,
) -> Result<Value, ToolError> {
    let opts = RunOptions {
        instruction,
        params,
        allow_shared_workdir: false,
        mode: RunMode::Autonomous,
        supervisor_expected: false,
        max_patches_per_run: None,
        context_max_bytes: None,
        context_compact_model: None,
        overrides: None,
    };
    let run_id = run_background(root, id, version, opts)?;
    Ok(json!({ "run_id": run_id }))
}
```

Check the signature of `run_background` (already imported in `tools.rs`): it returns a run_id for `mode: Supervised` in `workflow_run_supervised`; confirm that the autonomous path is supported (if `run_background` requires a special mode, use the same mode but without spawning a supervisor agent; escalate on any mismatch).

- [ ] **Step 4: Wire into the `workflow_run` tool**

In `crates/wf-mcp/src/server.rs` add a field to `WorkflowRunArgs`:

```rust
    /// background: true - start the run in the background and immediately
    /// return the run_id (without waiting for completion). For clients with
    /// a short tool-call timeout.
    #[serde(default)]
    pub background: Option<bool>,
```

Update the destructuring and body of the `workflow_run` tool:

```rust
    async fn workflow_run(
        &self,
        Parameters(WorkflowRunArgs { id, version, params, instruction, supervise, background }): Parameters<
            WorkflowRunArgs,
        >,
    ) -> CallToolResult {
        if supervise.as_deref() == Some("self") {
            return self.run_supervised_self(id, version, params, instruction);
        }
        if background == Some(true) {
            return to_call_tool_result(tools::workflow_run_background(
                &self.root, &id, version.as_deref(), params, instruction,
            ));
        }
        to_call_tool_result(tools::workflow_run(
            &self.root, &id, version.as_deref(), params, instruction,
        ))
    }
```

- [ ] **Step 5: Run - green**

Run: `cargo test -p wf-mcp --test async_run_test`
Expected: PASS.

- [ ] **Step 6: Clippy and commit**

Run: `cargo clippy -p wf-mcp --all-targets`

```bash
git add crates/wf-mcp/src/tools.rs crates/wf-mcp/src/server.rs crates/wf-mcp/tests/async_run_test.rs
git commit -m "feat(mcp): non-blocking background workflow_run (run_id + poll)"
```

---

## Task 2: Tool safety annotations

**Files:**
- Modify: `crates/wf-mcp/src/server.rs`
- Test: `crates/wf-mcp/tests/tool_annotations_test.rs`

**Interfaces:**
- Produces: every public tool has annotation hints set. Classification:
  - **read-only** (`readOnlyHint = true`): `workflow_list`, `workflow_get`, `workflow_validate`, `runs_list`, `run_status`, `run_events`, `run_report`.
  - **destructive** (`destructiveHint = true`): `workflow_run` (spawns agents, changes project files), `workflow_create`, `workflow_update`, `workflow_delete`, `run_resume`, `review_decide`.
  - Supervisor tools: mark according to their meaning (`supervisor_run_inspect`/`supervisor_wait_event` - read-only; mutating `supervisor_*` - destructive), without changing their session gate.

- [ ] **Step 1: Determine the annotation mechanism in rmcp 2.2.0**

Read how rmcp 2.2.0 expresses annotations:

Run: `find ~/.cargo -type d -name 'rmcp-2.2.0' 2>/dev/null` then `grep -rn "ToolAnnotations\|read_only_hint\|destructive_hint\|annotations" <path>/src | head -40`

Determine one of two options:
- (a) the `#[tool]` macro accepts annotations declaratively (e.g. `#[tool(description = "...", annotations(read_only_hint = true))]`) - use this syntax;
- (b) there is no declarative support - then override `list_tools` in `ServerHandler`, take the base list from `tool_router`, and set `annotations` on each `Tool` by name via a classification table.

Record the chosen mechanism in the task report.

- [ ] **Step 2: Write a failing test**

Create `crates/wf-mcp/tests/tool_annotations_test.rs`:

```rust
use wf_mcp::server::WfMcp;
use rmcp::ServerHandler;

/// tools/list must carry annotations: read-only on reads, destructive on
/// mutations. Checked via the public list_tools surface.
#[tokio::test]
async fn tools_list_carries_safety_annotations() {
    let dir = tempfile::tempdir().unwrap();
    let server = WfMcp::new(dir.path().to_path_buf());
    let tools = server.list_all_tools().await; // replace with the actual rmcp call
    let by_name = |n: &str| tools.iter().find(|t| t.name == n).expect(n).clone();

    let list = by_name("workflow_list");
    assert_eq!(list.annotations.as_ref().and_then(|a| a.read_only_hint), Some(true));

    let run = by_name("workflow_run");
    assert_eq!(run.annotations.as_ref().and_then(|a| a.destructive_hint), Some(true));
}
```

Note: adjust the exact type/method names (`list_all_tools`, the `annotations` field) to the actual rmcp 2.2.0 API found in Step 1. The test asserts the invariant (annotations are present and correct), not the specific way they are set.

- [ ] **Step 3: Run - it fails**

Run: `cargo test -p wf-mcp --test tool_annotations_test`
Expected: FAIL - no annotations yet.

- [ ] **Step 4: Implement per the chosen mechanism**

Apply the classification from Interfaces via mechanism (a) or (b) from Step 1. Read-only and destructive are mutually exclusive for our tools; set exactly one matching hint (plus, where appropriate, a `title` with a human-readable name).

- [ ] **Step 5: Run - green**

Run: `cargo test -p wf-mcp --test tool_annotations_test`
Expected: PASS.

- [ ] **Step 6: Clippy and commit**

Run: `cargo clippy -p wf-mcp --all-targets`

```bash
git add crates/wf-mcp/src/server.rs crates/wf-mcp/tests/tool_annotations_test.rs
git commit -m "feat(mcp): tool safety annotations (readOnly/destructive hints)"
```

---

## Task 3: MCP surface documentation + connection snippets

**Files:**
- Create: `docs/MCP.md`

**Interfaces:** documentation only, no code.

- [ ] **Step 1: Write docs/MCP.md**

Sections:
- **WF as an MCP server:** `wf mcp` (stdio), what it is for.
- **Curated public tool surface:** list of read and mutation tools with a one-line description and a read-only / destructive marker (matching Task 2).
- **Async run model:** `workflow_run` with `background: true` -> run_id; poll `run_status`/`run_events`; review via `review_decide`. Explain why (some hosts have short tool-call timeouts).
- **Connection (local agents, no relay):**
  - Claude Desktop: a `claude_desktop_config.json` snippet with `command: "wf", args: ["mcp"]` and the project working directory.
  - opencode: an `opencode.json` snippet, `mcp` block, `type: "local"`, `command: ["wf", "mcp"]`.
  - Mention that Hermes/Pi consume stdio in the same way.
- **Cloud hosts (ChatGPT/Claude.ai):** one line - they require a hosted remote MCP endpoint (Feature 3), see the design doc section 13.3.

- [ ] **Step 2: Check for em dashes and exclamation marks**

Run: `grep -nP "\x{2014}|!" docs/MCP.md`
Expected: empty (no em dashes and no exclamation marks).

- [ ] **Step 3: Commit**

```bash
git add docs/MCP.md
git commit -m "docs: MCP server surface, async run model, client setup"
```

---

## Final check (after all tasks)

- [ ] `cargo test -p wf-mcp` - green.
- [ ] `cargo test --workspace` - green (async/annotation changes did not break adjacent crates).
- [ ] `cargo clippy --workspace --all-targets` - no new warnings.
- [ ] `grep -rnP "\x{2014}" crates/wf-mcp docs/MCP.md` - no em dashes in the changed content.
- [ ] Manual check: `workflow_run` with `background: true` returns a run_id immediately; `tools/list` shows annotations; the blocking `workflow_run` without the flag works as before.
