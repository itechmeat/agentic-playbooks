# Workflows CLI, Phase 2b (web run monitor) - implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Web run monitor: a run list and a run page where the workflow graph highlights node statuses in real time, with an event feed below it. Data comes from the engine (`wf-engine`) over HTTP; live updates come via WebSocket (a watcher on `.wf/runs`).

**Architecture:** The server (`wf-server`) reads run state as a fold over events from `wf-engine` and serves it over HTTP; the file watcher now also watches `.wf/runs` and sends `{"type":"runs_changed"}` on the shared WS channel. The frontend (Svelte) reloads the details of the open run on that event. The run and the server are separate processes; the only channel between them is the run's files on disk (event sourcing) - that is the streaming mechanism itself.

**Tech Stack:** Rust (edition 2024), axum, notify, wf-engine, wf-core; Bun, Vite, Svelte 5, @xyflow/svelte, vitest.

**Spec:** `docs/superpowers/specs/2026-07-08-workflows-cli-design.md` (8.1 live streaming, 8.7 statuses, 12 web interface); builds on Phase 2a (`wf-engine`).

## Global Constraints

- Binary name: `wf`; port `7321`, `127.0.0.1` only. Run state: `.wf/runs/<run-id>/`.
- Rust edition 2024. TDD is mandatory: a failing test first, then the implementation. Commit at the end of each task.
- Server error messages are in English; comments/documentation are in Russian.
- No em dashes and no exclamation marks in documentation or UI text.
- The workflow/run web page occupies the full area: a 40px control header, no side panels or footer (as in Phase 1). The graph lays out horizontally (LR).
- Run code-ranker before marking a task done; navigate the code via codegraph.
- Tests never invoke the real `claude`: to seed runs, use a workflow without agent_task (start -> prompt -> finish), or write `events.jsonl` by hand in the `wf_engine::event::Event` format.

---

### Task 1: HTTP endpoints for runs

**Files:**
- Modify: `crates/wf-server/Cargo.toml` (dep wf-engine), `crates/wf-server/src/lib.rs`
- Create: `crates/wf-server/tests/runs_api_test.rs`

**Interfaces:**
- Consumes: `wf_engine::{list_runs}`, `wf_engine::event::read_all`, `wf_engine::state::RunState`, `wf_engine::run_config::read_run_config`, `wf_core::schema::Workflow`.
- Produces:
  - `GET /api/runs` -> an array of `RunSummary` (as from `wf_engine::list_runs`).
  - `GET /api/runs/{id}` -> `{ "run_id", "workflow", "version", "run_status", "nodes": {<id>: <status>}, "outputs": {<id>: <text>}, "instruction": <string|null>, "params": {..}, "model": <workflow json | null>, "events": [<event>...] }`; 404 if the run directory doesn't exist.

- [ ] **Step 1: wf-engine dependency**

Run: `cargo add wf-engine --path crates/wf-engine -p wf-server`
Expected: `wf-engine = { path = "../wf-engine" }` appears in `crates/wf-server/Cargo.toml`.

- [ ] **Step 2: Failing tests**

`crates/wf-server/tests/runs_api_test.rs`:

```rust
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::fs;
use tower::ServiceExt;
use wf_server::{build_router, AppState};

const NOAGENT: &str = r#"
schema: 1
id: noagent
name: No Agent
version: 1.0.0
params:
  - { name: who, type: text }
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hi {{params.who}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

fn seed_with_run() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    wf_core::registry::init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".wf/workflows/noagent/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("workflow.yaml"), NOAGENT).unwrap();
    fs::write(dir.path().join(".wf/workflows/noagent/current"), "1.0.0").unwrap();
    // a real, agent-less run via the engine
    let mut opts = wf_engine::RunOptions::default();
    opts.params.insert("who".into(), "world".into());
    wf_engine::run(dir.path(), "noagent", None, opts).unwrap();
    dir
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = app.oneshot(Request::get(uri).body(Body::empty()).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn lists_runs() {
    let dir = seed_with_run();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/runs").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json[0]["workflow"], "noagent");
    assert_eq!(json[0]["status"], "succeeded");
}

#[tokio::test]
async fn run_detail_has_statuses_and_events() {
    let dir = seed_with_run();
    let run_id = wf_engine::list_runs(dir.path()).unwrap()[0].run_id.clone();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, &format!("/api/runs/{run_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["run_status"], "succeeded");
    assert_eq!(json["nodes"]["note"], "succeeded");
    assert_eq!(json["model"]["nodes"][0]["type"], "start");
    assert!(json["events"].as_array().unwrap().len() >= 3);
}

#[tokio::test]
async fn unknown_run_404() {
    let dir = seed_with_run();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = get_json(app, "/api/runs/ghost-1").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
```

