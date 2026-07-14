# Workflows CLI, Phase 5b (visual editor in the browser) - implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:subagent-driven-development or superpowers:executing-plans to execute the plan task-by-task. Steps are marked with checkboxes (`- [ ]`).

**Goal:** a visual workflow editor in the browser on top of the finished Phase 5a API. The user edits a workflow (YAML panel with a live graph, node forms, palette, edge connecting), saves (= a new minor version), switches between versions with a diff, and creates/duplicates/deletes workflows from the list. Source of truth - YAML (spec 12); the graph and forms are its representation.

**Architecture:** Frontend (Svelte 5 runes + @xyflow/svelte + CodeMirror 6 + vitest), static assets baked into the binary (already set up). In the editor, the single source of truth is the YAML text: the graph is derived as `toFlow(parseWorkflow(yaml).model, layout)` and redrawn whenever the YAML changes; structural edits (palette/forms/edges) mutate the model and are re-serialized back to YAML. Saving posts YAML to the 5a endpoints (`POST/PUT /api/workflows`), the backend validates and creates a version (invalid YAML -> 400 with codes). Layout is saved separately (`PUT .../layout`) when nodes are dragged. Validation in the editor: live YAML parse errors on the client (best-effort), authoritative validation happens on the backend at save time and in `GET /api/workflows/{id}` (which already returns a `validation` array).

**Tech Stack:** Bun + Vite 8 + Svelte 5.56 (runes: $state, $derived, $effect, $props), @xyflow/svelte 1.6, @dagrejs/dagre 3, vitest 4. New additions: `yaml` (YAML parse/serialize in JS), `codemirror` 6 + `@codemirror/lang-yaml` (code editor). Check the current versions of new npm packages online at install time and pin them (project rule, spec 18); on incompatibility - a careful downgrade with double-checking.

**Spec:** `docs/superpowers/specs/2026-07-08-workflows-cli-design.md` - 12 (screens, especially 2 "Workflow editor"), 10 (versions/diff/layout). Builds on Phases 1-4 and 5a (the versions backend API).

## Global Constraints

- Comments/docs in English; error text and code identifiers in English. NO em dashes (U+2014) and no exclamation marks in code/comments/UI text.
- Project version stays at `0.1.0`.
- TDD: cover pure logic (YAML parse/serialize, model mutations, diff formatting, URL building) with vitest; for Svelte components, move logic into testable modules and keep components thin.
- Source of truth in the editor is the YAML text. Structural edits go through the model and re-serialization back to YAML, never around it.
- Keep the full-screen layout: 40px header, work area filling the rest of the height, no footer (as in the existing pages, styles in `web/src/app.css`).
- Save does NOT mutate the existing version - it always creates a new one (5a semantics). Layout is mutable (separate endpoint).
- Live updates from disk (watcher + WS) already exist - the list and viewer pick up others' changes; the editor does not need to listen to WS while actively editing (to avoid clobbering edits), but must show the new version after saving.
- Pin new npm versions; `bun run build`, `bun run test`, `bun run check` must be green before handoff.

## Current frontend layout (mirror it, do not break it)

- `web/src/App.svelte`: hash routing. `#/wf/<id>` -> WorkflowView, `#/run/<id>` -> RunView, `#/runs` -> RunList, otherwise WorkflowList. New routes go here.
- `web/src/lib/api.ts`: `getJson<T>(url)`; `fetchWorkflows()`, `fetchWorkflow(id) -> WorkflowDetail`, `fetchRuns()`, `fetchRun(id)`, `fetchRunReport(id)`. Pattern: thin functions wrapping fetch, errors via `throw new Error`.
- `web/src/lib/types.ts`: `WorkflowSummary {id,name,description,current,versions[]}`, `WorkflowDetail {id,version,yaml,workflow:{id,name,nodes:WfNode[],edges:WfEdge[]},layout:{nodes?:LayoutNode[]}|null,validation[]}`, `WfNode {id,type,title?,[k]:unknown}`, `WfEdge {from,to,condition?,fallback?}`, `LayoutNode {id,x,y}`.
- `web/src/lib/graph.ts`: `toFlow(workflow, layout, statuses?) -> {nodes:FlowNode[], edges:FlowEdge[]}` (LR dagre auto-layout if no saved positions). `FlowNode {id,position:{x,y},data:{title,kind,status?},type:'wfNode'}`, `FlowEdge {id,source,target,label?}`.
- `web/src/pages/WorkflowView.svelte`: read-only view (graph + validation badge). Base for edit mode - do NOT replace it, add a separate editor page.
- `web/src/lib/WfNode.svelte`: custom node (Left/Right handles, status coloring).
- vitest is already used (`web/src/lib/graph.test.ts`, `journal.test.ts`) - mirror the test style.
- 5a API (ready): `GET /api/workflows`, `GET /api/workflows/{id}`, `POST /api/workflows` (body `{id,yaml}` -> `{id,version}`), `PUT /api/workflows/{id}` (body `{yaml}` -> `{id,version}`), `DELETE /api/workflows/{id}` (-> `{trashed}`), `PUT /api/workflows/{id}/layout?version=` (body `{layout}` - string or object), `GET /api/workflows/{id}/diff?from=&to=` (-> `VersionDiff {nodes_added,nodes_removed,nodes_changed,edges_added,edges_removed,yaml_diff}`).

