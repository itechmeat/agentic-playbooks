<script lang="ts">
  import { untrack } from 'svelte'
  import { SvelteFlow, Background, Controls } from '@xyflow/svelte'
  import '@xyflow/svelte/dist/style.css'
  import {
    ApiError,
    fetchFit,
    fetchPlaybook,
    fetchStats,
    fetchVersions,
    promoteVersion,
    runPlaybook,
    setFrozen,
  } from '../lib/api'
  import { toFlow, type FlowEdge, type FlowNode } from '../lib/graph'
  import { subscribeChanges } from '../lib/ws'
  import { onEscape } from '../lib/hooks/escape.svelte'
  import { approvalBadge, approvalHint, provenanceLabel, trialBadge } from '../lib/versioninfo'
  import type {
    PlaybookDetail,
    PlaybookParam,
    VersionInfo,
    PlaybookNode as PlaybookNodeType,
  } from '../lib/types'
  import type { FitReport } from '../lib/api.gen'
  import FitCard from '../lib/FitCard.svelte'
  import ListChecks from '@lucide/svelte/icons/list-checks'
  import CodeEditor from '../lib/CodeEditor.svelte'
  import NodePanel from '../lib/NodePanel.svelte'
  import IrreversibleRunDialog from '../lib/IrreversibleRunDialog.svelte'
  import RunStartDialog from '../lib/RunStartDialog.svelte'
  import type { RunBudget, RunInput } from '../lib/api'
  import { consentStep } from '../lib/irreversibleconsent'
  import PlaybookNode from '../lib/PlaybookNode.svelte'
  import Topbar from '$lib/components/Topbar.svelte'
  import { Button } from '$lib/components/ui/button'
  import { Badge } from '$lib/components/ui/badge'
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

  // "Will it fit here?": read each time the panel opens, since profiles,
  // connectors and trust change outside this page.
  let showFit = $state(false)
  let fit = $state<FitReport | null>(null)
  let fitLoading = $state(false)
  let fitError = $state<string | null>(null)

  // Each load bumps the token; a response for an older load (another
  // playbook, or a toggle since) writes nothing.
  let fitToken = 0
  async function loadFit() {
    const token = ++fitToken
    fitLoading = true
    fitError = null
    try {
      const r = await fetchFit(id, workspace)
      if (token === fitToken) fit = r
    } catch (e) {
      if (token === fitToken) fitError = String(e)
    } finally {
      if (token === fitToken) fitLoading = false
    }
  }

  function toggleFit() {
    showFit = !showFit
    if (showFit) loadFit()
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
    } else if (showFit) {
      showFit = false
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
      budgetBlock = detail.playbook.budget ?? null
      runParams = detail.playbook.params ?? []
    } catch (e) {
      if (token === loadToken) toast.error('Failed to load playbook', { description: String(e) })
    }
    await loadVersions(token)
  }

  const reload = () => load(++loadToken)

  // --- 0.24.0 irreversible consent: the server refuses an irreversible
  // playbook until the person confirms it here; the dialog lists the
  // refusal's structured sources, and only its confirm button sends the
  // refusal's consent nonce, which binds the consent to what it showed.
  let irreversibleOpen = $state(false)
  let irreversibleSources = $state<string[]>([])
  let irreversibleNonce = $state('')
  // The Run dialog (estimate and budget) opens first; the budget it returns
  // rides every retry of the same start, the consent one included.
  let startOpen = $state(false)
  let budgetBlock = $state<PlaybookDetail['playbook']['budget']>(null)
  let startBudget = $state<RunBudget | undefined>(undefined)
  // The params and instruction the dialog collected, and whether to keep
  // them for this project; they ride every retry of the same start.
  let runParams = $state<PlaybookParam[]>([])
  let startInput = $state<RunInput>({})
  // The dialog reopened after a `budget_needs_decision` refusal: it keeps
  // the typed values and shows the pause or continue choice.
  let startRetry = $state(false)

  function openStart() {
    startRetry = false
    startOpen = true
  }

  function startWith(budget: RunBudget | undefined, input: RunInput) {
    startBudget = budget
    startInput = input
    void run()
  }

  async function run(consentNonce?: string) {
    starting = true
    try {
      const started = await runPlaybook(id, workspace, {
        consentNonce,
        budget: startBudget,
        input: startInput,
      })
      const { run_id, warnings } = started
      for (const w of warnings ?? []) toast.warning(w)
      for (const r of started.not_remembered ?? []) {
        toast.warning(`Not remembered: ${r.name} (${r.reason.replace(/_/g, ' ')})`)
      }
      if (started.memory_error) toast.warning('The input was not remembered', { description: started.memory_error })
      location.hash = `#/run/${encodeURIComponent(workspace)}/${encodeURIComponent(run_id)}`
    } catch (e) {
      starting = false
      if (e instanceof ApiError && e.code === 'irreversible_requires_confirmation') {
        const step = consentStep(e.body, consentNonce)
        if (step.kind === 'stop') {
          toast.error('Run refused', { description: step.message })
          return
        }
        irreversibleSources = step.sources
        irreversibleNonce = step.nonce
        irreversibleOpen = true
        return
      }
      if (e instanceof ApiError && e.code === 'budget_needs_decision') {
        // Show the dialog again, the typed values kept and the choice shown.
        toast.error('The budget needs a decision', {
          description: 'Some nodes have no estimate: choose pause or continue.',
        })
        startRetry = true
        startOpen = true
        return
      }
      toast.error('Failed to start run', { description: String(e) })
    }
  }

  function confirmIrreversibleRun() {
    void run(irreversibleNonce)
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
    fit = null
    fitError = null
    fitLoading = false
    fitToken++
    showFit = false
    reload()
    return subscribeChanges(reload)
  })
