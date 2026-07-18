<script lang="ts">
  import { SvelteFlow, Background, Controls } from '@xyflow/svelte'
  import { untrack } from 'svelte'
  import '@xyflow/svelte/dist/style.css'
  import {
    createPlaybook,
    fetchPlaybook,
    fetchPlaybooks,
    fetchProjects,
    saveLayout,
    updatePlaybook,
  } from '../lib/api'
  import type { Project } from '../lib/types'
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
  import Topbar from '$lib/components/Topbar.svelte'
  import { Button } from '$lib/components/ui/button'
  import { Badge } from '$lib/components/ui/badge'
  import { Input } from '$lib/components/ui/input'
  import { Spinner } from '$lib/components/ui/spinner'
  import * as Select from '$lib/components/ui/select'
  import { toast } from 'svelte-sonner'
  import Plus from '@lucide/svelte/icons/plus'
  import X from '@lucide/svelte/icons/x'
  import GitCompare from '@lucide/svelte/icons/git-compare'

  let { id, workspace = '' }: { id: string; workspace?: string } = $props()

  const isNew = $derived(!id)

  let projects = $state<Project[]>([])
  let targetWorkspace = $state('')

  const NODE_KINDS = ['start', 'agent_task', 'script', 'condition', 'finish'] as const

  let yamlText = $state('')
  let idInput = $state('')
  let debouncedYaml = $state('')
  let lastValidModel = $state<PlaybookModel | null>(null)
  let nodes = $state.raw<FlowNode[]>([])
  let edges = $state.raw<FlowEdge[]>([])
  let parseError = $state<string | null>(null)
  let saving = $state(false)
  let loadFailed = $state(false)

  let loadedVersion = $state('')
  let versions = $state<string[]>([])
  let storedLayout = $state<{ nodes?: { id: string; x: number; y: number }[] } | null>(null)

  let selectedNodeId = $state<string | null>(null)
  let selectedEdge = $state<{ from: string; to: string } | null>(null)
  let showDiff = $state(false)
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
  const projectName = $derived(
    projects.find((p) => p.workspace_id === targetWorkspace)?.name ?? 'select a project',
  )

  function onYamlChange(v: string) {
    yamlText = v
    clearTimeout(debounceTimer)
    debounceTimer = setTimeout(() => {
      debouncedYaml = v
    }, 250)
  }

  async function load() {
    loadFailed = false
    selectedNodeId = null
    selectedEdge = null
    if (isNew) {
      const draft = takeDraftYaml()
      yamlText = draft?.yaml ?? NEW_PLAYBOOK_TEMPLATE
      idInput = draft?.suggestedId ?? ''
      debouncedYaml = yamlText
      revision++
      try {
        projects = await fetchProjects()
        if (!targetWorkspace && projects.length) targetWorkspace = projects[0].workspace_id
      } catch (e) {
        toast.error('Failed to load projects', { description: String(e) })
      }
      return
    }
    try {
      const detail = await fetchPlaybook(id, workspace)
      yamlText = detail.yaml
      idInput = id
      debouncedYaml = yamlText
      loadedVersion = detail.version
      storedLayout = detail.layout
      revision++
      try {
        const all = await fetchPlaybooks()
        const found = all.find((w) => w.id === id && w.workspace_id === workspace)
        versions = found?.versions ?? (detail.version ? [detail.version] : [])
      } catch {
        versions = detail.version ? [detail.version] : []
      }
    } catch (e) {
      loadFailed = true
      toast.error('Failed to load playbook', { description: String(e) })
    }
  }

  async function loadVersion(v: string) {
    if (isNew || !v) return
    try {
      const detail = await fetchPlaybook(id, workspace, v)
      yamlText = detail.yaml
      debouncedYaml = yamlText
      loadedVersion = detail.version
      storedLayout = detail.layout
      selectedNodeId = null
      selectedEdge = null
      revision++
    } catch (e) {
      toast.error('Failed to load version', { description: String(e) })
    }
  }

  $effect(() => {
    id
    workspace
    load()
  })

  $effect(() => {
    if (!debouncedYaml) return
    const { model, error } = parsePlaybook(debouncedYaml)
    if (model) {
      lastValidModel = model
      parseError = null
      const prev = untrack(() => nodes.map((n) => ({ id: n.id, x: n.position.x, y: n.position.y })))
      const stored = (storedLayout?.nodes ?? []).map((n) => ({ id: n.id, x: n.x, y: n.y }))
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
        await saveLayout(id, loadedVersion, { nodes: positions }, workspace)
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
    const idErr = isNew ? validateIdInput() : null
    if (idErr) return toast.error(idErr)
    if (isNew && !targetWorkspace) return toast.error('Select a project')
    saving = true
    try {
      const targetId = isNew ? idInput.trim() : id
      const ws = isNew ? targetWorkspace : workspace
      const result = isNew
        ? await createPlaybook(targetId, yamlText, ws)
        : await updatePlaybook(targetId, yamlText, ws)
      toast.success(isNew ? `Created "${result.id}"` : `Saved "${result.id}"`)
      location.hash = `#/playbook/${encodeURIComponent(ws)}/${encodeURIComponent(result.id)}`
    } catch (e) {
      toast.error('Save failed', { description: String(e) })
    } finally {
      saving = false
    }
  }
</script>

<Topbar active="playbooks">
  {#snippet title()}
    <span class="truncate text-sm font-semibold">{isNew ? 'New playbook' : id}</span>
    {#if isNew}
      <Select.Root type="single" bind:value={targetWorkspace}>
        <Select.Trigger class="h-8 w-40">{projectName}</Select.Trigger>
        <Select.Content>
          <Select.Group>
            {#each projects as p (p.workspace_id)}
              <Select.Item value={p.workspace_id} label={p.name}>{p.name}</Select.Item>
            {/each}
          </Select.Group>
        </Select.Content>
      </Select.Root>
      <Input class="h-8 w-40" bind:value={idInput} placeholder="playbook-id" />
    {:else}
      <Select.Root type="single" value={loadedVersion} onValueChange={(v) => loadVersion(v)}>
        <Select.Trigger class="h-8 w-28">{loadedVersion || 'version'}</Select.Trigger>
        <Select.Content>
          <Select.Group>
            {#each versions as v (v)}<Select.Item value={v} label={v}>{v}</Select.Item>{/each}
          </Select.Group>
        </Select.Content>
      </Select.Root>
      <Button variant={showDiff ? 'default' : 'outline'} size="sm" onclick={() => (showDiff = !showDiff)}>
        <GitCompare data-icon="inline-start" />
        diff
      </Button>
    {/if}
    {#if parseError}
      <Badge variant="outline" class="text-warning" title={parseError}>parse error</Badge>
    {/if}
  {/snippet}
  {#snippet actions()}
    <Button size="sm" onclick={save} disabled={saving || loadFailed}>
      {#if saving}<Spinner data-icon="inline-start" />{/if}
      {saving ? 'Saving...' : 'Save'}
    </Button>
  {/snippet}
</Topbar>

<div class="flex min-h-0 flex-1">
  <div class="flex min-h-0 w-1/2 min-w-0 flex-col border-r border-border">
    <CodeEditor value={yamlText} onChange={onYamlChange} />
  </div>
  <div class="relative min-h-0 min-w-0 flex-1">
    {#if parseError && lastValidModel}
      <div
        class="absolute left-2 top-2 z-[5] max-w-[60%] rounded-md border border-warning bg-background px-2 py-1 text-xs text-warning"
      >
        Showing last valid graph ({parseError})
      </div>
    {/if}

    <div class="absolute left-2 top-2 z-[6] flex flex-col gap-1 rounded-md border border-border bg-background p-1">
      {#each NODE_KINDS as kind (kind)}
        <Button
          variant="ghost"
          size="sm"
          class="h-7 justify-start"
          onclick={() => onAddNode(kind)}
          disabled={!canEditStruct}
          title={`Add ${kind} node`}
        >
          <Plus data-icon="inline-start" />
          {kind}
        </Button>
      {/each}
    </div>

    {#if selectedNode}
      <div
        class="absolute right-2 top-2 z-[6] max-h-[calc(100%-1rem)] w-60 overflow-auto rounded-md border border-border bg-background p-3 shadow-md"
      >
        <NodePanel
          {id}
          node={selectedNode as PlaybookNodeType}
          onChange={onNodePatch}
          onDelete={onDeleteNode}
          {revision}
          workspace={isNew ? targetWorkspace : workspace}
        />
      </div>
    {:else if selectedEdge}
      <div
        class="absolute right-2 top-2 z-[6] flex w-60 flex-col gap-2 rounded-md border border-border bg-background p-3 text-sm shadow-md"
      >
        <strong>edge</strong>
        <span class="font-mono text-xs">{selectedEdge.from} → {selectedEdge.to}</span>
        <Button
          variant="outline"
          size="sm"
          class="self-start text-muted-foreground hover:text-destructive"
          onclick={onDeleteEdge}
        >
          delete
        </Button>
      </div>
    {/if}

    {#if showDiff && !isNew}
      <div class="absolute inset-0 z-10 flex flex-col bg-background p-3">
        <div class="mb-2 flex items-center justify-between">
          <strong class="text-sm">diff</strong>
          <Button variant="ghost" size="icon" class="size-7" onclick={() => (showDiff = false)}>
            <X />
          </Button>
        </div>
        <DiffView {id} {versions} {workspace} />
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
