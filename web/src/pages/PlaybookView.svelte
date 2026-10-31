<script lang="ts">
  import { untrack } from 'svelte'
  import { SvelteFlow, Background, Controls } from '@xyflow/svelte'
  import '@xyflow/svelte/dist/style.css'
  import {
    ApiError,
    fetchPlaybook,
    fetchProfiles,
    fetchProjects,
    fetchStats,
    fetchVersions,
    promoteVersion,
    runPlaybook,
    setFrozen,
    type ProfileSummary,
  } from '../lib/api'
  import {
    profileModelMap,
    toFlow,
    withProfileModels,
    type FlowEdge,
    type FlowNode,
  } from '../lib/graph'
  import { accentVar, profileColor } from '../lib/profilecolor'
  import { slowRefresh, subscribeChanges } from '../lib/ws'
  import { shortModel } from '../lib/format'
  import { onEscape } from '../lib/hooks/escape.svelte'
  import { provenanceLabel, trialBadge } from '../lib/versioninfo'
  import type {
    PlaybookDetail,
    PlaybookEdge as PlaybookEdgeType,
    VersionInfo,
    PlaybookNode as PlaybookNodeType,
  } from '../lib/types'
  import CodeEditor from '../lib/CodeEditor.svelte'
  import NodePanel from '../lib/NodePanel.svelte'
  import IrreversibleRunDialog from '../lib/IrreversibleRunDialog.svelte'
  import { consentStep } from '../lib/irreversibleconsent'
  import PlaybookNode from '../lib/PlaybookNode.svelte'
  import PlaybookEdge from '../lib/PlaybookEdge.svelte'
  import NodeSummary from '../lib/NodeSummary.svelte'
  import FitTop from '../lib/FitTop.svelte'
  import Topbar from '$lib/components/Topbar.svelte'
  import StatusBar, { type StatusItem } from '$lib/components/StatusBar.svelte'
  import { Button } from '$lib/components/ui/button'
  import { Badge } from '$lib/components/ui/badge'
  import { Spinner } from '$lib/components/ui/spinner'
  import { toast } from 'svelte-sonner'
  import Play from '@lucide/svelte/icons/play'
  import Pencil from '@lucide/svelte/icons/pencil'
  import Snowflake from '@lucide/svelte/icons/snowflake'
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
  let playbookEdges = $state.raw<PlaybookEdgeType[]>([])
  // The rest of the definition the side boxes show (description, effects,
  // requires, trigger, defaults); the server sends the whole parsed playbook.
  let meta = $state.raw<PlaybookMeta>({})
  let selectedNodeId = $state<string | null>(null)
  const selectedNode = $derived(
    selectedNodeId ? (playbookNodes.find((n) => n.id === selectedNodeId) ?? null) : null,
  )
  const selectedData = $derived(
    selectedNodeId ? (nodes.find((n) => n.id === selectedNodeId)?.data ?? null) : null,
  )
  // The full read-only form of the selected node, over the canvas. The side
  // summary is what a click shows; the form is one more click away.
  let showForm = $state(false)

  // Profile listing (all projects), for each profile's model and agent; the
  // recorded run count for the status bar; the project name.
  let profileList = $state.raw<ProfileSummary[]>([])
  let runsCount = $state<number | null>(null)
  let projectName = $state('')

  interface PlaybookMeta {
    description?: string
    effects?: string[]
    requires?: { files?: string[]; commands?: string[] } | null
    trigger?: { when?: string[]; avoid_when?: string[] } | null
    defaults?: { max_parallel?: number | null; on_failure?: string | null } | null
  }

  function metaOf(pb: PlaybookDetail['playbook']): PlaybookMeta {
    const raw = pb as unknown as Record<string, unknown>
    const list = (v: unknown) => (Array.isArray(v) ? v.map(String) : [])
    const obj = (v: unknown) => (v && typeof v === 'object' ? (v as Record<string, unknown>) : null)
    const req = obj(raw.requires)
    const trig = obj(raw.trigger)
    const def = obj(raw.defaults)
    return {
      description: typeof raw.description === 'string' ? raw.description.trim() : '',
      effects: list(raw.effects),
      requires: req ? { files: list(req.files), commands: list(req.commands) } : null,
      trigger: trig ? { when: list(trig.when), avoid_when: list(trig.avoid_when) } : null,
      defaults: def
        ? {
            max_parallel: typeof def.max_parallel === 'number' ? def.max_parallel : null,
            on_failure: typeof def.on_failure === 'string' ? def.on_failure : null,
          }
        : null,
    }
  }

  // YAML block text keeps its hard line breaks; only blank lines split it.
  const descriptionParas = $derived(
    (meta.description ?? '')
      .split(/\n\s*\n/)
      .map((p) => p.replace(/\s*\n\s*/g, ' ').trim())
      .filter(Boolean),
  )

  // Profile name -> model, resolved the way scope `auto` does for this project.
  const models = $derived(profileModelMap(profileList, workspace))
  // Each profile the graph uses, in first-use order, for the legend.
  const usedProfiles = $derived.by(() => {
    const seen: string[] = []
    for (const n of nodes) {
      const p = n.data.profile
      if (p && n.data.kind === 'agent_task' && !seen.includes(p)) seen.push(p)
    }
    return seen
  })
  const hasJudge = $derived(nodes.some((n) => n.data.kind === 'judge'))
  const hasHumanGate = $derived(nodes.some((n) => n.data.kind === 'human_review'))
  const agentTasks = $derived(nodes.filter((n) => n.data.kind === 'agent_task').length)
  const resolvedProfiles = $derived(usedProfiles.filter((p) => models.has(p)).length)
  // The profile a name resolves to here, the way scope `auto` does: this
  // project's own first, else the global one.
  const resolveProfile = (p: string) =>
    profileList.find((s) => s.name === p && s.scope === 'project' && s.workspace_id === workspace) ??
    profileList.find((s) => s.name === p && s.scope === 'global')
  const untrustedProfiles = $derived(
    usedProfiles.filter((p) => {
      const r = resolveProfile(p)
      return !!r && !r.trusted
    }),
  )
  const selectedAgent = $derived.by(() => {
    const p = selectedData?.profile
    return p ? (resolveProfile(p)?.agent ?? '') : ''
  })

  // Select a node on the canvas: the card glows and the side shows it.
  function select(nodeId: string | null) {
    selectedNodeId = nodeId
    showForm = false
    nodes = nodes.map((n) => (n.selected === (n.id === nodeId) ? n : { ...n, selected: n.id === nodeId }))
  }

  // Effects in their colors: what a run may touch, the riskier the warmer.
  const effectTone: Record<string, string> = {
    fs_read: 'text-fg-2',
    fs_write: 'text-orange',
    network: 'text-blue',
    external: 'text-red',
    secrets: 'text-pink',
    irreversible: 'text-red',
  }

  // Escape closes whatever is on top, in the order it was put there.
  onEscape(() => {
    if (showYaml) {
      showYaml = false
    } else if (showForm) {
      showForm = false
    } else if (selectedNodeId) {
      select(null)
    } else if (showHistory) {
      showHistory = false
    } else if (showStats) {
      showStats = false
    }
  })

  const nodeTypes = { playbookNode: PlaybookNode }

  // Every edge `toFlow` builds has no type, so it takes the default one.
  const edgeTypes = { default: PlaybookEdge }

  // Monotonic token: each (re)load bumps it, and an in-flight fetch that
  // resolves after a newer load started is ignored, so a slow response for a
  // previous route cannot overwrite the current playbook (out-of-order loads).
  let loadToken = 0

  const errors = $derived(validation.filter((v) => v.severity === 'error'))
  const warnings = $derived(validation.length - errors.length)
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
      nodes = withProfileModels(flow.nodes, models).map((n) =>
        n.id === selectedNodeId ? { ...n, selected: true } : n,
      )
      edges = flow.edges
      playbookNodes = detail.playbook.nodes
      playbookEdges = detail.playbook.edges
      meta = metaOf(detail.playbook)
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

  // The side facts: none of them is needed to draw the graph, so a failure
  // here only leaves a box or a status bar item empty. Profiles and projects
  // load once per route, not on every change; the profile listing is the same
  // for every playbook, so it is kept even when the route moved on meanwhile.
  let routeToken = 0
  async function loadRouteSide(route: number) {
    const [profiles, projects] = await Promise.allSettled([fetchProfiles(), fetchProjects()])
    if (profiles.status === 'fulfilled') profileList = profiles.value
    if (route !== routeToken) return
    projectName =
      projects.status === 'fulfilled'
        ? (projects.value.find((p) => p.workspace_id === workspace)?.name ?? '')
        : ''
  }

  // The recorded runs of this playbook for the status bar. The stats report
  // is the cheap source (the run listing walks every project), and it still
  // reads journals, so it follows changes on the slow lane.
  async function loadRunsCount(route: number) {
    try {
      const r = await fetchStats(id, workspace)
      if (route === routeToken) runsCount = r.runs
    } catch {
      if (route === routeToken) runsCount = null
    }
  }

  // The cards show each profile's model: fill it in again whenever the
  // listing (and so the resolution) changes, on the nodes already drawn.
  $effect(() => {
    const m = models
    untrack(() => {
      if (nodes.length) nodes = withProfileModels(nodes, m)
    })
  })

  const statusItems = $derived.by((): StatusItem[] => {
    const items: StatusItem[] = [
      { label: 'playbook', value: version ? `${id}@${version}` : id, tone: 'orange' },
      errors.length > 0
        ? { label: 'validate', value: `${errors.length} errors`, tone: 'danger', title: issuesTitle }
        : {
            label: 'validate',
            value: `ok · ${warnings} ${warnings === 1 ? 'warning' : 'warnings'}`,
            tone: warnings > 0 ? 'warning' : 'success',
            title: issuesTitle || undefined,
          },
      {
        label: 'profiles',
        value: `${resolvedProfiles}/${usedProfiles.length} resolved`,
        tone: resolvedProfiles < usedProfiles.length ? 'warning' : 'violet',
      },
    ]
    if (runsCount !== null) items.push({ label: 'runs', value: runsCount, tone: 'info' })
    if (frozen) items.push({ label: 'frozen', value: 'yes', tone: 'info' })
    return items
  })

  const reload = () => load(++loadToken)

  // --- 0.24.0 irreversible consent: the server refuses an irreversible
  // playbook until the person confirms it here; the dialog lists the
  // refusal's structured sources, and only its confirm button sends the
  // refusal's consent nonce, which binds the consent to what it showed.
  let irreversibleOpen = $state(false)
  let irreversibleSources = $state<string[]>([])
  let irreversibleNonce = $state('')

  async function run(consentNonce?: string) {
    starting = true
    try {
      const { run_id } = await runPlaybook(id, workspace, consentNonce)
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
    playbookEdges = []
    meta = {}
    runsCount = null
    selectedNodeId = null
    showForm = false
    versions = []
    validation = []
    stats = null
    showStats = false
    projectName = ''
    const route = ++routeToken
    void loadRouteSide(route)
    const runsLane = slowRefresh(() => void loadRunsCount(route), 30_000)
    runsLane.request()
    reload()
    const unsubscribe = subscribeChanges(() => {
      reload()
      runsLane.request()
    })
    return () => {
      unsubscribe()
      runsLane.dispose()
    }
  })
