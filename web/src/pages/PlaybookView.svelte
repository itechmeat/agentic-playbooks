<script lang="ts">
  import { SvelteFlow, Background, Controls } from '@xyflow/svelte'
  import '@xyflow/svelte/dist/style.css'
  import { fetchPlaybook, fetchVersions, promoteVersion, runPlaybook, setFrozen } from '../lib/api'
  import { toFlow, type FlowEdge, type FlowNode } from '../lib/graph'
  import { subscribeChanges } from '../lib/ws'
  import { provenanceLabel } from '../lib/versioninfo'
  import type { VersionInfo } from '../lib/types'
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

  let { id, workspace = '' }: { id: string; workspace?: string } = $props()

  let nodes = $state.raw<FlowNode[]>([])
  let edges = $state.raw<FlowEdge[]>([])
  let name = $state<string>(id)
  let version = $state<string>('')
  let validation = $state<
    { code: string; severity: string; message: string; node?: string | null }[]
  >([])
  let versions = $state<VersionInfo[]>([])
  let promoting = $state<string | null>(null)
  let frozen = $state<boolean>(false)
  let freezing = $state<boolean>(false)
  let starting = $state<boolean>(false)

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

  // Version history newest-first.
  function cmpVersionDesc(a: string, b: string): number {
    const pa = a.split('.').map((n) => parseInt(n, 10) || 0)
    const pb = b.split('.').map((n) => parseInt(n, 10) || 0)
    for (let i = 0; i < 3; i++) if ((pb[i] ?? 0) !== (pa[i] ?? 0)) return (pb[i] ?? 0) - (pa[i] ?? 0)
    return 0
  }
  const versionsDesc = $derived([...versions].sort((a, b) => cmpVersionDesc(a.version, b.version)))

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

  async function run() {
    starting = true
    try {
      const { run_id } = await runPlaybook(id, workspace)
      location.hash = `#/run/${encodeURIComponent(workspace)}/${encodeURIComponent(run_id)}`
    } catch (e) {
      toast.error('Failed to start run', { description: String(e) })
      starting = false
    }
  }

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

  async function promote(v: string) {
    promoting = v
    try {
      await promoteVersion(id, v, workspace)
      reload()
      toast.success(`Promoted ${v} to current`)
    } catch (e) {
      toast.error('Promote failed', { description: String(e) })
    } finally {
      promoting = null
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
    versions = []
    validation = []
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
      onclick={run}
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
    <SvelteFlow
      bind:nodes
      bind:edges
      {nodeTypes}
      fitView
      nodesDraggable={false}
      nodesConnectable={false}
      elementsSelectable={false}
    >
      <Background />
      <Controls />
    </SvelteFlow>
  </div>

  <aside class="min-h-0 w-72 shrink-0 overflow-auto border-l border-border p-4">
    <h2 class="mb-3 text-sm font-semibold">Version history</h2>
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
            {#if v.provenance && !v.provenance.promoted}
              <Button
                variant="outline"
                size="sm"
                class="mt-1 h-7"
                onclick={() => promote(v.version)}
                disabled={promoting === v.version}
              >
                {#if promoting === v.version}<Spinner data-icon="inline-start" />{/if}
                Promote
              </Button>
            {/if}
            <Separator class="mt-2" />
          </li>
        {/each}
      </ul>
    {/if}
  </aside>
</div>
