<script lang="ts">
  import { SvelteFlow, Background, Controls } from '@xyflow/svelte'
  import { untrack } from 'svelte'
  import '@xyflow/svelte/dist/style.css'
  import {
    createPlaybook,
    fetchPlaybook,
    fetchPlaybooks,
    saveLayout,
    updatePlaybook,
  } from '../lib/api'
  import CodeEditor from '../lib/CodeEditor.svelte'
  import DiffView from '../lib/DiffView.svelte'
  import { toFlow, type FlowEdge, type FlowNode } from '../lib/graph'
  import NodePanel from '../lib/NodePanel.svelte'
  import PlaybookNode from '../lib/PlaybookNode.svelte'
  import { takeDraftYaml } from '../lib/playbookdupe'
  import { addEdge, addNode, removeEdge, removeNode, suggestNodeId, updateNode } from '../lib/playbookedit'
  import { docToString, NEW_PLAYBOOK_TEMPLATE, parseDoc, parsePlaybook } from '../lib/playbookyaml'
  import type { PlaybookModel } from '../lib/playbookyaml'
  import type { PlaybookNode as PlaybookNodeType } from '../lib/types'
  import type { Document } from 'yaml'

  let { id }: { id: string } = $props()

  const isNew = $derived(!id)

  // Palette of node types.
  const NODE_KINDS = ['start', 'agent_task', 'script', 'condition', 'finish'] as const

  let yamlText = $state('')
  let idInput = $state('')
  let debouncedYaml = $state('')
  let lastValidModel = $state<PlaybookModel | null>(null)
  let nodes = $state.raw<FlowNode[]>([])
  let edges = $state.raw<FlowEdge[]>([])
  let parseError = $state<string | null>(null)
  let saveError = $state<string | null>(null)
  let saving = $state(false)
  let loadError = $state<string | null>(null)

  // Versions and layout.
  let loadedVersion = $state('')
  let versions = $state<string[]>([])
  let storedLayout = $state<{ nodes?: { id: string; x: number; y: number }[] } | null>(null)

  // Selection and inspectors.
  let selectedNodeId = $state<string | null>(null)
  let selectedEdge = $state<{ from: string; to: string } | null>(null)
  let showDiff = $state(false)
  // Bumped on load/version-switch so NodePanel re-syncs its fields.
  let revision = $state(0)

  const nodeTypes = { playbookNode: PlaybookNode }
  let debounceTimer: ReturnType<typeof setTimeout> | undefined
  let layoutTimer: ReturnType<typeof setTimeout> | undefined

  const selectedNode = $derived(
    selectedNodeId && lastValidModel
      ? (lastValidModel.nodes.find((n) => n.id === selectedNodeId) ?? null)
      : null,
  )
  const canEditStruct = $derived(!parseError && !!lastValidModel)

  function onYamlChange(v: string) {
    yamlText = v
    clearTimeout(debounceTimer)
    debounceTimer = setTimeout(() => {
      debouncedYaml = v
    }, 250)
  }

  async function load() {
    loadError = null
    selectedNodeId = null
    selectedEdge = null
    if (isNew) {
      const draft = takeDraftYaml()
      yamlText = draft?.yaml ?? NEW_PLAYBOOK_TEMPLATE
      idInput = draft?.suggestedId ?? ''
      debouncedYaml = yamlText
      revision++
      return
    }
    try {
      const detail = await fetchPlaybook(id)
      yamlText = detail.yaml
      idInput = id
      debouncedYaml = yamlText
      loadedVersion = detail.version
      storedLayout = detail.layout
      revision++
      // List of versions for the switcher.
      try {
        const all = await fetchPlaybooks()
        const found = all.find((w) => w.id === id)
        versions = found?.versions ?? (detail.version ? [detail.version] : [])
      } catch {
        versions = detail.version ? [detail.version] : []
      }
      loadError = null
    } catch (e) {
      loadError = String(e)
    }
  }

  async function loadVersion(v: string) {
    if (isNew || !v) return
    try {
      const detail = await fetchPlaybook(id, v)
      yamlText = detail.yaml
      debouncedYaml = yamlText
      loadedVersion = detail.version
      storedLayout = detail.layout
      selectedNodeId = null
      selectedEdge = null
      revision++
      loadError = null
    } catch (e) {
      loadError = String(e)
    }
  }

  $effect(() => {
    id
    load()
  })

  $effect(() => {
    if (!debouncedYaml) return
    const { model, error } = parsePlaybook(debouncedYaml)
    if (model) {
      lastValidModel = model
      parseError = null
      // Merge the stored layout (storedLayout) with the current node positions
      // (prev). prev takes priority - dragged positions override the stored
      // ones. On initial load prev is empty and the stored positions are used;
      // new nodes (absent from both prev and stored) get the dagre auto-layout.
      // Read nodes without tracking, otherwise this would loop.
      const prev = untrack(() =>
        nodes.map((n) => ({ id: n.id, x: n.position.x, y: n.position.y })),
      )
      const stored = (storedLayout?.nodes ?? []).map((n) => ({
        id: n.id,
        x: n.x,
        y: n.y,
      }))
      const merged = new Map<string, { id: string; x: number; y: number }>()
      for (const n of stored) merged.set(n.id, n)
      for (const n of prev) merged.set(n.id, n)
      const flow = toFlow(model, { nodes: [...merged.values()] })
      nodes = flow.nodes
      edges = flow.edges
    } else if (error) {
      parseError = error
    }
  })

  // Applies a structural mutation to the YAML document and updates yamlText.
  function applyMutation(fn: (doc: Document) => Document): boolean {
    const { doc, error } = parseDoc(yamlText)
    if (error || !doc) {
      parseError = error ?? 'cannot parse YAML'
      return false
    }
    const next = fn(doc)
    yamlText = docToString(next)
    clearTimeout(debounceTimer)
    debouncedYaml = yamlText
    return true
  }

  function onAddNode(kind: string) {
    const { doc } = parseDoc(yamlText)
    if (!doc) return
    const nodeId = suggestNodeId(doc, kind)
    if (applyMutation((d) => addNode(d, kind, nodeId))) {
      selectedNodeId = nodeId
      selectedEdge = null
    }
  }

  function onNodeClick({ node }: { node: FlowNode }) {
    selectedNodeId = node.id
    selectedEdge = null
  }

  function onEdgeClick({ edge }: { edge: FlowEdge }) {
    selectedEdge = { from: edge.source, to: edge.target }
    selectedNodeId = null
  }

  function onConnect(conn: { source: string; target: string }) {
    if (!conn.source || !conn.target) return
    applyMutation((d) => addEdge(d, conn.source, conn.target))
  }

  function onPaneClick() {
    selectedNodeId = null
    selectedEdge = null
  }

  function onNodePatch(patch: Record<string, unknown>) {
    if (!selectedNodeId) return
    const targetId = selectedNodeId
    applyMutation((d) => updateNode(d, targetId, patch))
  }

  function onDeleteNode() {
    if (!selectedNodeId) return
    const target = selectedNodeId
    selectedNodeId = null
    applyMutation((d) => removeNode(d, target))
  }

  function onDeleteEdge() {
    if (!selectedEdge) return
    const target = selectedEdge
    selectedEdge = null
    applyMutation((d) => removeEdge(d, target.from, target.to))
  }

  function onNodeDragStop() {
    if (isNew || !loadedVersion) return
    clearTimeout(layoutTimer)
    layoutTimer = setTimeout(async () => {
      const positions = untrack(() =>
        nodes.map((n) => ({ id: n.id, x: n.position.x, y: n.position.y })),
      )
      try {
        await saveLayout(id, loadedVersion, { nodes: positions })
      } catch {
        // saving the layout isn't critical, ignore silently
      }
    }, 500)
  }

  function validateIdInput(): string | null {
    const trimmed = idInput.trim()
    if (!trimmed) return 'id is required'
    if (trimmed.includes('/')) return 'id must not contain /'
    return null
  }

  async function save() {
    saveError = null
    const idErr = isNew ? validateIdInput() : null
    if (idErr) {
      saveError = idErr
      return
    }
    saving = true
    try {
      const targetId = isNew ? idInput.trim() : id
      const result = isNew
        ? await createPlaybook(targetId, yamlText)
        : await updatePlaybook(targetId, yamlText)
      location.hash = `#/playbook/${result.id}`
    } catch (e) {
      saveError = String(e)
    } finally {
      saving = false
    }
  }