---

### Task 1: Dependencies, API client, CodeEditor component

**Files:** Modify `web/package.json` (deps), `web/src/lib/api.ts`, `web/src/lib/types.ts`; Create `web/src/lib/CodeEditor.svelte`, `web/src/lib/api.test.ts`.

**Steps:**
- [ ] Check online for current versions and add to `web` (bun add): `yaml`, `codemirror`, `@codemirror/lang-yaml` (and, transitively, `@codemirror/state`/`@codemirror/view` if not pulled in automatically). Pin versions.
- [ ] `types.ts`: add `VersionDiff { nodes_added:string[]; nodes_removed:string[]; nodes_changed:string[]; edges_added:string[]; edges_removed:string[]; yaml_diff:string }` and `WriteResult { id:string; version:string }`.
- [ ] `api.ts`: add (mirror the existing style; for non-GET use `fetch` with `method`, `headers {content-type: application/json}`, `body: JSON.stringify(...)`, throwing `Error` when `!res.ok`):
  - `createWorkflow(id:string, yaml:string): Promise<WriteResult>` -> `POST /api/workflows`.
  - `updateWorkflow(id:string, yaml:string): Promise<WriteResult>` -> `PUT /api/workflows/{id}`.
  - `deleteWorkflow(id:string): Promise<{trashed:string}>` -> `DELETE /api/workflows/{id}`.
  - `saveLayout(id:string, version:string, layout:unknown): Promise<void>` -> `PUT /api/workflows/{id}/layout?version=<enc>` body `{layout}` (204 - no body).
  - `fetchDiff(id:string, from:string, to:string): Promise<VersionDiff>` -> `GET /api/workflows/{id}/diff?from=&to=`.
  - Where possible, surface backend errors (400 with `{error, codes}`) as the text of the thrown `Error` (for display in the UI).
- [ ] `CodeEditor.svelte`: thin wrapper over CodeMirror 6. Props (runes `$props`): `value: string`, `onChange: (v:string)=>void`, optional `language: 'yaml'|'text'` (default yaml), optional `readonly?: boolean`. Initialize `EditorView` in `$effect` on mount (basic setup + yaml language), call `onChange` with the content; properly destroy the view on unmount; when `value` changes externally (does not match the current content) - update the document. Comments in English.
- [ ] `api.test.ts` (vitest): mock the global `fetch`, verify that `createWorkflow/updateWorkflow/deleteWorkflow/saveLayout/fetchDiff` hit the right URL with the right method/body and parse the response; on `!ok` they throw an Error. Mirror the style of the existing vitest tests.
- [ ] `bun run test`, `bun run check`, `bun run build` green. Commit: `feat(web): api write client, VersionDiff type, CodeEditor (CodeMirror 6)`.

---

### Task 2: YAML<->model module and editor page

**Files:** Create `web/src/lib/wfyaml.ts`, `web/src/lib/wfyaml.test.ts`, `web/src/pages/WorkflowEdit.svelte`; Modify `web/src/App.svelte` (routes `#/edit/<id>` and `#/new`).

**Interfaces:**
- `wfyaml.ts` (pure, built on the `yaml` package):
  - `type WfModel = WorkflowDetail['workflow']` (or an explicit type with nodes/edges).
  - `parseWorkflow(text:string): { model?: WfModel; error?: string }` - parses YAML; on a syntax/structural error returns `{error}` (human-readable), otherwise `{model}`. The model must be usable by `toFlow` (must have `nodes:[{id,type,title?,...}]`, `edges:[{from,to,...}]`, `name`).
  - `serializeWorkflow(model:WfModel): string` - back to YAML (deterministic).
  - Note: this is a client-side PREVIEW/structural-edit path; the authoritative parse/validation is the backend at save time. The client-side parse tolerates incompleteness (for the live graph).