- [ ] **Step 3: Confirm the tests fail**

Run: `cargo test -p wf-server --test runs_api_test`
Expected: FAIL (no routes, 404 for everything).

- [ ] **Step 4: Implement the endpoints**

In `crates/wf-server/src/lib.rs`, in `build_router`, add two routes (after the line with `/api/workflows/{id}`):

```rust
        .route("/api/runs", get(list_runs_handler))
        .route("/api/runs/{id}", get(get_run_handler))
```

And add handlers (e.g., after `get_workflow`):

```rust
async fn list_runs_handler(State(state): State<AppState>) -> impl IntoResponse {
    match wf_engine::list_runs(&state.root) {
        Ok(list) => Json(list).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_run_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    let run_dir = state.root.join(".wf/runs").join(&id);
    if !run_dir.is_dir() {
        return (StatusCode::NOT_FOUND, format!("run `{id}` not found")).into_response();
    }
    let events = match wf_engine::event::read_all(&run_dir) {
        Ok(ev) => ev,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let run_state = wf_engine::state::RunState::fold(&events);
    let cfg = wf_engine::run_config::read_run_config(&run_dir).unwrap_or_default();

    // Snapshot of the run's workflow (may be missing for very old runs).
    let (workflow_json, workflow_id, version) = {
        let path = run_dir.join("workflow.yaml");
        match std::fs::read_to_string(&path).ok()
            .and_then(|y| wf_core::schema::Workflow::from_yaml(&y).ok())
        {
            Some(wf) => (
                serde_json::to_value(&wf).unwrap_or(serde_json::Value::Null),
                wf.id.clone(),
                wf.version.clone(),
            ),
            None => (serde_json::Value::Null, id.clone(), String::new()),
        }
    };

    let nodes: std::collections::BTreeMap<String, String> = run_state.nodes.iter()
        .map(|(k, v)| (k.clone(), v.as_str().to_string())).collect();

    Json(serde_json::json!({
        "run_id": id,
        "workflow": workflow_id,
        "version": version,
        "run_status": run_state.run_status.as_str(),
        "nodes": nodes,
        "outputs": run_state.outputs,
        "instruction": cfg.instruction,
        "params": cfg.params,
        "model": workflow_json,
        "events": events,
    })).into_response()
}
```

Note: `RunState.nodes` is a `BTreeMap<String, NodeStatus>`; we manually serialize it into a map of status strings (above). `events` are serialized directly (`Event: Serialize`).

- [ ] **Step 5: Run the tests and commit**

Run: `cargo test -p wf-server --test runs_api_test`
Expected: 3 passed. Then `cargo test -p wf-server` - previous tests (api, ws) are green.

```bash
git add -A
git commit -m "feat(server): run list and run detail endpoints backed by wf-engine"
```

---

### Task 2: Watcher on .wf/runs and typed WS events

**Files:**
- Modify: `crates/wf-server/src/watch.rs`
- Create: `crates/wf-server/tests/runs_watch_test.rs`

