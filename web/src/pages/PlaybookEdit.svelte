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
  import { toFlow, isUserArranged, type FlowEdge, type FlowNode } from '../lib/graph'
  import NodePanel from '../lib/NodePanel.svelte'
  import PlaybookNode from '../lib/PlaybookNode.svelte'
  import { takeDraftYaml } from '../lib/playbookdupe'
  import { addEdge, addNode, removeEdge, removeNode, suggestNodeId, updateNode } from '../lib/playbookedit'
  import { onEscape } from '../lib/hooks/escape.svelte'
  import { docToString, NEW_PLAYBOOK_TEMPLATE, parseDoc, parsePlaybook } from '../lib/playbookyaml'
  import type { PlaybookModel } from '../lib/playbookyaml'
  import type { PlaybookNode as PlaybookNodeType, WfLayout } from '../lib/types'
  import type { Document } from 'yaml'
  import Topbar from '$lib/components/Topbar.svelte'
  import { Button } from '$lib/components/ui/button'
  import { Badge } from '$lib/components/ui/badge'
  import { Input } from '$lib/components/ui/input'
  import { Spinner } from '$lib/components/ui/spinner'
  import * as Select from '$lib/components/ui/select'
  import { toast } from 'svelte-sonner'
  import Plus from '@lucide/svelte/icons/plus'
  import Trash2 from '@lucide/svelte/icons/trash-2'
  import X from '@lucide/svelte/icons/x'
  import Code from '@lucide/svelte/icons/code'
  import GitCompare from '@lucide/svelte/icons/git-compare'
  import LayoutGrid from '@lucide/svelte/icons/layout-grid'

  let { id, workspace = '' }: { id: string; workspace?: string } = $props()

  const isNew = $derived(!id)

  let projects = $state<Project[]>([])
  let targetWorkspace = $state('')

  // Offered in the order a graph is usually built, with the node that ends a
  // run last.
  const NODE_KINDS = ['start', 'agent_task', 'script', 'condition', 'playbook', 'finish'] as const

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
  let storedLayout = $state<WfLayout | null>(null)

  let selectedNodeId = $state<string | null>(null)
  let selectedEdge = $state<{ from: string; to: string } | null>(null)
  let showDiff = $state(false)
  // The YAML source is the playbook's ground truth, but it is not what you look
  // at while wiring a graph: like `diff`, it takes over the canvas on demand
  // from its topbar button, leaves the topbar reachable, and gives the canvas
  // back when closed.
  let showYaml = $state(false)
  const overlayOpen = $derived(showDiff || showYaml)

  // The two overlays share the canvas, so opening one closes the other.
  function toggleYaml() {
    showYaml = !showYaml
    if (showYaml) showDiff = false
  }

  function toggleDiff() {
    showDiff = !showDiff
    if (showDiff) showYaml = false
  }

  // Escape closes whatever is on top: an overlay first, then the selection
  // behind it (a node form opened under the yaml overlay survives one press
  // and closes on the next).
  onEscape(() => {
    if (showYaml) {
      showYaml = false
    } else if (showDiff) {
      showDiff = false
    } else if (selectedNodeId) {
      selectedNodeId = null
    } else if (selectedEdge) {
      selectedEdge = null
    }
  })
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

  // Reload when the route changes, and ONLY then. `load()` runs untracked
  // because its synchronous prefix touches the editor's own state: on the
  // new-playbook path `revision++` reads and writes `revision` before the first
  // await, which inside a tracking context makes the effect depend on a value
  // it just wrote. That looped: the page remounted continuously (89 fetches of
  // /api/projects and counting), so nothing could be typed, clicked or saved.
  $effect(() => {
    id
    workspace
    untrack(() => load())
  })

  $effect(() => {
    if (!debouncedYaml) return
    const { model, error } = parsePlaybook(debouncedYaml)
    if (model) {
      lastValidModel = model
      parseError = null
      // When the user has arranged nodes by hand, the marker is the explicit
      // guard that stops automatic reflow (this effect, fired on every YAML
      // edit) from clobbering their layout. Stored positions win and only the
      // genuinely new nodes get auto-laid-out by `toFlow`.
      const stored = (storedLayout?.nodes ?? []).map((n) => ({ id: n.id, x: n.x, y: n.y }))
      const layoutInput: WfLayout = isUserArranged(storedLayout)
        ? { nodes: stored, userArranged: true }
        : (() => {
            const prev = untrack(() =>
              nodes.map((n) => ({ id: n.id, x: n.position.x, y: n.position.y })),
            )
            const merged = new Map<string, { id: string; x: number; y: number }>()
            for (const n of stored) merged.set(n.id, n)
            for (const n of prev) merged.set(n.id, n)
            return { nodes: [...merged.values()] }
          })()
      const flow = toFlow(model, layoutInput)
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

  // The add-node selector holds no value: each pick is an action, so it is
  // cleared as soon as it is handled and the same kind can be added twice in a
  // row (bits-ui keeps its own value otherwise, and a repeated pick is then
  // simply not a change).
  let addKind = $state('')
  $effect(() => {
    const kind = addKind
    if (!kind) return
    addKind = ''
    onAddNode(kind as (typeof NODE_KINDS)[number])
  })

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
        // The user moved a node by hand: from now on this layout is their
        // arrangement, and automatic reflow must respect it (see the parse
        // effect guard above). The marker is the only thing that flips that
        // switch, and the only thing allowed to clear it is "Re-layout".
        await saveLayout(id, loadedVersion, { nodes: positions, userArranged: true }, workspace)
      } catch {
        // saving the layout isn't critical, ignore silently
      }
    }, 500)
  }

  // The topbar "Re-layout" button is the SOLE override of the userArranged
  // marker: it discards the stored arrangement, recomputes the auto layout,
  // and persists the result WITHOUT the marker so it is auto again. The
  // button is only available in the loaded-version editor (not the new-
  // playbook path, which has nothing to discard).
  function onRelayout() {
    if (!lastValidModel) return
    const flow = toFlow(lastValidModel, null, undefined, undefined, true)
    nodes = flow.nodes
    edges = flow.edges
    if (isNew || !loadedVersion) return
    clearTimeout(layoutTimer)
    layoutTimer = setTimeout(async () => {
      const positions = untrack(() =>
        nodes.map((n) => ({ id: n.id, x: n.position.x, y: n.position.y })),
      )
      try {
        await saveLayout(id, loadedVersion, { nodes: positions }, workspace)
        // Reflect the cleared marker locally too, so a subsequent YAML edit
        // in the same session takes the auto path again immediately.
        storedLayout = { nodes: positions }
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
      <Button
        variant={showDiff ? 'default' : 'outline'}
        size="sm"
        class="max-sm:px-2"
        onclick={toggleDiff}
      >
        <GitCompare data-icon="inline-start" />
        <span class="max-sm:sr-only">diff</span>
      </Button>
      <Button
        variant="outline"
        size="sm"
        class="max-sm:px-2"
        title="Recompute the automatic layout, discarding any manual arrangement"
        onclick={onRelayout}
      >
        <LayoutGrid data-icon="inline-start" />
        <span class="max-sm:sr-only">re-layout</span>
      </Button>
    {/if}
    <!-- Adding a node is a topbar control, not a stack of buttons parked over
         the canvas. The trigger keeps its own label rather than showing the
         last pick: see `addKind`. -->
    <Select.Root type="single" bind:value={addKind} disabled={!canEditStruct}>
      <Select.Trigger class="h-8 w-36" aria-label="Add node">
        <Plus data-icon="inline-start" />
        Add node
      </Select.Trigger>
      <Select.Content>
        <Select.Group>
          {#each NODE_KINDS as kind (kind)}
            <Select.Item value={kind} label={kind}>{kind}</Select.Item>
          {/each}
        </Select.Group>
      </Select.Content>
    </Select.Root>
    <Button
      variant={showYaml ? 'default' : 'outline'}
      size="sm"
      class="max-sm:px-2"
      onclick={toggleYaml}
    >
      <Code data-icon="inline-start" />
      <span class="max-sm:sr-only">yaml</span>
    </Button>
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
  <div class="relative min-h-0 min-w-0 flex-1">
    {#if parseError && lastValidModel}
      <div
        class="absolute left-2 top-2 z-[5] max-w-[60%] rounded-md border border-warning bg-background px-2 py-1 text-xs text-warning"
      >
        Showing last valid graph ({parseError})
      </div>
    {/if}

    <!-- The node form takes the canvas the way `yaml` and `diff` do: a node has
         a prompt, a profile, connector grants and their function lists, and none
         of that is readable in a 240px rail. Hidden (not unmounted) while
         another overlay is up, so the selection survives a look at the yaml. -->
    {#if selectedNode && !overlayOpen}
      <div class="absolute inset-0 z-10 flex flex-col bg-background p-3">
        <div class="mx-auto mb-2 flex w-full max-w-5xl items-center justify-between gap-2">
          <div class="flex min-w-0 items-center gap-2">
            <strong class="truncate font-mono text-sm">{selectedNode.id}</strong>
            <Badge variant="secondary" class="text-[10px]">{selectedNode.type}</Badge>
            <Button
              variant="ghost"
              size="icon"
              class="size-7 text-muted-foreground hover:text-destructive"
              title="Delete node"
              onclick={onDeleteNode}
            >
              <Trash2 />
            </Button>
          </div>
          <Button
            variant="ghost"
            size="icon"
            class="size-7"
            title="Close"
            onclick={() => (selectedNodeId = null)}
          >
            <X />
          </Button>
        </div>
        <div class="min-h-0 flex-1 overflow-auto">
          <div class="mx-auto w-full max-w-5xl pb-6">
            <NodePanel
              {id}
              node={selectedNode as PlaybookNodeType}
              onChange={onNodePatch}
              {revision}
              workspace={isNew ? targetWorkspace : workspace}
            />
          </div>
        </div>
      </div>
    {:else if selectedEdge && !overlayOpen}
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

    {#if showYaml}
      <div class="absolute inset-0 z-10 flex flex-col bg-background p-3">
        <div class="mb-2 flex items-center justify-between gap-2">
          <div class="flex items-center gap-2">
            <strong class="text-sm">yaml</strong>
            {#if parseError}
              <Badge variant="outline" class="text-warning" title={parseError}>parse error</Badge>
            {/if}
          </div>
          <Button variant="ghost" size="icon" class="size-7" onclick={() => (showYaml = false)}>
            <X />
          </Button>
        </div>
        <div class="flex min-h-0 flex-1 flex-col overflow-hidden rounded-md border border-border">
          <CodeEditor value={yamlText} onChange={onYamlChange} />
        </div>
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
      nodesDraggable={!overlayOpen}
      nodesConnectable={!overlayOpen && canEditStruct}
      elementsSelectable={!overlayOpen}
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
