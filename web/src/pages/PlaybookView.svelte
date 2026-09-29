<script lang="ts">
  import { untrack } from 'svelte'
  import { SvelteFlow, Background, Controls } from '@xyflow/svelte'
  import '@xyflow/svelte/dist/style.css'
  import {
    ApiError,
    fetchPlaybook,
    fetchStats,
    fetchVersions,
    promoteVersion,
    runPlaybook,
    setFrozen,
  } from '../lib/api'
  import * as AlertDialog from '$lib/components/ui/alert-dialog'
  import { toFlow, type FlowEdge, type FlowNode } from '../lib/graph'
  import { subscribeChanges } from '../lib/ws'
  import { onEscape } from '../lib/hooks/escape.svelte'
  import { provenanceLabel } from '../lib/versioninfo'
  import type { VersionInfo, PlaybookNode as PlaybookNodeType } from '../lib/types'
  import CodeEditor from '../lib/CodeEditor.svelte'
  import NodePanel from '../lib/NodePanel.svelte'
  import PlaybookNode from '../lib/PlaybookNode.svelte'
  import Topbar from '$lib/components/Topbar.svelte'
  import { Button } from '$lib/components/ui/button'
  import { Badge } from '$lib/components/ui/badge'
  import { Separator } from '$lib/components/ui/separator'
  import { Spinner } from '$lib/components/ui/spinner'
  import { toast } from 'svelte-sonner'
  import Play from '@lucide/svelte/icons/play'
  import Pencil from '@lucide/svelte/icons/pencil'
  import Snowflake from '@lucide/svelte/icons/snowflake'
  import CircleCheck from '@lucide/svelte/icons/circle-check'
  import TriangleAlert from '@lucide/svelte/icons/triangle-alert'
  import Code from '@lucide/svelte/icons/code'
  import History from '@lucide/svelte/icons/history'
  // --- 0.23.0 stats (C3) ---
  import ChartColumn from '@lucide/svelte/icons/chart-column'
  import PlaybookStatsCard from '../lib/PlaybookStatsCard.svelte'
  // --- end of 0.23.0 stats ---
  import X from '@lucide/svelte/icons/x'

  let { id, workspace = '' }: { id: string; workspace?: string } = $props()

  let nodes = $state.raw<FlowNode[]>([])
  let edges = $state.raw<FlowEdge[]>([])
  let name = $state<string>(untrack(() => id))
  let version = $state<string>('')
  let validation = $state<
    { code: string; severity: string; message: string; node?: string | null }[]
  >([])
  let versions = $state<VersionInfo[]>([])
  let activating = $state<string | null>(null)
  let frozen = $state<boolean>(false)
  let freezing = $state<boolean>(false)
  let starting = $state<boolean>(false)
  // The definition as stored, shown read-only over the canvas: this page never
  // writes a playbook, so the editor here is for reading, not for editing.
  let yamlText = $state<string>('')
  let showYaml = $state(false)
  // Version history is a side trip, not the reason this page exists, so it
  // stays off the canvas until asked for.
  let showHistory = $state(false)
  // --- 0.23.0 stats (C3): cross-run metrics, a side panel like the history.
  let showStats = $state(false)
  let stats = $state<import('../lib/api.gen').StatsReport | null>(null)
  let statsLoading = $state(false)
  let statsError = $state<string | null>(null)

  // Loaded each time the panel opens, so it reflects runs finished since.
  async function loadStats() {
    const key = `${workspace}/${id}`
    statsLoading = true
    statsError = null
    try {
      const r = await fetchStats(id, workspace)
      if (key === `${workspace}/${id}`) stats = r
    } catch (e) {
      statsError = String(e)
    } finally {
      statsLoading = false
    }
  }

  function toggleStats() {
    showStats = !showStats
    if (showStats) loadStats()
  }
  // --- end of 0.23.0 stats ---
  // The playbook's own nodes (not the flow nodes): clicking a node opens the
  // same form the editor uses, read-only, so a prompt can be read here without
  // switching to edit mode.
  let playbookNodes = $state.raw<PlaybookNodeType[]>([])
  let selectedNodeId = $state<string | null>(null)
  const selectedNode = $derived(
    selectedNodeId ? (playbookNodes.find((n) => n.id === selectedNodeId) ?? null) : null,
  )

  // Escape closes whatever is on top, in the order it was put there.
  onEscape(() => {
    if (showYaml) {
      showYaml = false
    } else if (selectedNodeId) {
      selectedNodeId = null
    } else if (showHistory) {
      showHistory = false
    } else if (showStats) {
      showStats = false
    }
  })

  const nodeTypes = { playbookNode: PlaybookNode }

  // Monotonic token: each (re)load bumps it, and an in-flight fetch that
  // resolves after a newer load started is ignored, so a slow response for a
  // previous route cannot overwrite the current playbook (out-of-order loads).
  let loadToken = 0

  const errors = $derived(validation.filter((v) => v.severity === 'error'))
  const issuesTitle = $derived(
    validation
      .map((v) => `${v.severity} ${v.code}: ${v.message}${v.node ? ` (${v.node})` : ''}`)
      .join('\n'),
  )

  // Version history newest-first: the API lists versions oldest first in
  // semver order, so this only reverses it.
  const versionsDesc = $derived([...versions].reverse())

  async function loadVersions(token: number) {
    try {
      const vs = await fetchVersions(id, workspace)
      if (token !== loadToken) return
      versions = vs
    } catch (e) {
      if (token === loadToken) toast.error('Failed to load versions', { description: String(e) })
    }
  }

  async function load(token: number) {
    try {
      const detail = await fetchPlaybook(id, workspace)
      if (token !== loadToken) return
      const flow = toFlow(detail.playbook, detail.layout)
      nodes = flow.nodes
      edges = flow.edges
      playbookNodes = detail.playbook.nodes
      yamlText = detail.yaml
      name = detail.playbook.name || detail.id
      version = detail.version
      validation = detail.validation
      frozen = detail.frozen
    } catch (e) {
      if (token === loadToken) toast.error('Failed to load playbook', { description: String(e) })
    }
    await loadVersions(token)
  }

  const reload = () => load(++loadToken)

  // --- 0.24.0 irreversible consent: the server refuses an irreversible
  // playbook until the person confirms it here; the dialog shows what the
  // refusal names, and only its confirm button sends the consent.
  let irreversibleOpen = $state(false)
  let irreversibleDetail = $state('')

  async function run(confirmIrreversible = false) {
    starting = true
    try {
      const { run_id } = await runPlaybook(id, workspace, confirmIrreversible)
      location.hash = `#/run/${encodeURIComponent(workspace)}/${encodeURIComponent(run_id)}`
    } catch (e) {
      starting = false
      if (e instanceof ApiError && e.code === 'irreversible_requires_confirmation') {
        irreversibleDetail = e.detail ?? ''
        irreversibleOpen = true
        return
      }
      toast.error('Failed to start run', { description: String(e) })
    }
  }

  function confirmIrreversibleRun() {
    irreversibleOpen = false
    void run(true)
  }
  // --- end 0.24.0 irreversible consent ---

  async function toggleFreeze() {
    freezing = true
    try {
      const res = await setFrozen(id, !frozen, workspace)
      frozen = res.frozen
      toast.success(frozen ? 'Playbook frozen' : 'Playbook unfrozen')
    } catch (e) {
      toast.error('Freeze toggle failed', { description: String(e) })
    } finally {
      freezing = false
    }
  }

  // Repoints `current` at the chosen version. Any stored version can be made
  // current, newer or older - the only refusal is a frozen playbook, which the
  // button reflects rather than discovering on click.
  async function useVersion(v: string) {
    activating = v
    try {
      await promoteVersion(id, v, workspace)
      // Awaited: the button stays busy until the refreshed version list is on
      // screen, so the toast and the `current` badge never disagree.
      await reload()
      toast.success(`Now using ${v}`)
    } catch (e) {
      toast.error('Failed to switch version', { description: String(e) })
    } finally {
      activating = null
    }
  }

  $effect(() => {
    // Track the route target so navigating (incl. browser back/forward) between
    // two playbook views reuses this component and still reloads. Clear the
    // route-specific view first so a slow load cannot briefly show the previous
    // playbook's graph or version list.
    void id
    void workspace
    nodes = []
    edges = []
    playbookNodes = []
    selectedNodeId = null
    versions = []
    validation = []
    stats = null
    showStats = false
    reload()
    return subscribeChanges(reload)
  })