**Interfaces:**
- Consumes: `AppState.events`.
- Produces: the watcher additionally watches `.wf/runs`; on a change under `runs` it sends `{"type":"runs_changed"}`, otherwise `{"type":"workflows_changed"}` (chosen based on the event's paths).

- [ ] **Step 1: Failing test**

`crates/wf-server/tests/runs_watch_test.rs`:

```rust
use std::fs;
use std::time::Duration;
use wf_server::AppState;

#[tokio::test]
async fn watcher_emits_runs_changed_on_run_file() {
    let dir = tempfile::tempdir().unwrap();
    wf_core::registry::init_project(dir.path()).unwrap();
    let state = AppState::new(dir.path().to_path_buf());
    let mut rx = state.events.subscribe();
    let _w = wf_server::watch::spawn_watcher(dir.path().to_path_buf(), state.events.clone()).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    fs::create_dir_all(dir.path().join(".wf/runs/demo-1")).unwrap();
    fs::write(dir.path().join(".wf/runs/demo-1/events.jsonl"), "{}\n").unwrap();

    // wait for a runs event (workflows events may slip through too - look for the right one)
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut saw_runs = false;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
            Ok(Ok(msg)) if msg.contains("runs_changed") => { saw_runs = true; break; }
            Ok(Ok(_)) => continue,
            _ => break,
        }
    }
    assert!(saw_runs, "expected a runs_changed event");
}
```

- [ ] **Step 2: Confirm the test fails**

Run: `cargo test -p wf-server --test runs_watch_test`
Expected: FAIL (the watcher doesn't watch runs, it only sends workflows_changed).

- [ ] **Step 3: Implement**

Replace `crates/wf-server/src/watch.rs`:

```rust
use std::path::PathBuf;

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::broadcast;

pub fn spawn_watcher(
    root: PathBuf,
    tx: broadcast::Sender<String>,
) -> notify::Result<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            // Choose the message type by paths: changes under runs are runs.
            let is_run = event.paths.iter().any(|p| {
                p.components().any(|c| c.as_os_str() == "runs")
            });
            let msg = if is_run {
                r#"{"type":"runs_changed"}"#
            } else {
                r#"{"type":"workflows_changed"}"#
            };
            // Ignore the send error: no subscribers means nothing to send.
            let _ = tx.send(msg.to_string());
        }
    })?;
    for sub in ["workflows", "profiles", "runs"] {
        let p = root.join(".wf").join(sub);
        if p.is_dir() {
            watcher.watch(&p, RecursiveMode::Recursive)?;
        }
    }
    Ok(watcher)
}
```

Note: `.wf/runs` is created by `init_project`, so the directory usually exists by the time the server starts.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p wf-server`
Expected: all green (including ws_test - `workflows_changed` is still sent for workflows changes).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(server): watch .wf/runs and emit typed runs_changed ws events"
```

---

### Task 3: Frontend - types, API, statuses in the graph

**Files:**
- Modify: `web/src/lib/types.ts`, `web/src/lib/api.ts`, `web/src/lib/graph.ts`, `web/src/lib/graph.test.ts`, `web/src/lib/WfNode.svelte`

**Interfaces:**
- Consumes: the endpoints from Task 1.
- Produces: `RunSummary`, `RunDetail` types; `fetchRuns()`, `fetchRun(id)`; `toFlow(workflow, layout, statuses?)` adds `data.status`; `WfNode` colors the node by status.

- [ ] **Step 1: Failing test for statuses in toFlow**

Add to `web/src/lib/graph.test.ts`:

```ts
it('annotates nodes with run status when provided', () => {
  const statuses = { start: 'succeeded', a: 'running', done: 'pending' }
  const { nodes } = toFlow(workflow, null, statuses)
  expect(nodes.find((n) => n.id === 'a')!.data.status).toBe('running')
  expect(nodes.find((n) => n.id === 'start')!.data.status).toBe('succeeded')
})

it('leaves status undefined when no statuses given', () => {
  const { nodes } = toFlow(workflow, null)
  expect(nodes[0].data.status).toBeUndefined()
})
```

- [ ] **Step 2: Confirm the test fails**

Run: `cd web && bun run test`
Expected: FAIL (toFlow ignores the 3rd argument; there's no `data.status`).

- [ ] **Step 3: Implement**

Add to `web/src/lib/types.ts`:

```ts
export interface RunSummary {
  run_id: string
  workflow: string
  status: string
  started_ts: number
}

export interface WfEvent {
  seq: number
  ts: number
  type: string
  node?: string | null
  [key: string]: unknown
}

export interface RunDetail {
  run_id: string
  workflow: string
  version: string
  run_status: string
  nodes: Record<string, string>
  outputs: Record<string, string>
  instruction: string | null
  params: Record<string, string>
  model: { id: string; name: string; nodes: WfNode[]; edges: WfEdge[] } | null
  events: WfEvent[]
}
```

Add to `web/src/lib/api.ts`:

```ts
import type { RunDetail, RunSummary, WorkflowDetail, WorkflowSummary } from './types'
```
(replace the existing import line, adding RunDetail and RunSummary), and at the end of the file:

```ts
export const fetchRuns = () => getJson<RunSummary[]>('/api/runs')
export const fetchRun = (id: string) =>
  getJson<RunDetail>(`/api/runs/${encodeURIComponent(id)}`)
```

In `web/src/lib/graph.ts`, extend `FlowNode.data` and `toFlow`:

```ts
export interface FlowNode {
  id: string
  position: { x: number; y: number }
  data: { title: string; kind: string; status?: string }
  type: 'wfNode'
}
```
and the signature:

```ts
export function toFlow(
  workflow: WfModel,
  layout: WfLayout,
  statuses?: Record<string, string>,
): { nodes: FlowNode[]; edges: FlowEdge[] } {
```
add status in the node mapping:

```ts
  const nodes: FlowNode[] = workflow.nodes.map((n) => ({
    id: n.id,
    type: 'wfNode',
    position: stored.get(n.id) ?? auto.get(n.id) ?? { x: 0, y: 0 },
    data: { title: n.title ?? n.id, kind: n.type, status: statuses?.[n.id] },
  }))
```

In `web/src/lib/WfNode.svelte`, color by status. Replace the contents of the `<script>` block and `<style>`:

```svelte
<script lang="ts">
  import { Handle, Position } from '@xyflow/svelte'
  let { data }: { data: { title: string; kind: string; status?: string } } = $props()
  const hasTarget = $derived(data.kind !== 'start')
  const hasSource = $derived(data.kind !== 'finish')
</script>

<div class="wf-node" data-kind={data.kind} data-status={data.status ?? ''}>
  {#if hasTarget}<Handle type="target" position={Position.Left} />{/if}
  <span class="kind">{data.kind}</span>
  <strong>{data.title}</strong>
  {#if data.status}<span class="status">{data.status}</span>{/if}
  {#if hasSource}<Handle type="source" position={Position.Right} />{/if}
</div>

<style>
  .wf-node {
    padding: 8px 12px;
    border: 1px solid #8884;
    border-radius: 8px;
    background: #ffffff;
    color: #1a1a1a;
    min-width: 160px;
  }
  @media (prefers-color-scheme: dark) {
    .wf-node { background: #242430; color: #e6e6e6; border-color: #3a3a44; }
  }
  .kind { display: block; font-size: 11px; opacity: 0.6; }
  .status { display: block; font-size: 11px; margin-top: 2px; opacity: 0.8; }
  [data-kind='condition'] { border-style: dashed; }
  [data-status='running'] { border-color: #2563eb; box-shadow: 0 0 0 2px #2563eb55; }
  [data-status='succeeded'] { border-color: #22a06b; }
  [data-status='failed'], [data-status='timed_out'] { border-color: #dc2626; }
  [data-status='interrupted'], [data-status='unknown'] { border-color: #d97706; }
</style>
```

Note: the start/finish indicator (start/finish border color) gives way to status coloring in the monitor; on the workflow page (without statuses) `data-status=""` and only the base styles apply - this is acceptable.

- [ ] **Step 4: Run the tests and the build**

Run: `cd web && bun run test && bun run build`
Expected: all tests (including the new status ones) are green, the build succeeds.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(web): run types, api, node status annotation and coloring"
```

---

### Task 4: Frontend - run list page and run monitor

**Files:**
- Create: `web/src/pages/RunList.svelte`, `web/src/pages/RunView.svelte`
- Modify: `web/src/App.svelte`, `web/src/pages/WorkflowList.svelte` (link to runs)

**Interfaces:**
- Consumes: `fetchRuns`, `fetchRun`, `toFlow`, `subscribeChanges`.
- Produces: routes `#/runs` (run list) and `#/run/<id>` (monitor); the workflow-list header gets a link to runs; the monitor updates live over WS.

- [ ] **Step 1: Run list page**

`web/src/pages/RunList.svelte`:

```svelte
<script lang="ts">
  import { fetchRuns } from '../lib/api'
  import { subscribeChanges } from '../lib/ws'
  import type { RunSummary } from '../lib/types'

  let items = $state<RunSummary[]>([])
  let error = $state<string | null>(null)

  async function load() {
    try { items = await fetchRuns(); error = null }
    catch (e) { error = String(e) }
  }

  $effect(() => {
    load()
    return subscribeChanges(load)
  })
</script>

<header class="topbar">
  <a href="#/">workflows</a>
  <h1>Runs</h1>
  <span class="spacer"></span>
  {#if error}<span class="badge err" title={error}>error</span>{/if}
</header>

<div class="page-scroll">
  {#if items.length === 0 && !error}<p>No runs yet.</p>{/if}
  <ul>
    {#each items as r (r.run_id)}
      <li>
        <a href={`#/run/${r.run_id}`}><strong>{r.run_id}</strong></a>
        <span class="meta">{r.workflow} · {r.status}</span>
      </li>
    {/each}
  </ul>
</div>
```

- [ ] **Step 2: Run monitor page**

`web/src/pages/RunView.svelte`:

```svelte
<script lang="ts">
  import { SvelteFlow, Background, Controls } from '@xyflow/svelte'
  import '@xyflow/svelte/dist/style.css'
  import { fetchRun } from '../lib/api'
  import { toFlow, type FlowEdge, type FlowNode } from '../lib/graph'
  import { subscribeChanges } from '../lib/ws'
  import WfNode from '../lib/WfNode.svelte'
  import type { RunDetail } from '../lib/types'

  let { id }: { id: string } = $props()

  let nodes = $state.raw<FlowNode[]>([])
  let edges = $state.raw<FlowEdge[]>([])
  let detail = $state<RunDetail | null>(null)
  let error = $state<string | null>(null)

  const nodeTypes = { wfNode: WfNode }

  async function load() {
    try {
      const d = await fetchRun(id)
      detail = d
      if (d.model) {
        const flow = toFlow(d.model, null, d.nodes)
        nodes = flow.nodes
        edges = flow.edges
      }
      error = null
    } catch (e) {
      error = String(e)
    }
  }

  $effect(() => {
    load()
    return subscribeChanges(load)
  })
</script>

<header class="topbar">
  <a href="#/runs">runs</a>
  <h1>{id}</h1>
  {#if detail}<span class="muted">{detail.workflow} · {detail.run_status}</span>{/if}
  <span class="spacer"></span>
  {#if error}<span class="badge err" title={error}>error</span>{/if}
</header>

<div class="run-body">
  <div class="graph">
    <SvelteFlow bind:nodes bind:edges {nodeTypes} fitView
      nodesDraggable={false} nodesConnectable={false} elementsSelectable={false}>
      <Background />
      <Controls />
    </SvelteFlow>
  </div>
  <aside class="events">
    {#if detail}
      <ol>
        {#each detail.events as e (e.seq)}
          <li><code>{e.type}</code>{#if e.node} <span class="muted">{e.node}</span>{/if}</li>
        {/each}
      </ol>
    {/if}
  </aside>
</div>

<style>
  .run-body { flex: 1 1 auto; min-height: 0; display: flex; }
  .graph { flex: 1 1 auto; min-height: 0; position: relative; }
  .events {
    flex: 0 0 280px;
    overflow: auto;
    border-left: 1px solid var(--border);
    padding: 8px 10px;
    font-size: 12px;
  }
  .events ol { margin: 0; padding-left: 18px; }
  .events code { font-size: 11px; }
  .muted { opacity: 0.6; }
</style>
```

Note: the event feed is a narrow right-hand column inside the run area (not an app-wide side panel); the 40px header and full-screen layout are preserved. This is the only place where an auxiliary column is allowed - it's part of the run monitor itself, not app chrome.

- [ ] **Step 3: Routing**

`web/src/App.svelte` (full replacement):

```svelte
<script lang="ts">
  import WorkflowList from './pages/WorkflowList.svelte'
  import WorkflowView from './pages/WorkflowView.svelte'
  import RunList from './pages/RunList.svelte'
  import RunView from './pages/RunView.svelte'

  let hash = $state(location.hash)
  $effect(() => {
    const onHash = () => (hash = location.hash)
    window.addEventListener('hashchange', onHash)
    return () => window.removeEventListener('hashchange', onHash)
  })

  const route = $derived.by(() => {
    if (hash.startsWith('#/wf/')) return { page: 'wf', id: decodeURIComponent(hash.slice(5)) }
    if (hash.startsWith('#/run/')) return { page: 'run', id: decodeURIComponent(hash.slice(6)) }
    if (hash.startsWith('#/runs')) return { page: 'runs', id: '' }
    return { page: 'workflows', id: '' }
  })
</script>

{#if route.page === 'wf'}
  <WorkflowView id={route.id} />
{:else if route.page === 'run'}
  <RunView id={route.id} />
{:else if route.page === 'runs'}
  <RunList />
{:else}
  <WorkflowList />
{/if}
```

In `web/src/pages/WorkflowList.svelte`, add a link to runs in the header: replace the line `<span class="spacer"></span>` with:

```svelte
  <span class="spacer"></span>
  <a href="#/runs">runs</a>
```

- [ ] **Step 4: Tests and build**

Run: `cd web && bun run test && bun run build`
Expected: tests are green (graph.test), the build succeeds (pages compile).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(web): run list and live run monitor pages with hash routing"
```

---

### Task 5: End-to-end check, static assets, code-ranker, status

**Files:**
- Modify: `docs/tasks.md`, `CHANGELOG.md`

**Interfaces:**
- Produces: a working run monitor in the baked build; status checkmarks.

- [ ] **Step 1: Rebuild static assets and the whole workspace**

```bash
cd web && bun run build && cd ..
cargo test --workspace
```
Expected: all tests are green, the frontend build succeeds.

- [ ] **Step 2: End-to-end manual check**

```bash
cargo build -p wf-cli
DEMO=$(mktemp -d)
cd "$DEMO"
/Users/techmeat/www/projects/omniteamhq/workflows/target/debug/wf init
mkdir -p .wf/workflows/noagent/1.0.0
cat > .wf/workflows/noagent/1.0.0/workflow.yaml <<'YAML'
schema: 1
id: noagent
name: No Agent
version: 1.0.0
params:
  - { name: who, type: text }
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hi {{params.who}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
YAML
printf "1.0.0" > .wf/workflows/noagent/current
/Users/techmeat/www/projects/omniteamhq/workflows/target/debug/wf run noagent --param who=world
/Users/techmeat/www/projects/omniteamhq/workflows/target/debug/wf serve --no-open &
sleep 1
curl -s http://127.0.0.1:7321/api/runs | head -c 300
echo
RID=$(curl -s http://127.0.0.1:7321/api/runs | python3 -c "import sys,json;print(json.load(sys.stdin)[0]['run_id'])")
curl -s "http://127.0.0.1:7321/api/runs/$RID" | python3 -c "import sys,json;d=json.load(sys.stdin);print('run_status',d['run_status'],'nodes',d['nodes'],'events',len(d['events']))"
kill %1
```
Expected: `/api/runs` returns the noagent run with status succeeded; the detail shows `run_status succeeded`, node statuses, and an event count >= 3. In the browser at `http://127.0.0.1:7321/#/runs` the run is visible; clicking it opens the monitor: a horizontal graph with green nodes and an event feed on the right.

- [ ] **Step 2b: Run code-ranker**

```bash
cd /Users/techmeat/www/projects/omniteamhq/workflows
cargo metadata --format-version 1 >/dev/null
code-ranker check .
```
Expected: `no violations`. Otherwise: `report --output.scorecard --focus <ID> --top 1`, read `docs base <ID>`, fix, repeat.

- [ ] **Step 3: CHANGELOG and tasks.md**

Add a section to `CHANGELOG.md` (using regular hyphens, no em dashes):

```markdown
## [0.2.0] - phases 2a + 2b

### Added
- Execution engine (wf-engine): event sourcing, wf run/runs/resume, retry and fallbacks, sh-script, shared context, one-off instruction, max_loops.
- Web run monitor: run list, run page with live highlighting of node statuses and an event feed (watcher on .wf/runs + WebSocket).
```

In `docs/tasks.md`, in the "Phase 2" section, mark the "Run monitor in the web UI" item as done (`[x]`).

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "docs: changelog and tasks status for phase 2b web run monitor"
```

---

## What is deliberately NOT part of Phase 2b (for the reviewer)

- Starting/canceling a run from the web UI (Run button, abort) - later; 2b is observe-only.
- A granular per-run WS channel (currently one shared `runs_changed` event, the client reloads the open run) - sufficient for a local tool.
- Streaming a node's partial output (`node_progress`) and rendering reports/`context.md` in the web UI - later.
- Parallel branches, human_review/wait, the supervising agent - other phases.