</script>

<header class="topbar">
  <a href={isNew ? '#/' : `#/playbook/${encodeURIComponent(id)}`}>&larr; back</a>
  <h1>{isNew ? 'New playbook' : id}</h1>
  {#if isNew}
    <label class="id-field">
      id
      <input type="text" bind:value={idInput} placeholder="playbook-id" />
    </label>
  {:else}
    <select class="ver-sel" value={loadedVersion} onchange={(e) => loadVersion(e.currentTarget.value)}>
      {#each versions as v}<option value={v}>{v}</option>{/each}
    </select>
    <button class="btn btn-toggle" class:on={showDiff} onclick={() => (showDiff = !showDiff)}>diff</button>
  {/if}
  <span class="spacer"></span>
  {#if loadError}
    <span class="badge err" title={loadError}>load error</span>
  {/if}
  {#if parseError}
    <span class="badge warn" title={parseError}>parse error</span>
  {/if}
  {#if saveError}
    <span class="badge err" title={saveError}>save error</span>
  {/if}
  <button class="btn" onclick={save} disabled={saving || !!loadError}>
    {saving ? 'Saving...' : 'Save'}
  </button>
</header>

<div class="page-split">
  <div class="pane-editor">
    <CodeEditor value={yamlText} onChange={onYamlChange} />
  </div>
  <div class="pane-graph">
    {#if parseError && lastValidModel}
      <div class="parse-hint">Showing last valid graph ({parseError})</div>
    {/if}

    <div class="palette">
      {#each NODE_KINDS as kind}
        <button class="pal-btn" onclick={() => onAddNode(kind)} disabled={!canEditStruct} title={`Add ${kind} node`}>
          + {kind}
        </button>
      {/each}
    </div>

    {#if selectedNode}
      <div class="inspector">
        <NodePanel
          node={selectedNode as PlaybookNodeType}
          onChange={onNodePatch}
          onDelete={onDeleteNode}
          {revision}
        />
      </div>
    {:else if selectedEdge}
      <div class="inspector">
        <div class="edge-panel">
          <strong>edge</strong>
          <span>{selectedEdge.from} -&gt; {selectedEdge.to}</span>
          <button class="btn-del-edge" onclick={onDeleteEdge}>delete</button>
        </div>
      </div>
    {/if}

    {#if showDiff && !isNew}
      <div class="diff-overlay">
        <div class="diff-head">
          <strong>diff</strong>
          <button class="btn-close" onclick={() => (showDiff = false)}>x</button>
        </div>
        <DiffView {id} {versions} />
      </div>
    {/if}

    <SvelteFlow
      bind:nodes
      bind:edges
      {nodeTypes}
      fitView
      nodesDraggable={!showDiff}
      nodesConnectable={!showDiff && canEditStruct}
      elementsSelectable={!showDiff}
      onnodeclick={onNodeClick}
      onedgeclick={onEdgeClick}
      onconnect={onConnect}
      onnodedragstop={onNodeDragStop}
      onpaneclick={onPaneClick}
    >
      <Background />
      <Controls />
    </SvelteFlow>
  </div>
</div>

<style>
  .id-field {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    color: var(--muted);
  }
  .id-field input {
    font: inherit;
    color: var(--fg);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 2px 6px;
    width: 160px;
  }
  .ver-sel {
    font: inherit;
    font-size: 12px;
    color: var(--fg);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 2px 4px;
  }
  .btn {
    font: inherit;
    font-size: 12px;
    padding: 4px 12px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    cursor: pointer;
  }
  .btn:hover:not(:disabled) { border-color: var(--accent); }
  .btn:disabled { opacity: 0.5; cursor: default; }
  .btn-toggle.on { border-color: var(--accent); color: var(--accent); }
  .parse-hint {
    position: absolute;
    top: 8px;
    left: 8px;
    z-index: 5;
    font-size: 12px;
    color: var(--warn);
    background: var(--bg);
    border: 1px solid var(--warn);
    border-radius: 4px;
    padding: 4px 8px;
    max-width: 60%;
  }
  .palette {
    position: absolute;
    top: 8px;
    left: 8px;
    z-index: 6;
    display: flex;
    flex-direction: column;
    gap: 4px;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 4px;
  }
  .pal-btn {
    font: inherit;
    font-size: 11px;
    text-align: left;
    padding: 3px 8px;
    border: 1px solid transparent;
    border-radius: 3px;
    background: transparent;
    color: var(--fg);
    cursor: pointer;
  }
  .pal-btn:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
  .pal-btn:disabled { opacity: 0.4; cursor: default; }
  .inspector {
    position: absolute;
    top: 8px;
    right: 8px;
    z-index: 6;
    width: 220px;
    max-height: calc(100% - 16px);
    overflow: auto;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 8px;
  }
  .edge-panel {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 12px;
  }
  .edge-panel .btn-del-edge {
    font: inherit;
    font-size: 11px;
    padding: 2px 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    cursor: pointer;
    align-self: flex-start;
  }
  .edge-panel .btn-del-edge:hover { border-color: var(--err); color: var(--err); }
  .diff-overlay {
    position: absolute;
    inset: 0;
    z-index: 10;
    background: var(--bg);
    padding: 8px;
    display: flex;
    flex-direction: column;
  }
  .diff-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 6px;
  }
  .btn-close {
    font: inherit;
    font-size: 12px;
    padding: 1px 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    cursor: pointer;
  }
  .btn-close:hover { border-color: var(--err); color: var(--err); }
</style>