- `WorkflowEdit.svelte` (page, runes):
  - Props: `id: string` (empty for `#/new`).
  - State: `yamlText: string` ($state), `isNew: boolean`, `saveError: string|null`, `parseError: string|null`, `saving: boolean`.
  - Loading: for an existing workflow - `fetchWorkflow(id)`, take `detail.yaml` into `yamlText`; for a new one - a starter YAML template (a valid minimum: start -> finish).
  - Two panes in the work area (full-screen layout, 40px header): on the left `CodeEditor(value=yamlText, onChange)`, on the right the live graph via `SvelteFlow` from `toFlow(parseWorkflow(yamlText).model, null)` (redraw on YAML change, with a debounce of ~200-300ms; on `parseError` - do not break the graph, show the last valid one plus a parse-error marker).
  - Save button: `isNew ? createWorkflow(idInput, yamlText) : updateWorkflow(id, yamlText)`; on success - navigate to `#/wf/<id>` (or show the new version); on error (400) - show `saveError` (backend text/codes). For a new workflow - an id input field (must validate non-empty, no `/`).
  - An "edit" link from `WorkflowView` to `#/edit/<id>` (add a button in the WorkflowView header).
- `App.svelte`: route `#/edit/<id>` -> `WorkflowEdit id=<id>`; `#/new` -> `WorkflowEdit id=""`.

**Steps:**
- [ ] `wfyaml.test.ts` (vitest): round-trip `serialize(parse(valid).model) ~ an equivalent model` (compare by model/structure, not bytes); `parse(broken YAML)` -> `{error}`; `parse(valid)` -> a model usable by `toFlow` (the graph builds without exceptions). Take a valid YAML sample (an inline constant mirroring the shape of `crates/wf-core/tests/fixtures/valid.yaml` is fine).
- [ ] Implement `wfyaml.ts`, `WorkflowEdit.svelte`, the routes, the edit button in WorkflowView.
- [ ] `bun run test`, `bun run check`, `bun run build` green. Commit: `feat(web): workflow editor page (YAML source + live graph + save as new version)`.

---

### Task 3: Palette, node property forms, edge editing

**Files:** Modify `web/src/pages/WorkflowEdit.svelte`; Create `web/src/lib/wfedit.ts` (model mutations), `web/src/lib/NodePanel.svelte`, `web/src/lib/wfedit.test.ts`.

**IMPORTANT (preserving top-level fields):** in 1b `CodeEditor` is a plain textarea (CodeMirror is deferred, not needed yet), and `serializeWorkflow` from `wfyaml.ts` serializes ONLY `id/name/nodes/edges`. The structural edits in Task 3 must NOT lose other top-level fields (`schema`, `version`, `params`, `executors`, `defaults`, `supervisor`). Implement via one of two approaches: (a) edit the YAML document through the `yaml` package's AST (`YAML.parseDocument` + targeted `setIn`/`deleteIn` edits, preserving the rest), or (b) extend the parsing so the model retains the entire original parsed object and edits are applied to it, with `serialize` emitting all fields. The round-trip `parse -> (edit) -> serialize` on a workflow with all fields must preserve them - cover this with a test.