</script>

<Topbar active="playbooks">
  {#snippet title()}
    <span class="pv-title">
      <span class="pv-name">{name}</span>
      <span class="pv-chip mono">{version}</span>
      {#if frozen}
        <span class="pv-chip info">
          <Snowflake class="pv-chip-icon" />
          frozen
        </span>
      {/if}
    </span>
  {/snippet}
  {#snippet actions()}
    <span class="pv-tools">
      {#if errors.length > 0}
        <span class="pv-chip danger" title={issuesTitle}>
          <TriangleAlert class="pv-chip-icon" />
          {errors.length} errors
        </span>
      {:else if validation.length > 0}
        <span class="pv-chip warn" title={issuesTitle}>
          <TriangleAlert class="pv-chip-icon" />
          {validation.length} warnings
        </span>
      {:else}
        <span class="pv-chip ok">
          <CircleCheck class="pv-chip-icon" />
          valid
        </span>
      {/if}
      <Button variant={showYaml ? 'default' : 'outline'} size="sm" class="pv-tool" onclick={() => (showYaml = !showYaml)}>
        <Code data-icon="inline-start" />
        <span class="pv-label">yaml</span>
      </Button>
      <Button
        variant={showHistory ? 'default' : 'outline'}
        size="sm"
        class="pv-tool"
        onclick={() => (showHistory = !showHistory)}
        title="Show the version history of this playbook"
      >
        <History data-icon="inline-start" />
        <span class="pv-label">History</span>
      </Button>
      <!-- 0.23.0 stats (C3) -->
      <Button
        variant={showStats ? 'default' : 'outline'}
        size="sm"
        class="pv-tool"
        onclick={toggleStats}
        title="Show how the runs of this playbook went, per version"
      >
        <ChartColumn data-icon="inline-start" />
        <span class="pv-label">Stats</span>
      </Button>
      <Button
        variant={showFit ? 'default' : 'outline'}
        size="sm"
        class="pv-tool"
        onclick={toggleFit}
        title="Will this playbook run in this project: compatibility, setup, trust and consent"
        data-testid="fit-toggle"
      >
        <ListChecks data-icon="inline-start" />
        <span class="pv-label">Fit</span>
      </Button>
      <Button
        variant="outline"
        size="sm"
        class="pv-tool"
        onclick={toggleFreeze}
        disabled={freezing}
        title={frozen ? 'Allow changes to this playbook again' : 'Lock this playbook against any definition change'}
      >
        <Snowflake data-icon="inline-start" />
        <span class="pv-label">{frozen ? 'Unfreeze' : 'Freeze'}</span>
      </Button>
      <Button
        variant="outline"
        size="sm"
        class="pv-tool pv-edit"
        href={`#/edit/${encodeURIComponent(workspace)}/${encodeURIComponent(id)}`}
      >
        <Pencil data-icon="inline-start" />
        <span class="pv-label">Edit</span>
      </Button>
      <Button
        size="sm"
        class="pv-tool pv-run"
        onclick={openStart}
        disabled={starting}
        title="Start a run of this playbook"
      >
        {#if starting}<Spinner data-icon="inline-start" />{:else}<Play data-icon="inline-start" />{/if}
        <span class="pv-label">{starting ? 'Starting...' : 'Run'}</span>
      </Button>
    </span>
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

  {#if showFit}
    <aside class="side-panel fit-panel" data-testid="fit-panel">
      <div class="side-head">
        <h2>Will it fit here?</h2>
        <Button variant="ghost" size="icon" title="Hide the fit report" onclick={() => (showFit = false)}>
          <X />
        </Button>
      </div>
      <FitCard report={fit} loading={fitLoading} error={fitError} />
    </aside>
  {/if}

  {#if showHistory}
    <aside class="side-panel history-panel">
      <div class="side-head">
        <h2>Version history</h2>
        <Button variant="ghost" size="icon" title="Hide version history" onclick={() => (showHistory = false)}>
          <X />
        </Button>
      </div>
      {#if versions.length === 0}
        <p class="muted">no versions</p>
      {:else}
        <ul class="versions">
          {#each versionsDesc as v (v.version)}
            <li>
              <div class="version-row">
                <span class="version-name" class:current={v.is_current}>{v.version}</span>
                {#if v.is_current}
                  <Badge variant="secondary">current</Badge>
                {/if}
                {#if trialBadge(v)}
                  <Badge variant="outline" title={v.provenance?.trial?.reason ?? ''}>
                    {trialBadge(v)}
                  </Badge>
                {/if}
                {#if approvalBadge(v)}
                  <Badge
                    variant="outline"
                    title={approvalHint(id, v.version)}
                    data-testid="version-awaiting-approval"
                  >
                    {approvalBadge(v)}
                  </Badge>
                {/if}
              </div>
              <div class="muted provenance">{provenanceLabel(v)}</div>
              <!-- Any stored version can become the current one, which is what
                   the store allows; the old condition offered the switch only
                   for an unpromoted supervisor patch, so ordinary versions -
                   the newer ones included - had no way back. -->
              {#if !v.is_current}
                <Button
                  variant="outline"
                  size="sm"
                  class="use-version"
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
            </li>
          {/each}
        </ul>
      {/if}
    </aside>
  {/if}
</div>

<RunStartDialog
  bind:open={startOpen}
  playbookId={id}
  {workspace}
  block={budgetBlock}
  params={runParams}
  busy={starting}
  retry={startRetry}
  onrun={startWith}
/>

<!-- 0.24.0 irreversible consent -->
<IrreversibleRunDialog
  bind:open={irreversibleOpen}
  playbookId={id}
  sources={irreversibleSources}
  onconfirm={confirmIrreversibleRun}
/>

<style>
  /* Plain CSS over the app's variables for the side panels. */
  .side-panel {
    min-height: 0;
    flex-shrink: 0;
    overflow: auto;
    border-left: 1px solid var(--border);
    padding: 16px;
  }
  .history-panel {
    width: 18rem;
  }
  .fit-panel {
    width: 22rem;
  }
  /* The toolbar: plain CSS over the app's variables. It wraps with the bar
     (see Topbar), and on a phone the buttons keep only their icons. */
  .pv-title,
  .pv-tools {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .pv-tools {
    justify-content: flex-end;
  }
  .pv-name {
    min-width: 0;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 14px;
    font-weight: 600;
  }
  .pv-chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 1px 8px;
    border: 1px solid var(--border);
    border-radius: 999px;
    font-size: 12px;
    font-weight: 500;
    white-space: nowrap;
  }
  .pv-chip.mono {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .pv-chip :global(.pv-chip-icon) {
    width: 12px;
    height: 12px;
  }
  .pv-chip.info {
    color: var(--info);
    border-color: color-mix(in oklab, var(--info) 30%, transparent);
    background: color-mix(in oklab, var(--info) 15%, transparent);
  }
  .pv-chip.ok {
    color: var(--success);
    border-color: color-mix(in oklab, var(--success) 30%, transparent);
    background: color-mix(in oklab, var(--success) 15%, transparent);
  }
  .pv-chip.warn {
    color: var(--warning);
    border-color: color-mix(in oklab, var(--warning) 30%, transparent);
    background: color-mix(in oklab, var(--warning) 15%, transparent);
  }
  .pv-chip.danger {
    color: var(--destructive);
    border-color: color-mix(in oklab, var(--destructive) 30%, transparent);
    background: color-mix(in oklab, var(--destructive) 15%, transparent);
  }
  .pv-tools :global(.pv-edit) {
    border-color: color-mix(in oklab, var(--warning) 50%, transparent);
    color: var(--warning);
  }
  .pv-tools :global(.pv-edit:hover) {
    border-color: var(--warning);
    background: color-mix(in oklab, var(--warning) 10%, transparent);
  }
  .pv-tools :global(.pv-run) {
    background: var(--success);
    color: var(--success-foreground);
  }
  .pv-tools :global(.pv-run:hover) {
    background: color-mix(in oklab, var(--success) 90%, transparent);
  }
  @media (max-width: 640px) {
    .history-panel,
    .fit-panel {
      width: min(22rem, 85vw);
    }
    .pv-tools :global(.pv-tool) {
      padding-inline: 8px;
    }
    .pv-label {
      position: absolute;
      width: 1px;
      height: 1px;
      overflow: hidden;
      clip-path: inset(50%);
      white-space: nowrap;
    }
  }
  .side-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 12px;
  }
  .side-head h2 {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
  }
  .muted {
    color: var(--muted-foreground);
    font-size: 12px;
  }
  .versions {
    display: flex;
    flex-direction: column;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .versions li {
    padding: 8px 0;
    border-bottom: 1px solid var(--border);
  }
  .version-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }
  .version-name {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 14px;
  }
  .version-name.current {
    font-weight: 600;
  }
  .provenance {
    margin-top: 2px;
    overflow-wrap: anywhere;
  }
  .versions :global(.use-version) {
    margin-top: 4px;
    height: 28px;
  }
</style>