</script>

<Topbar active="playbooks" project={projectName} />

<!-- Legend: one square per profile this playbook runs, then the judge and the
     human gate when it has them. -->
<div
  class="flex shrink-0 flex-wrap items-center gap-x-5 gap-y-1 border-b border-line px-4 py-1.5 text-xs text-fg-2"
  data-testid="playbook-legend"
>
  {#each usedProfiles as p (p)}
    <span class="legend-sq whitespace-nowrap" style:--k={profileColor(p)}>
      {p}{#if models.get(p)}<span class="text-muted-foreground" title={models.get(p)}
          >{` · ${shortModel(models.get(p) ?? '')}`}</span
        >{/if}
    </span>
  {/each}
  {#if usedProfiles.length && (hasJudge || hasHumanGate)}
    <span class="text-faint" aria-hidden="true">│</span>
  {/if}
  {#if hasJudge}
    <span class="legend-sq whitespace-nowrap" style:--k={accentVar('green')}>judge · decides</span>
  {/if}
  {#if hasHumanGate}
    <span class="legend-sq whitespace-nowrap" style:--k={accentVar('amber')}>human gate</span>
  {/if}
  {#if !usedProfiles.length && !hasJudge && !hasHumanGate}
    <span class="text-muted-foreground">no agent profiles</span>
  {/if}
</div>

<div class="min-h-0 flex-1 overflow-auto lg:overflow-hidden">
  <div
    class="grid gap-4 p-4 lg:h-full lg:grid-cols-[300px_minmax(0,1fr)_340px] xl:grid-cols-[330px_minmax(0,1fr)_360px]"
  >
    <!-- Left: what this playbook is, its state, what it touches, when to use it. -->
    <div class="flex min-w-0 flex-col gap-[18px] pt-2.5 lg:min-h-0 lg:overflow-auto">
      <section class="box px-4 pt-3.5 pb-4" style:--k={accentVar('orange')}>
        <span class="box-cap">playbook</span>
        <div class="flex flex-wrap items-baseline gap-x-1.5">
          <span class="font-bold break-all text-orange">{id}</span>
          {#if version}<span class="text-muted-foreground">· {version}</span>{/if}
          {#if frozen}
            <span class="tag ml-1 inline-flex items-center gap-1 text-info">
              <Snowflake class="size-3" />
              frozen
            </span>
          {/if}
        </div>
        <h1 class="mt-0.5 mb-2 font-semibold">{name}</h1>
        {#each descriptionParas as para, i (i)}
          <p class="prose-text mb-1.5 text-xs leading-relaxed text-fg-2">{para}</p>
        {/each}
        <div class="mt-3 flex flex-wrap gap-2">
          <Button
            size="sm"
            class="h-7"
            onclick={() => run()}
            disabled={starting}
            title="Start a run of this playbook"
          >
            {#if starting}<Spinner data-icon="inline-start" />{:else}<Play data-icon="inline-start" />{/if}
            {starting ? 'Starting...' : 'Run'}
          </Button>
          <Button
            variant="outline"
            size="sm"
            class="h-7"
            href={`#/edit/${encodeURIComponent(workspace)}/${encodeURIComponent(id)}`}
          >
            <Pencil data-icon="inline-start" />
            Edit
          </Button>
          <Button
            variant={showHistory ? 'default' : 'outline'}
            size="sm"
            class="h-7"
            onclick={() => (showHistory = !showHistory)}
            title="Show the version history of this playbook"
          >
            <History data-icon="inline-start" />
            History
          </Button>
          <Button
            variant={showYaml ? 'default' : 'outline'}
            size="sm"
            class="h-7"
            onclick={() => (showYaml = !showYaml)}
          >
            <Code data-icon="inline-start" />
            yaml
          </Button>
          <!-- 0.23.0 stats (C3) -->
          <Button
            variant={showStats ? 'default' : 'outline'}
            size="sm"
            class="h-7"
            onclick={toggleStats}
            title="Show how the runs of this playbook went, per version"
          >
            <ChartColumn data-icon="inline-start" />
            Stats
          </Button>
          <Button
            variant="outline"
            size="sm"
            class="h-7"
            onclick={toggleFreeze}
            disabled={freezing}
            title={frozen ? 'Allow changes to this playbook again' : 'Lock this playbook against any definition change'}
          >
            <Snowflake data-icon="inline-start" />
            {frozen ? 'Unfreeze' : 'Freeze'}
          </Button>
        </div>
      </section>

      <section class="box px-4 pt-3.5 pb-3.5">
        <span class="box-cap">status</span>
        <dl class="kv">
          <dt>validate</dt>
          {#if errors.length > 0}
            <dd class="text-red" title={issuesTitle}>{errors.length} errors</dd>
          {:else if validation.length > 0}
            <dd class="text-amber" title={issuesTitle}>{validation.length} warnings</dd>
          {:else}
            <dd class="text-green">valid</dd>
          {/if}
          <dt>lock</dt>
          <dd class={frozen ? 'text-info' : 'text-muted-foreground'}>{frozen ? 'frozen' : 'open'}</dd>
          <dt>nodes</dt>
          <dd>{nodes.length} · {agentTasks} agent {agentTasks === 1 ? 'task' : 'tasks'}</dd>
          <dt>profiles</dt>
          <dd class={untrustedProfiles.length ? 'text-amber' : ''} title={untrustedProfiles.join(', ')}>
            {usedProfiles.length}{untrustedProfiles.length ? ` · ${untrustedProfiles.length} untrusted` : ''}
          </dd>
          <dt>max parallel</dt>
          <dd class={meta.defaults?.max_parallel ? '' : 'text-muted-foreground'}>{meta.defaults?.max_parallel ?? 'default'}</dd>
          <dt>on failure</dt>
          <dd>{meta.defaults?.on_failure ?? 'route'}</dd>
          <dt>versions</dt>
          <dd>{versions.length || '-'}</dd>
          {#if runsCount !== null}
            <dt>runs</dt>
            <dd>{runsCount}</dd>
          {/if}
        </dl>
        {#if validation.length > 0}
          <ul class="mt-3 flex flex-col gap-1 border-t border-line pt-2 text-xs">
            {#each validation as v, i (i)}
              <li class={v.severity === 'error' ? 'text-red' : 'text-amber'}>
                {v.code}<span class="prose-text text-fg-2">: {v.message}{v.node ? ` (${v.node})` : ''}</span>
              </li>
            {/each}
          </ul>
        {/if}
      </section>

      {#if meta.effects?.length || meta.requires?.files?.length || meta.requires?.commands?.length}
        <section class="box px-4 pt-3.5 pb-3.5">
          <span class="box-cap">effects</span>
          {#if meta.effects?.length}
            <div class="flex flex-wrap gap-1.5">
              {#each meta.effects as e (e)}
                <span class={['tag', effectTone[e] ?? 'text-muted-foreground']}>{e}</span>
              {/each}
            </div>
          {/if}
          {#if meta.requires?.files?.length || meta.requires?.commands?.length}
            <p class="mt-2.5 text-xs break-words text-muted-foreground">
              requires · {[...(meta.requires?.files ?? []), ...(meta.requires?.commands ?? [])].join(' ')}
            </p>
          {/if}
        </section>
      {/if}

      {#if meta.trigger?.when?.length || meta.trigger?.avoid_when?.length}
        <section class="box px-4 pt-3.5 pb-3">
          <span class="box-cap">trigger</span>
          <ul class="prose-text flex flex-col gap-1.5 text-xs">
            {#each meta.trigger?.when ?? [] as w, i (i)}
              <li><span class="font-mono text-violet">»</span> {w}</li>
            {/each}
            {#each meta.trigger?.avoid_when ?? [] as w, i (i)}
              <li class="text-muted-foreground"><span class="font-mono">✕</span> {w}</li>
            {/each}
          </ul>
        </section>
      {/if}
    </div>

    <!-- Center: the graph. The yaml and the full node form open over it. -->
    <div class="relative mt-2.5 h-[65vh] min-h-0 min-w-0 border border-line lg:h-auto">
      {#if showYaml}
        <div class="absolute inset-0 z-10 flex flex-col bg-background p-3">
          <div class="mb-2 flex items-center justify-between gap-2">
            <div class="flex items-center gap-2">
              <span class="hd">yaml</span>
              <Badge variant="outline" class="text-muted-foreground">read-only</Badge>
            </div>
            <Button variant="ghost" size="icon" class="size-7" onclick={() => (showYaml = false)}>
              <X />
            </Button>
          </div>
          <div class="flex min-h-0 flex-1 flex-col overflow-hidden border border-line">
            <CodeEditor value={yamlText} readonly />
          </div>
        </div>
      {/if}

      <!-- Same form as the editor, read-only: fields keep their text selectable
           and the prompt keeps its scrollbar, but nothing here can change the
           playbook. -->
      {#if selectedNode && showForm && !showYaml}
        <div class="absolute inset-0 z-10 flex flex-col bg-background p-3">
          <div class="mx-auto mb-2 flex w-full max-w-5xl items-center justify-between gap-2">
            <div class="flex min-w-0 items-center gap-2">
              <strong class="truncate text-sm">{selectedNode.id}</strong>
              <Badge variant="secondary" class="text-[10px]">{selectedNode.type}</Badge>
              <Badge variant="outline" class="text-muted-foreground">read-only</Badge>
            </div>
            <Button variant="ghost" size="icon" class="size-7" title="Close" onclick={() => (showForm = false)}>
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
        {edgeTypes}
        nodesDraggable={false}
        nodesConnectable={false}
        elementsSelectable={false}
        elevateEdgesOnSelect={false}
        onnodeclick={({ node }) => select(node.id)}
        onpaneclick={() => select(null)}
      >
        <Background />
        <Controls showLock={false} />
        <FitTop {nodes} key={`${workspace}/${id}`} />
      </SvelteFlow>
    </div>

    <!-- Right: the selected node, and the history and stats when asked for. -->
    <div class="flex min-w-0 flex-col gap-[18px] pt-2.5 lg:min-h-0 lg:overflow-auto">
      <!-- 0.23.0 stats (C3) -->
      {#if showStats}
        <section class="box px-4 pt-3.5 pb-3.5" data-testid="playbook-stats-panel">
          <span class="box-cap">run stats</span>
          <div class="mb-1 flex justify-end">
            <Button variant="ghost" size="icon-xs" title="Hide the run stats" onclick={() => (showStats = false)}>
              <X />
            </Button>
          </div>
          <PlaybookStatsCard report={stats} loading={statsLoading} error={statsError} />
        </section>
      {/if}

      {#if showHistory}
        <section class="box px-4 pt-3.5 pb-3.5">
          <span class="box-cap">version history</span>
          <div class="mb-1 flex justify-end">
            <Button
              variant="ghost"
              size="icon-xs"
              title="Hide version history"
              onclick={() => (showHistory = false)}
            >
              <X />
            </Button>
          </div>
          {#if versions.length === 0}
            <p class="text-xs text-muted-foreground">no versions</p>
          {:else}
            <ul class="flex max-h-80 flex-col overflow-auto">
              {#each versionsDesc as v (v.version)}
                <li class="border-b border-line py-2 last:border-b-0">
                  <div class="flex items-center gap-2">
                    <span class={['text-sm', v.is_current && 'font-semibold text-orange']}>{v.version}</span>
                    {#if v.is_current}
                      <Badge variant="secondary" class="text-[10px]">current</Badge>
                    {/if}
                    {#if trialBadge(v)}
                      <Badge variant="outline" class="text-[10px]" title={v.provenance?.trial?.reason ?? ''}>
                        {trialBadge(v)}
                      </Badge>
                    {/if}
                    <!-- Any stored version can become the current one, which is
                         what the store allows; the old condition offered the
                         switch only for an unpromoted supervisor patch, so
                         ordinary versions - the newer ones included - had no
                         way back. -->
                    {#if !v.is_current}
                      <Button
                        variant="outline"
                        size="xs"
                        class="ml-auto"
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
                  </div>
                  <div class="mt-0.5 text-xs text-muted-foreground">{provenanceLabel(v)}</div>
                </li>
              {/each}
            </ul>
          {/if}
        </section>
      {/if}

      {#if selectedNode}
        <NodeSummary
          node={selectedNode}
          data={selectedData}
          edges={playbookEdges}
          agent={selectedAgent}
          onopenform={() => (showForm = true)}
          onclose={() => select(null)}
        />
      {:else}
        <section class="box px-4 pt-3.5 pb-3.5">
          <span class="box-cap">node</span>
          <p class="text-xs text-muted-foreground">
            Select a node on the graph to see its profile, links and prompt.
          </p>
        </section>
      {/if}
    </div>
  </div>
</div>

<StatusBar items={statusItems} />

<!-- 0.24.0 irreversible consent -->
<IrreversibleRunDialog
  bind:open={irreversibleOpen}
  playbookId={id}
  sources={irreversibleSources}
  onconfirm={confirmIrreversibleRun}
/>