</script>

<Topbar active="playbooks">
  {#snippet title()}
    <span class="truncate text-sm font-semibold">{name}</span>
    <Badge variant="outline" class="font-mono text-xs">{version}</Badge>
    {#if frozen}
      <Badge variant="outline" class="gap-1 border-info/30 bg-info/15 text-info">
        <Snowflake class="size-3" />
        frozen
      </Badge>
    {/if}
  {/snippet}
  {#snippet actions()}
    {#if errors.length > 0}
      <Badge
        variant="outline"
        class="gap-1 border-destructive/30 bg-destructive/15 text-destructive"
        title={issuesTitle}
      >
        <TriangleAlert class="size-3" />
        {errors.length} errors
      </Badge>
    {:else if validation.length > 0}
      <Badge
        variant="outline"
        class="gap-1 border-warning/30 bg-warning/15 text-warning"
        title={issuesTitle}
      >
        <TriangleAlert class="size-3" />
        {validation.length} warnings
      </Badge>
    {:else}
      <Badge variant="outline" class="gap-1 border-success/30 bg-success/15 text-success">
        <CircleCheck class="size-3" />
        valid
      </Badge>
    {/if}
    <Button
      variant={showYaml ? 'default' : 'outline'}
      size="sm"
      class="max-sm:px-2"
      onclick={() => (showYaml = !showYaml)}
    >
      <Code data-icon="inline-start" />
      <span class="max-sm:sr-only">yaml</span>
    </Button>
    <Button
      variant={showHistory ? 'default' : 'outline'}
      size="sm"
      class="max-sm:px-2"
      onclick={() => (showHistory = !showHistory)}
      title="Show the version history of this playbook"
    >
      <History data-icon="inline-start" />
      <span class="max-sm:sr-only">History</span>
    </Button>
    <!-- 0.23.0 stats (C3) -->
    <Button
      variant={showStats ? 'default' : 'outline'}
      size="sm"
      class="max-sm:px-2"
      onclick={toggleStats}
      title="Show how the runs of this playbook went, per version"
    >
      <ChartColumn data-icon="inline-start" />
      <span class="max-sm:sr-only">Stats</span>
    </Button>
    <Button
      variant="outline"
      size="sm"
      class="max-sm:px-2"
      onclick={toggleFreeze}
      disabled={freezing}
      title={frozen ? 'Allow changes to this playbook again' : 'Lock this playbook against any definition change'}
    >
      <Snowflake data-icon="inline-start" />
      <span class="max-sm:sr-only">{frozen ? 'Unfreeze' : 'Freeze'}</span>
    </Button>
    <Button
      variant="outline"
      size="sm"
      class="max-sm:px-2 border-warning/50 text-warning hover:border-warning hover:bg-warning/10 hover:text-warning"
      href={`#/edit/${encodeURIComponent(workspace)}/${encodeURIComponent(id)}`}
    >
      <Pencil data-icon="inline-start" />
      <span class="max-sm:sr-only">Edit</span>
    </Button>
    <Button
      size="sm"
      class="max-sm:px-2 bg-success text-success-foreground hover:bg-success/90"
      onclick={() => run()}
      disabled={starting}
      title="Start a run of this playbook"
    >
      {#if starting}<Spinner data-icon="inline-start" />{:else}<Play data-icon="inline-start" />{/if}
      <span class="max-sm:sr-only">{starting ? 'Starting...' : 'Run'}</span>
    </Button>
  {/snippet}
</Topbar>

<div class="flex min-h-0 flex-1">
  <div class="relative min-h-0 min-w-0 flex-1">
    {#if showYaml}
      <div class="absolute inset-0 z-10 flex flex-col bg-background p-3">
        <div class="mb-2 flex items-center justify-between gap-2">
          <div class="flex items-center gap-2">
            <strong class="text-sm">yaml</strong>
            <Badge variant="outline" class="text-muted-foreground">read-only</Badge>
          </div>
          <Button variant="ghost" size="icon" class="size-7" onclick={() => (showYaml = false)}>
            <X />
          </Button>
        </div>
        <div class="flex min-h-0 flex-1 flex-col overflow-hidden rounded-md border border-border">
          <CodeEditor value={yamlText} readonly />
        </div>
      </div>
    {/if}

    <!-- Same form as the editor, read-only: fields keep their text selectable
         and the prompt keeps its scrollbar, but nothing here can change the
         playbook. -->
    {#if selectedNode && !showYaml}
      <div class="absolute inset-0 z-10 flex flex-col bg-background p-3">
        <div class="mx-auto mb-2 flex w-full max-w-5xl items-center justify-between gap-2">
          <div class="flex min-w-0 items-center gap-2">
            <strong class="truncate font-mono text-sm">{selectedNode.id}</strong>
            <Badge variant="secondary" class="text-[10px]">{selectedNode.type}</Badge>
            <Badge variant="outline" class="text-muted-foreground">read-only</Badge>
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
            <NodePanel {id} node={selectedNode} {workspace} nodes={playbookNodes} readonly />
          </div>
        </div>
      </div>
    {/if}

    <SvelteFlow
      bind:nodes
      bind:edges
      {nodeTypes}
      fitView
      nodesDraggable={false}
      nodesConnectable={false}
      elementsSelectable={false}
      onnodeclick={({ node }) => (selectedNodeId = node.id)}
      onpaneclick={() => (selectedNodeId = null)}
    >
      <Background />
      <Controls />
    </SvelteFlow>
  </div>

  <!-- 0.23.0 stats (C3) -->
  {#if showStats}
    <aside class="min-h-0 w-80 shrink-0 overflow-auto border-l border-border p-4" data-testid="playbook-stats-panel">
      <div class="mb-3 flex items-center justify-between gap-2">
        <h2 class="text-sm font-semibold">Run stats</h2>
        <Button variant="ghost" size="icon" class="size-7" title="Hide the run stats" onclick={() => (showStats = false)}>
          <X />
        </Button>
      </div>
      <PlaybookStatsCard report={stats} loading={statsLoading} error={statsError} />
    </aside>
  {/if}

  {#if showHistory}
    <aside class="min-h-0 w-72 shrink-0 overflow-auto border-l border-border p-4">
      <div class="mb-3 flex items-center justify-between gap-2">
        <h2 class="text-sm font-semibold">Version history</h2>
        <Button
          variant="ghost"
          size="icon"
          class="size-7"
          title="Hide version history"
          onclick={() => (showHistory = false)}
        >
          <X />
        </Button>
      </div>
      {#if versions.length === 0}
        <p class="text-sm text-muted-foreground">no versions</p>
      {:else}
        <ul class="flex flex-col">
          {#each versionsDesc as v (v.version)}
            <li class="py-2">
              <div class="flex items-center gap-2">
                <span class="font-mono text-sm" class:font-semibold={v.is_current}>{v.version}</span>
                {#if v.is_current}
                  <Badge variant="secondary" class="text-[10px]">current</Badge>
                {/if}
              </div>
              <div class="mt-0.5 text-xs text-muted-foreground">{provenanceLabel(v)}</div>
              <!-- Any stored version can become the current one, which is what
                   the store allows; the old condition offered the switch only
                   for an unpromoted supervisor patch, so ordinary versions -
                   the newer ones included - had no way back. -->
              {#if !v.is_current}
                <Button
                  variant="outline"
                  size="sm"
                  class="mt-1 h-7"
                  onclick={() => useVersion(v.version)}
                  disabled={activating !== null || frozen}
                  title={frozen
                    ? 'Frozen playbook: unfreeze it to change the current version'
                    : 'Make this version the current one'}
                >
                  {#if activating === v.version}<Spinner data-icon="inline-start" />{/if}
                  Use
                </Button>
              {/if}
              <Separator class="mt-2" />
            </li>
          {/each}
        </ul>
      {/if}
    </aside>
  {/if}
</div>

<!-- 0.24.0 irreversible consent -->
<AlertDialog.Root bind:open={irreversibleOpen}>
  <AlertDialog.Content>
    <AlertDialog.Header>
      <AlertDialog.Title>Run a playbook with irreversible effects?</AlertDialog.Title>
      <AlertDialog.Description>
        {irreversibleDetail ||
          'This playbook declares irreversible effects, such as a push, a merge or a deploy.'}
        Starting it records your consent in the run manifest, and its sub-playbooks inherit it.
      </AlertDialog.Description>
    </AlertDialog.Header>
    <AlertDialog.Footer>
      <AlertDialog.Cancel>Cancel</AlertDialog.Cancel>
      <AlertDialog.Action onclick={confirmIrreversibleRun}>Run it</AlertDialog.Action>
    </AlertDialog.Footer>
  </AlertDialog.Content>
</AlertDialog.Root>