**Interfaces:**
- `wfedit.ts` (pure model mutations, cover with vitest): `addNode(model, kind, id) -> model`, `removeNode(model, id) -> model` (also removes connected edges), `updateNode(model, id, patch) -> model`, `addEdge(model, from, to, condition?) -> model`, `removeEdge(model, from, to) -> model`. All return a NEW model (immutably), never mutating the input. Mutations must PRESERVE top-level fields (see above).
- `NodePanel.svelte`: a property form for the selected node, per its type (prompt: text; agent_task: executor agent+model+fallbacks, profile, max_retries, timeout; script: runner+script; condition: max_loops; finish: outcome). Props: `node`, `onChange(patch)`. Thin component; all the mutation-applying logic lives in `wfedit.updateNode` on the page side.
- In `WorkflowEdit.svelte`: a node-type palette (add buttons); selecting a node on the canvas opens `NodePanel`; connecting edges on the canvas (svelte-flow's `onconnect`) -> `wfedit.addEdge`; node/edge deletion. Every structural edit: apply to the model -> `serializeWorkflow` -> update `yamlText` (YAML stays the source of truth, CodeEditor shows the result).

**Steps:**
- [ ] `wfedit.test.ts`: cover every mutation (add/remove/update node, add/remove edge; removeNode cleans up edges; input immutability).
- [ ] Implement `wfedit.ts`, `NodePanel.svelte`, integrate into the page.
- [ ] `bun run test/check/build` green. Commit: `feat(web): node palette, property forms, edge editing (model mutations -> yaml)`.

---

### Task 4: Save layout on drag + version switcher with diff

**Files:** Modify `web/src/pages/WorkflowEdit.svelte` (or `WorkflowView.svelte` for the diff); Create `web/src/lib/DiffView.svelte`, and if needed `web/src/lib/difffmt.ts` + a test.

**Interfaces:**
- Layout: on `onnodedragstop` collect node positions (`[{id,x,y}]`) and call `saveLayout(id, version, {nodes})`. Debounce/coalesce so as not to spam. Layout does not create a version.
- Version switcher: a dropdown built from `WorkflowDetail`/`WorkflowSummary.versions`; selecting a version loads it (`fetchWorkflow(id, version)` - extend `fetchWorkflow` with an optional version and `GET /api/workflows/{id}?version=` if needed; the endpoint already supports `?version=`).
- Diff: pick from/to versions -> `fetchDiff(id, from, to)` -> `DiffView` shows structural lists (nodes added/removed/changed, edges added/removed) and a text `yaml_diff` (monospace, with highlighted `+`/`-`/` ` lines). If a pure formatting function for turning `yaml_diff` into lines/classes emerges, extract it into `difffmt.ts` and cover it with vitest.

**Steps:**
- [ ] A test for `difffmt` (if extracted) and/or for the extended `fetchDiff` in api.test.
- [ ] Implement layout saving, the version switcher, `DiffView`.
- [ ] `bun run test/check/build` green. Commit: `feat(web): layout persistence on drag, version switcher with diff view`.

---

### Task 5: Workflow CRUD from the list

**Files:** Modify `web/src/pages/WorkflowList.svelte`, `web/src/App.svelte` (route `#/new` already from Task 2).

**Interfaces:**
- On the list cards: "Create" (navigate to `#/new`), "Duplicate" (load the source's yaml, open `#/new` prefilled with the YAML and a new id - via query/param or transient state; simplest: navigate into the new editor with the source's text), "Delete" (confirmation -> `deleteWorkflow(id)` -> refresh the list). No em dashes/exclamation marks in button labels.
- Refresh the list after operations (re-fetch `fetchWorkflows`; the WS watcher will also refresh it).

**Steps:**
- [ ] Implement the buttons and handlers (move the duplicate logic into a testable helper if non-trivial).
- [ ] `bun run test/check/build` green. Commit: `feat(web): workflow list CRUD (create, duplicate, delete to trash)`.

---

### Task 6: Build, docs, closing out Phase 5

**Files:** Modify `docs/tasks.md`, `CHANGELOG.md`, `README.md`; run code-ranker if needed.

**Steps:**
- [ ] `bun run build` (static assets for baking) + `bun run test` + `bun run check` green; `cargo test --workspace` green (the baked static assets do not break the binary build). `cargo metadata --format-version 1 >/dev/null && code-ranker check .` with no violations.
- [ ] README: a section about the editor (YAML panel + live graph, node forms, palette, version switcher with diff, list CRUD).
- [ ] tasks.md "Phase 5": "Node and edge editor", "CodeMirror", "Workflow CRUD from the web UI" -> `[x]`; note that Phase 5 is fully done (5a + 5b).
- [ ] CHANGELOG `### Added`: a line about the visual editor.
- [ ] Commit: `docs: mark phase 5b (visual editor) done; phase 5 complete`.

---

## What is deliberately NOT part of Phase 5b (for the reviewer)

- Major versions (an explicit user action) and physical deletion with force - later.
- Single-file export/import (ui.xyflow) - Phase 9.
- Agent self-patches / patch versions / promotion on success - Phase 6 (on top of the 5a version machinery).
- Dev mode with Vite HMR (`wf dev`) - Phase 9.
- Run overrides (effective workflow) - groundwork for spec 11, not this phase.
