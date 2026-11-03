<script lang="ts">
  import { tick, untrack } from 'svelte'
  import { SvelteFlow, Background, Controls } from '@xyflow/svelte'
  import '@xyflow/svelte/dist/style.css'
  import {
    ApiError,
    decideKnownProblem,
    fetchCandidate,
    fetchCurrentHistory,
    fetchFit,
    fetchKnownProblem,
    fetchKnownProblems,
    fetchPlaybook,
    fetchProfiles,
    fetchProjects,
    fetchOverlay,
    fetchStats,
    fetchVersions,
    keepCurrentCandidate,
    makeCurrent,
    runPlaybook,
    setFrozen,
    type ProfileSummary,
  } from '../lib/api'
  import {
    profileModelKey,
    profileModelMap,
    toFlow,
    withProfileModels,
    type FlowEdge,
    type FlowNode,
  } from '../lib/graph'
  import { accentVar, profileColor } from '../lib/profilecolor'
  import type { ProfileScope } from '../lib/profileref'
  import { slowRefresh, subscribeChanges } from '../lib/ws'
  import { shortModel } from '../lib/format'
  import { onEscape } from '../lib/hooks/escape.svelte'
  import { approvalBadge, approvalHint, provenanceLabel, trialBadge } from '../lib/versioninfo'
  import type {
    PlaybookDetail,
    PlaybookEdge as PlaybookEdgeType,
    PlaybookParam,
    VersionInfo,
    PlaybookNode as PlaybookNodeType,
    ValidationIssue,
  } from '../lib/types'
  import { issueLine } from '../lib/playbookedit'
  import IssuesPanel from '../lib/IssuesPanel.svelte'
  import FocusOn from '../lib/FocusOn.svelte'
  import { drawableOf, withIssues, type IssueTarget } from '../lib/issues'
  import {
    alignmentText,
    alignmentTone,
    classWord,
    graphOf,
    graphWithheld,
    nodeCounts,
    notRunTitle,
    overlayBanner,
    projectOverlay,
  } from '../lib/runoverlay'
  import { nodeLayer, nodeStatsOf } from '../lib/playbookstats'
  import type {
    OverlayCandidate,
    PlaybookOverlay,
    CandidateQualityView,
    FitReport,
    History as CurrentHistory,
    Moved,
    ProblemsView,
    QualityRecord,
  } from '../lib/api.gen'
  import MakeCurrentDialog from '../lib/MakeCurrentDialog.svelte'
  import CurrentHistoryList from '../lib/CurrentHistoryList.svelte'
  import { moveOutcome } from '../lib/currentmove'
  import type { ProblemAction, ProblemDecision } from '../lib/api'
  import FitCard from '../lib/FitCard.svelte'
  import CandidateQualityCard from '../lib/CandidateQualityCard.svelte'
  import CandidateSummaryCard from '../lib/CandidateSummaryCard.svelte'
  import ScenarioRunPanel from '../lib/ScenarioRunPanel.svelte'
  import JudgeSandboxPanel from '../lib/JudgeSandboxPanel.svelte'
  import { DEMO_PLAYBOOK } from '../lib/api'
  import FlaskConical from '@lucide/svelte/icons/flask-conical'
  import KnownProblemsCard from '../lib/KnownProblemsCard.svelte'
  import { groupProblems, recheckResult } from '../lib/knownproblems'
  import Bug from '@lucide/svelte/icons/bug'
  import { verdictText } from '../lib/quality'
  import ListChecks from '@lucide/svelte/icons/list-checks'
  import CodeEditor from '../lib/CodeEditor.svelte'
  import NodePanel from '../lib/NodePanel.svelte'
  import IrreversibleRunDialog from '../lib/IrreversibleRunDialog.svelte'
  import LaunchConfirmDialog from '../lib/LaunchConfirmDialog.svelte'
  import RunStartDialog from '../lib/RunStartDialog.svelte'
  import StartActionsList from '../lib/StartActionsList.svelte'
  import { startRefusalOf } from '../lib/nextactions'
  import { sameRoute } from '../lib/routeguard'
  import type { StartAction } from '../lib/api.gen'
  import type { RunBudget, RunInput } from '../lib/api'
  import { consentStep } from '../lib/irreversibleconsent'
  import {
    launchQuestion,
    launchStep,
    type LaunchConfirmation,
    type LaunchQuestion,
  } from '../lib/launch'
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
  import { previousVersion } from '../lib/playbookstats'
  // --- end of 0.23.0 stats ---
  import X from '@lucide/svelte/icons/x'

  // `run`: a deep link (`?run=`) that lays that run over the graph.
  let { id, workspace = '', run: runLink }: { id: string; workspace?: string; run?: string } = $props()

  let nodes = $state.raw<FlowNode[]>([])
  let edges = $state.raw<FlowEdge[]>([])
  // The graph as the definition draws it; the layers and the issue markers
  // are put on top of it by `decorate`.
  let baseNodes = $state.raw<FlowNode[]>([])
  let baseEdges = $state.raw<FlowEdge[]>([])

  // --- graph layers: "last run" and "counts" ---
  // Off by default, remembered per browser; storage may be unavailable.
  const LAYER_KEY = 'apb.playbook.layers'
  function readLayers(): { lastRun: boolean; counts: boolean } {
    try {
      const v = JSON.parse(localStorage.getItem(LAYER_KEY) ?? '{}') as Record<string, unknown>
      return { lastRun: v.lastRun === true, counts: v.counts === true }
    } catch {
      return { lastRun: false, counts: false }
    }
  }
  function writeLayers() {
    try {
      localStorage.setItem(LAYER_KEY, JSON.stringify({ lastRun: layerLastRun, counts: layerCounts }))
    } catch {
      // A per-browser convenience only.
    }
  }
  const storedLayers = readLayers()
  let layerLastRun = $state(storedLayers.lastRun)
  let layerCounts = $state(storedLayers.counts)
  let overlay = $state.raw<PlaybookOverlay | null>(null)
  let overlayCandidates = $state.raw<OverlayCandidate[]>([])
  let overlayError = $state<string | null>(null)
  let overlayLoading = $state(false)
  let pickerOpen = $state(false)
  // The run history the counts layer reads: the same request as the runs
  // count (every run, no period chosen yet).
  let countsReport = $state.raw<import('../lib/api.gen').StatsReport | null>(null)
  // The selected issue's edges, highlighted, and the canvas focus.
  let issueEdges = $state.raw<ReadonlySet<string>>(new Set())
  let focus = $state.raw<{ nodes: string[]; seq: number } | null>(null)
  let focusSeq = 0

  const overlayGraph = $derived(graphOf(overlay?.graph ?? null))
  const projected = $derived(overlay && overlayGraph ? projectOverlay(overlay, overlayGraph) : null)
  const overlayNote = $derived(
    overlay && projected ? overlayBanner(overlay, projected.missingInShown) : null,
  )

  // The canvas: the definition, then the layers, then the issue markers.
  function decorate() {
    const g = layerLastRun ? overlayGraph : null
    const p = layerLastRun ? projected : null
    const counts = layerCounts ? nodeStatsOf(countsReport, version) : null
    const runNodes = new Map((g?.nodes ?? []).map((n) => [n.node, n]))
    const notRun = new Map((g?.not_run ?? []).map((n) => [n.node, n]))
    const decorated = baseNodes.map((n) => {
      const { status: _s, runCount: _c, notRun: _n, layer: _l, ...data } = n.data
      const r = runNodes.get(n.id)
      const nr = notRun.get(n.id)
      const c = counts?.get(n.id)
      return {
        ...n,
        selected: n.id === selectedNodeId,
        data: {
          ...data,
          ...(r ? { status: r.status ?? r.last_status ?? undefined, runCount: nodeCounts(r) } : {}),
          ...(nr ? { notRun: { word: classWord(nr.class), title: notRunTitle(nr) } } : {}),
          ...(c ? { layer: nodeLayer(c) } : {}),
        },
      }
    })
    const runEdges = baseEdges.map((e) => {
      const look = p && e.data.address ? p.edges.get(e.data.address) : undefined
      const { run: _r, ...data } = e.data
      return { ...e, data: look ? { ...data, run: look } : data }
    })
    const out = withIssues(decorated, runEdges, validation, issueEdges)
    nodes = withProfileModels(out.nodes, models)
    edges = out.edges
  }

  $effect(() => {
    void [layerLastRun, layerCounts, overlay, countsReport, validation, issueEdges, baseNodes, baseEdges]
    untrack(() => decorate())
  })

  let overlayToken = 0
  async function loadOverlay(opts: { run?: string; before?: string } = {}) {
    const token = ++overlayToken
    overlayLoading = true
    overlayError = null
    try {
      const o = await fetchOverlay(id, workspace, { version: version || undefined, ...opts })
      if (token !== overlayToken) return
      if (opts.before !== undefined) {
        overlayCandidates = [...overlayCandidates, ...o.candidates]
        overlay = { ...(overlay ?? o), next_before: o.next_before }
      } else {
        if (!overlayCandidates.length || !opts.run) overlayCandidates = o.candidates
        overlay = overlay && opts.run ? { ...o, candidates: overlay.candidates, next_before: overlay.next_before } : o
      }
    } catch (e) {
      if (token === overlayToken) overlayError = String(e)
    } finally {
      if (token === overlayToken) overlayLoading = false
    }
  }

  function toggleLastRun() {
    layerLastRun = !layerLastRun
    writeLayers()
    if (layerLastRun && !overlay) void loadOverlay()
  }
  function toggleCounts() {
    layerCounts = !layerCounts
    writeLayers()
  }
  function pickRun(runId: string) {
    pickerOpen = false
    void loadOverlay({ run: runId })
  }

  // An issue picked in the list: its edges highlighted and the first
  // selected, its node selected, or the YAML view for one with nothing to
  // draw; the canvas centres on it.
  function selectIssue(t: IssueTarget) {
    if (t.kind === 'document') {
      if (t.yaml) showYaml = true
      return
    }
    showYaml = false
    if (t.kind === 'node') {
      issueEdges = new Set()
      select(t.node)
      focus = { nodes: [t.node], seq: ++focusSeq }
      return
    }
    issueEdges = new Set(t.edges)
    select(null)
    const first = baseEdges.find((e) => e.data.address === t.first)
    if (first) focus = { nodes: [first.source, first.target], seq: ++focusSeq }
  }
  let name = $state<string>(untrack(() => id))
  let version = $state<string>('')
  let validation = $state<ValidationIssue[]>([])
  let versions = $state<VersionInfo[]>([])
  // The candidate on trial and how it compares with current, read with the
  // version list; a person's decision on it in flight.
  let candidate = $state<CandidateQualityView | null>(null)
  let deciding = $state(false)
  // The moves of `current`, read with the version list, and the version a
  // person is about to make current.
  let currentHistory = $state<CurrentHistory | null>(null)
  let moveTarget = $state<string | null>(null)
  let moveOpen = $state(false)
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
      // With two versions or more, compare the newest with the one before
      // it, so the card shows how quality moved.
      let r = await fetchStats(id, workspace)
      const base = previousVersion(r)
      if (base) {
        try {
          r = await fetchStats(id, workspace, /* since */ '', base)
        } catch {
          // The comparison is an extra: the plain report stays.
        }
      }
      if (key === `${workspace}/${id}`) stats = r
    } catch (e) {
      statsError = String(e)
    } finally {
      statsLoading = false
    }
  }

  function toggleStats() {
    showStats = !showStats
    if (showStats) {
      loadStats()
      void reveal('[data-testid="playbook-stats-panel"]')
    }
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
    if (showFit) {
      loadFit()
      void reveal('[data-testid="fit-panel"]')
    }
  }

  function openHistory(open: boolean) {
    showHistory = open
    if (open) void reveal('[data-testid="history-panel"]')
  }

  // Below the three-column breakpoint (lg, 1024px) the right column sits
  // under the graph: scroll a panel that just opened into view, else a tap
  // on History, Stats, Fit, Problems or a node seems to do nothing.
  async function reveal(selector: string) {
    if (window.matchMedia?.('(min-width: 1024px)').matches) return
    await tick()
    const reduce = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches
    document.querySelector(selector)?.scrollIntoView({ block: 'start', behavior: reduce ? 'auto' : 'smooth' })
  }
  // --- end of 0.23.0 stats ---

  // Known problems: read with the playbook (the toolbar counts the ones a
  // person should look at), shown in a side panel; a person's action on one
  // in flight. A response for another playbook writes nothing.
  // Scenario runs: the panel reads its own list each time it opens. The
  // demo's page opens it, so a first look also shows a scenario run.
  let showScenarios = $state(false)
  function toggleScenarios() {
    showScenarios = !showScenarios
    if (showScenarios) void reveal('[data-testid="scenarios-panel"]')
  }
  // The judge sandbox of the selected judge node.
  let showSandbox = $state(false)

  let showProblems = $state(false)
  function toggleProblems() {
    showProblems = !showProblems
    if (showProblems) void reveal('[data-testid="known-problems-panel"]')
  }
  let problems = $state<ProblemsView | null>(null)
  let problemsError = $state<string | null>(null)
  // The problems whose action is in flight: each is disabled on its own.
  let problemsBusy = $state<ReadonlySet<string>>(new Set())
  const problemsToLook = $derived(problems ? groupProblems(problems.problems).look.length : 0)
  let problemsToken = 0
  async function loadProblems() {
    const token = ++problemsToken
    try {
      const v = await fetchKnownProblems(id, workspace)
      if (token !== problemsToken) return
      problems = v
      problemsError = null
    } catch (e) {
      if (token === problemsToken) problemsError = String(e)
    }
  }

  const ACTION_DONE: Record<ProblemAction, string> = {
    confirm: 'Problem confirmed: shown to runs',
    dismiss: 'Problem dismissed',
    reopen: 'Problem reopened',
    close: 'Problem closed',
    recheck: 'Problem rechecked',
  }

  /** Acts on one problem; resolves to null when it was done, else why not
   * (the close form keeps itself open and shows it). */
  async function actOnProblem(
    problem: string,
    action: ProblemAction,
    body: ProblemDecision,
  ): Promise<string | null> {
    const on = `${workspace}\u0000${id}`
    const stillHere = () => on === `${workspace}\u0000${id}`
    problemsBusy = new Set([...problemsBusy, problem])
    try {
      const result = await decideKnownProblem(id, problem, action, body, workspace)
      if (!stillHere()) return null
      if (action === 'recheck' && body.verdict === 'holds') {
        toast.success('Recorded: the problem still holds')
      } else if (action === 'recheck' && 'verdict' in result) {
        const r = recheckResult(result)
        toast[r.tone](r.text)
      } else {
        toast.success(ACTION_DONE[action])
      }
      await loadProblems()
      return null
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e)
      // A refused close is shown in its form, where the quote can be fixed.
      if (stillHere() && action !== 'close') {
        toast.error('The known-problem action failed', { description: message })
      }
      return message
    } finally {
      if (stillHere()) problemsBusy = new Set([...problemsBusy].filter((p) => p !== problem))
    }
  }

  async function problemPassages(problem: string) {
    return (await fetchKnownProblem(id, problem, workspace)).changed_passages
  }

  // The playbook's own nodes (not the flow nodes): clicking a node opens the
  // same form the editor uses, read-only, so a prompt can be read here without
  // switching to edit mode.
  let playbookNodes = $state.raw<PlaybookNodeType[]>([])
  let playbookEdges = $state.raw<PlaybookEdgeType[]>([])
  // The rest of the definition the side boxes show (description, effects,
  // requires, trigger, defaults); the server sends the whole parsed playbook.
  let meta = $state.raw<PlaybookMeta>({})
  let publishOnly = $state(false)
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

  // `effective`: the server's effective effects; the declared list (deduped)
  // only when an older server sends none.
  function metaOf(pb: PlaybookDetail['playbook'], effective?: string[]): PlaybookMeta {
    const raw = pb as unknown as Record<string, unknown>
    const list = (v: unknown) => (Array.isArray(v) ? v.map(String) : [])
    const obj = (v: unknown) => (v && typeof v === 'object' ? (v as Record<string, unknown>) : null)
    const req = obj(raw.requires)
    const trig = obj(raw.trigger)
    const def = obj(raw.defaults)
    return {
      description: typeof raw.description === 'string' ? raw.description.trim() : '',
      effects: [...new Set(effective ?? list(raw.effects))],
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
  // Each profile the graph's agent and answering finish nodes use, with the
  // scope a node pinned it to, in first-use order, for the legend.
  type UsedProfile = { name: string; scope?: ProfileScope; key: string }
  const usedProfiles = $derived.by(() => {
    const seen: UsedProfile[] = []
    for (const n of nodes) {
      const name = n.data.profile
      if (!name || (n.data.kind !== 'agent_task' && n.data.kind !== 'finish')) continue
      const key = profileModelKey(name, n.data.profileScope)
      if (!seen.some((u) => u.key === key)) seen.push({ name, scope: n.data.profileScope, key })
    }
    return seen
  })
  const hasJudge = $derived(nodes.some((n) => n.data.kind === 'judge'))
  const hasHumanGate = $derived(nodes.some((n) => n.data.kind === 'human_review'))
  const agentTasks = $derived(nodes.filter((n) => n.data.kind === 'agent_task').length)
  const resolvedProfiles = $derived(usedProfiles.filter((p) => models.has(p.key)).length)
  // The profile a ref resolves to here: an explicit scope exactly, else the
  // way scope `auto` does (this project's own first, else the global one).
  const resolveProfile = (p: string, scope?: ProfileScope) => {
    const project = () =>
      profileList.find((s) => s.name === p && s.scope === 'project' && s.workspace_id === workspace)
    const global = () => profileList.find((s) => s.name === p && s.scope === 'global')
    if (scope === 'project') return project()
    if (scope === 'global') return global()
    return project() ?? global()
  }
  const untrustedProfiles = $derived(
    usedProfiles
      .filter((p) => {
        const r = resolveProfile(p.name, p.scope)
        return !!r && !r.trusted
      })
      .map((p) => p.key),
  )
  const selectedAgent = $derived.by(() => {
    const p = selectedData?.profile
    return p ? (resolveProfile(p, selectedData?.profileScope)?.agent ?? '') : ''
  })

  // Select a node on the canvas: the card glows and the side shows it.
  function select(nodeId: string | null) {
    selectedNodeId = nodeId
    showForm = false
    showSandbox = false
    nodes = nodes.map((n) => (n.selected === (n.id === nodeId) ? n : { ...n, selected: n.id === nodeId }))
    if (nodeId) void reveal('[data-testid="node-summary"]')
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
    } else if (showFit) {
      showFit = false
    } else if (showSandbox) {
      showSandbox = false
    } else if (showScenarios) {
      showScenarios = false
    } else if (showProblems) {
      showProblems = false
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
      .map((v) => `${v.severity} ${issueLine(v)}`)
      .join('\n'),
  )

  // Version history newest-first: the API lists versions oldest first in
  // semver order, so this only reverses it.
  const versionsDesc = $derived([...versions].reverse())
  const currentVersion = $derived(versions.find((v) => v.is_current)?.version ?? null)

  async function loadVersions(token: number) {
    try {
      const vs = await fetchVersions(id, workspace)
      if (token !== loadToken) return
      versions = vs
    } catch (e) {
      if (token === loadToken) toast.error('Failed to load versions', { description: String(e) })
    }
    try {
      const h = await fetchCurrentHistory(id, workspace)
      if (token === loadToken) currentHistory = h
    } catch (e) {
      if (token === loadToken) {
        currentHistory = null
        toast.error('Failed to load the moves of current', { description: String(e) })
      }
    }
    try {
      const c = await fetchCandidate(id, workspace)
      if (token === loadToken) {
        candidate = c.candidate
        candidateRefresh++
      }
    } catch (e) {
      // `candidate: null` is how "none on trial" reads; a failure says so
      // instead of looking the same.
      if (token === loadToken) {
        candidate = null
        toast.error('Failed to load the candidate', { description: String(e) })
      }
    }
  }

  // Promote or keep current: the decision is recorded on the candidate's
  // trial record as made in the dashboard, then the history reloads. A
  // decision answers for the playbook it was made on: once the page moved
  // to another playbook, it neither reloads nor reports here, and a reload
  // that fails is not a failed decision.
  async function decideCandidate(decide: () => Promise<QualityRecord>) {
    const decidedOn = `${workspace}\u0000${id}`
    const stillHere = () => decidedOn === `${workspace}\u0000${id}`
    deciding = true
    let rec: QualityRecord
    try {
      rec = await decide()
    } catch (e) {
      // Another playbook's card is not this decision's to unlock.
      if (stillHere()) {
        toast.error('The decision failed', { description: String(e) })
        deciding = false
      }
      return
    }
    if (!stillHere()) return
    deciding = false
    toast.success(`Candidate: ${verdictText(rec.verdict)}`)
    await reload()
  }

  // The summary card reads its own summary: each reload of the candidate
  // asks it to read again. A promotion it made reloads the page like any
  // decision.
  let candidateRefresh = $state(0)
  async function candidateDecided(rec: QualityRecord) {
    toast.success(`Candidate: ${verdictText(rec.verdict)}`)
    await reload()
  }

  async function load(token: number) {
    try {
      const detail = await fetchPlaybook(id, workspace)
      if (token !== loadToken) return
      const flow = toFlow(detail.playbook, detail.layout)
      baseNodes = flow.nodes
      baseEdges = flow.edges
      playbookNodes = detail.playbook.nodes
      playbookEdges = detail.playbook.edges
      meta = metaOf(detail.playbook, detail.effective_effects)
      publishOnly = detail.playbook.publish_only === true
      yamlText = detail.yaml
      name = detail.playbook.name || detail.id
      version = detail.version
      validation = detail.validation
      frozen = detail.frozen
      budgetBlock = detail.playbook.budget ?? null
      runParams = detail.playbook.params ?? []
      // The layer reads the shown version: load it once the version is known.
      if (layerLastRun) void loadOverlay(overlay?.chosen ? { run: overlay.chosen } : runLink ? { run: runLink } : {})
    } catch (e) {
      if (token === loadToken) toast.error('Failed to load playbook', { description: String(e) })
    }
    void loadProblems()
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
      if (route === routeToken) {
        runsCount = r.runs
        countsReport = r
      }
    } catch {
      if (route === routeToken) {
        runsCount = null
        countsReport = null
      }
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
        ? { label: 'validate', value: `${errors.length} ${errors.length === 1 ? 'error' : 'errors'}`, tone: 'danger', title: issuesTitle }
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
  // A refused start the person can confirm here: the question, and the
  // retry it makes.
  let launchOpen = $state(false)
  let launchAsked = $state<LaunchQuestion | null>(null)
  let launchRetry: (() => void) | null = null
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
  // A refused start that says what it needs first: its message and start
  // actions, shown beside the graph until the next start or a close.
  let startRefusal = $state<{ message: string; actions: StartAction[] } | null>(null)

  function openStart() {
    startRetry = false
    startOpen = true
    // The dialog shows the launch policy from the fit report, which is
    // otherwise loaded only with the fit panel: fetch it fresh here.
    loadFit()
  }

  function startWith(budget: RunBudget | undefined, input: RunInput) {
    startBudget = budget
    startInput = input
    void run()
  }

  // The launch confirmation the last start carried, for its consent retry.
  let lastLaunch: LaunchConfirmation | undefined

  async function run(consentNonce?: string, launch?: LaunchConfirmation) {
    starting = true
    startRefusal = null
    lastLaunch = launch
    // An answer that arrives after the page moved to another playbook is
    // not this page's: it only ends the pending start, nothing else.
    const still = sameRoute(() => routeToken)
    try {
      const started = await runPlaybook(id, workspace, {
        consentNonce,
        budget: startBudget,
        input: startInput,
        launch,
      })
      if (!still()) {
        starting = false
        return
      }
      const { run_id, warnings } = started
      for (const w of warnings ?? []) toast.warning(w)
      for (const r of started.not_remembered ?? []) {
        toast.warning(`Not remembered: ${r.name} (${r.reason.replace(/_/g, ' ')})`)
      }
      if (started.memory_error) toast.warning('The input was not remembered', { description: started.memory_error })
      location.hash = `#/run/${encodeURIComponent(workspace)}/${encodeURIComponent(run_id)}`
    } catch (e) {
      starting = false
      if (!still()) return
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
      // A dashboard without keys starts an `ask` playbook only with the
      // nonce of its refusal, after the person confirms here.
      const launchNext = e instanceof ApiError ? launchStep(e.body, launch) : null
      if (launchNext?.kind === 'confirm') {
        const confirmation = launchNext.confirmation
        launchAsked = launchQuestion(launchNext)
        launchRetry = () => void run(consentNonce, confirmation)
        launchOpen = true
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
      const refusal = startRefusalOf(e)
      if (refusal) startRefusal = refusal
    }
  }

  function confirmIrreversibleRun() {
    // The launch confirmation of the same start rides the consent retry.
    void run(irreversibleNonce, lastLaunch)
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

  // "Make current": the dialog posts the move with the version this page
  // shows as current, so a move made elsewhere meanwhile is never undone
  // unseen; a stale answer reloads the list. Any stored version can be made
  // current except the candidate on trial, which its own card decides.
  function askMakeCurrent(v: string) {
    moveTarget = v
    moveOpen = true
  }

  // `expected` is the `current` the dialog showed the person, kept while
  // the page reloads underneath it.
  const submitMove = (reason: string, expected: string | null) =>
    moveOutcome(() => makeCurrent(id, moveTarget ?? '', expected, reason, workspace))

  async function moved(m: Moved) {
    // Awaited: the toast and the `current` badge never disagree.
    await reload()
    toast.success(`Now using ${m.to}`)
    if (m.awaiting_approval) {
      toast.warning(`${m.to} waits for an approval of its imported scripts`, {
        description: 'Runs stay refused until a person approves it.',
      })
    }
    if (m.audit_pending) {
      toast.warning('The move is recorded later', {
        description: 'Its history line is written by the next change of current.',
      })
    }
  }

  $effect(() => {
    // Track the route target so navigating (incl. browser back/forward) between
    // two playbook views reuses this component and still reloads. Clear the
    // route-specific view first so a slow load cannot briefly show the previous
    // playbook's graph or version list.
    void id
    void workspace
    startRefusal = null
    nodes = []
    edges = []
    baseNodes = []
    baseEdges = []
    overlay = null
    overlayCandidates = []
    overlayError = null
    overlayToken++
    pickerOpen = false
    countsReport = null
    issueEdges = new Set()
    focus = null
    // A deep link to a run turns the layer on for this visit.
    if (untrack(() => runLink)) layerLastRun = true
    playbookNodes = []
    playbookEdges = []
    meta = {}
    runsCount = null
    publishOnly = false
    selectedNodeId = null
    showForm = false
    versions = []
    candidate = null
    currentHistory = null
    moveOpen = false
    moveTarget = null
    deciding = false
    validation = []
    stats = null
    showStats = false
    projectName = ''
    const route = ++routeToken
    void loadRouteSide(route)
    const runsLane = slowRefresh(() => void loadRunsCount(route), 30_000)
    runsLane.request()
    fit = null
    fitError = null
    fitLoading = false
    fitToken++
    showFit = false
    showProblems = false
    showScenarios = id === DEMO_PLAYBOOK
    showSandbox = false
    problems = null
    problemsError = null
    problemsBusy = new Set()
    problemsToken++
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
  {#each usedProfiles as p (p.key)}
    <span class="legend-sq whitespace-nowrap" style:--k={profileColor(p.name)}>
      {p.name}{#if models.get(p.key)}<span class="text-muted-foreground" title={models.get(p.key)}
          >{` · ${shortModel(models.get(p.key) ?? '')}`}</span
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
          {#if candidate}
            <button
              type="button"
              class="tag candidate-chip"
              title="A candidate version is on trial: open the history to compare and decide"
              data-testid="candidate-chip"
              onclick={() => openHistory(true)}
            >
              candidate {candidate.version}
            </button>
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
            onclick={openStart}
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
            onclick={() => openHistory(!showHistory)}
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
            variant={showFit ? 'default' : 'outline'}
            size="sm"
            class="h-7"
            onclick={toggleFit}
            title="Will this playbook run in this project: compatibility, setup, trust and consent"
            data-testid="fit-toggle"
          >
            <ListChecks data-icon="inline-start" />
            Fit
          </Button>
          <Button
            variant={showScenarios ? 'default' : 'outline'}
            size="sm"
            class="h-7"
            onclick={toggleScenarios}
            title="Scenario runs: run the playbook against scripted answers, quick (nothing spent) or smoke"
            data-testid="scenarios-toggle"
          >
            <FlaskConical data-icon="inline-start" />
            Scenarios
          </Button>
          <Button
            variant={showProblems ? 'default' : 'outline'}
            size="sm"
            class="h-7"
            onclick={toggleProblems}
            title="Known problems: trouble with this playbook's tools that earlier runs reported"
            data-testid="known-problems-toggle"
          >
            <Bug data-icon="inline-start" />
            Problems
            {#if problemsToLook > 0}
              <span class="pv-count" title="Known problems that need a look">{problemsToLook}</span>
            {/if}
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
          <div class="issues-slot">
            <IssuesPanel
              issues={validation}
              drawable={drawableOf(baseNodes, baseEdges)}
              fix={{ workspace, id }}
              onselect={selectIssue}
            />
          </div>
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
              <NodePanel {id} node={selectedNode} {workspace} nodes={playbookNodes} {publishOnly} readonly />
            </div>
          </div>
        </div>
      {/if}

      <div class="layers" data-testid="graph-layers">
        <div class="layer-bar">
          <button
            type="button"
            class="layer-toggle"
            aria-pressed={layerLastRun}
            onclick={toggleLastRun}
            title="Lay a run of this playbook over the graph"
            data-testid="layer-last-run">last run</button
          >
          <button
            type="button"
            class="layer-toggle"
            aria-pressed={layerCounts}
            onclick={toggleCounts}
            title="How often each node executed over every recorded run"
            data-testid="layer-counts">counts</button
          >
          {#if layerLastRun && overlay?.chosen}
            <button
              type="button"
              class="layer-run"
              onclick={() => (pickerOpen = !pickerOpen)}
              title="Choose another run"
              data-testid="layer-run-picker"
            >
              {overlay.chosen}
              <span class={['badge', alignmentTone(overlay.alignment)]}>{alignmentText(overlay.alignment)}</span>
            </button>
          {:else if layerLastRun && overlayCandidates.length}
            <button type="button" class="layer-run" onclick={() => (pickerOpen = !pickerOpen)}>choose a run</button>
          {/if}
        </div>
        {#if layerLastRun}
          {#if overlayLoading && !overlay}
            <p class="layer-note">loading the runs</p>
          {:else if overlayError}
            <p class="layer-note warn">Could not load the runs: {overlayError}</p>
          {:else if overlay && !overlay.chosen && !overlayCandidates.length}
            <p class="layer-note">No run of this playbook yet.</p>
          {:else if overlay && !overlay.chosen}
            <p class="layer-note">No run of this version matches it exactly: choose one.</p>
          {/if}
          {#if overlay && graphWithheld(overlay.graph)}
            <p class="layer-note warn">The run graph data is withheld: edge states are not shown.</p>
          {/if}
          {#if overlayNote}<p class="layer-note warn" data-testid="layer-banner">{overlayNote}</p>{/if}
          {#if projected?.unprojected.length}
            <details class="layer-note">
              <summary>{projected.unprojected.length} not established (a definition that cannot be mapped)</summary>
              <ul>
                {#each projected.unprojected as row, i (i)}<li>{row.text}</li>{/each}
              </ul>
            </details>
          {/if}
          {#if pickerOpen}
            <ul class="picker" data-testid="layer-picker">
              {#each overlayCandidates as c (c.run_id)}
                <li>
                  <button
                    type="button"
                    class={['pick', c.run_id === overlay?.chosen && 'chosen']}
                    onclick={() => pickRun(c.run_id)}
                    title={c.reason ?? undefined}
                  >
                    <span class="pick-id">{c.run_id}</span>
                    <span class="muted">{c.version}</span>
                    <span class={['badge', alignmentTone(c.alignment)]}>{alignmentText(c.alignment)}</span>
                    {#if c.reason}<span class="muted pick-reason">{c.reason}</span>{/if}
                  </button>
                </li>
              {/each}
              {#if overlay?.next_before !== null && overlay?.next_before !== undefined}
                <li>
                  <button
                    type="button"
                    class="pick older"
                    disabled={overlayLoading}
                    onclick={() => void loadOverlay({ before: overlay?.next_before ?? undefined, run: overlay?.chosen ?? undefined })}
                    >older</button
                  >
                </li>
              {/if}
            </ul>
          {/if}
        {/if}
        {#if layerCounts && !countsReport}
          <p class="layer-note">No run history to count yet.</p>
        {/if}
      </div>

      <SvelteFlow
        bind:nodes
        bind:edges
        {nodeTypes}
        {edgeTypes}
        nodesDraggable={false}
        nodesConnectable={false}
        elementsSelectable={false}
        elevateEdgesOnSelect={false}
        onnodeclick={({ node }) => {
          issueEdges = new Set()
          select(node.id)
        }}
        onpaneclick={() => {
          issueEdges = new Set()
          select(null)
        }}
      >
        <Background />
        <Controls showLock={false} />
        <FitTop {nodes} key={`${workspace}/${id}`} />
        <FocusOn {focus} />
      </SvelteFlow>
    </div>

    <!-- Right: the selected node, and the history and stats when asked for. -->
    <div class="flex min-w-0 flex-col gap-[18px] pt-2.5 lg:min-h-0 lg:overflow-auto">
      {#if startRefusal}
        <section class="box start-refusal" data-testid="start-refusal">
          <span class="box-cap">start refused</span>
          <div class="box-close">
            <Button variant="ghost" size="icon-xs" title="Hide the refusal" onclick={() => (startRefusal = null)}>
              <X />
            </Button>
          </div>
          <p class="start-refusal-message">{startRefusal.message}</p>
          <StartActionsList actions={startRefusal.actions} />
        </section>
      {/if}

      <!-- 0.23.0 stats (C3) -->
      {#if showStats}
        <section class="box px-4 pt-3.5 pb-3.5" data-testid="playbook-stats-panel">
          <span class="box-cap">run stats</span>
          <div class="box-close">
            <Button variant="ghost" size="icon-xs" title="Hide the run stats" onclick={() => (showStats = false)}>
              <X />
            </Button>
          </div>
          <PlaybookStatsCard report={stats} loading={statsLoading} error={statsError} />
        </section>
      {/if}

      {#if showFit}
        <section class="box px-4 pt-3.5 pb-3.5" data-testid="fit-panel">
          <span class="box-cap">will it fit here?</span>
          <div class="box-close">
            <Button variant="ghost" size="icon-xs" title="Hide the fit report" onclick={() => (showFit = false)}>
              <X />
            </Button>
          </div>
          <FitCard report={fit} loading={fitLoading} error={fitError} />
        </section>
      {/if}

      {#if showScenarios}
        <section class="box pv-side" data-testid="scenarios-panel">
          <span class="box-cap">scenarios</span>
          <div class="box-close">
            <Button variant="ghost" size="icon-xs" title="Hide the scenarios" onclick={() => (showScenarios = false)}>
              <X />
            </Button>
          </div>
          <ScenarioRunPanel {id} {workspace} />
        </section>
      {/if}

      {#if showProblems}
        <section class="box px-4 pt-3.5 pb-3.5" data-testid="known-problems-panel">
          <span class="box-cap">known problems</span>
          <div class="box-close">
            <Button
              variant="ghost"
              size="icon-xs"
              title="Hide the known problems"
              onclick={() => (showProblems = false)}
            >
              <X />
            </Button>
          </div>
          <KnownProblemsCard
            view={problems}
            loading={problems === null && !problemsError}
            error={problemsError}
            busy={problemsBusy}
            onAct={actOnProblem}
            loadPassages={problemPassages}
          />
        </section>
      {/if}

      {#if showHistory}
        <section class="box px-4 pt-3.5 pb-3.5" data-testid="history-panel">
          <span class="box-cap">version history</span>
          <div class="box-close">
            <Button
              variant="ghost"
              size="icon-xs"
              title="Hide version history"
              onclick={() => (showHistory = false)}
            >
              <X />
            </Button>
          </div>
          {#if candidate}
            <div class="candidate-slot">
              <CandidateSummaryCard
                {id}
                {workspace}
                {frozen}
                refresh={candidateRefresh}
                onDecided={candidateDecided}
              />
              <CandidateQualityCard
                {candidate}
                busy={deciding}
                onKeepCurrent={(reason) => decideCandidate(() => keepCurrentCandidate(id, reason, workspace))}
              />
            </div>
          {/if}
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
                    {#if approvalBadge(v)}
                      <Badge
                        variant="outline"
                        class="text-[10px]"
                        title={approvalHint(id, v.version)}
                        data-testid="version-awaiting-approval"
                      >
                        {approvalBadge(v)}
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
                        onclick={() => askMakeCurrent(v.version)}
                        disabled={frozen || v.is_candidate}
                        title={frozen
                          ? 'Frozen playbook: unfreeze it to change the current version'
                          : v.is_candidate
                            ? 'The candidate on trial is decided in its card'
                            : 'Make this version the current one'}
                        data-testid="make-current"
                      >
                        Make current
                      </Button>
                    {/if}
                  </div>
                  <div class="mt-0.5 text-xs text-muted-foreground">{provenanceLabel(v)}</div>
                </li>
              {/each}
            </ul>
          {/if}
          <div class="current-moves">
            <CurrentHistoryList history={currentHistory} />
          </div>
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
        {#if selectedNode.type === 'judge'}
          <section class="box pv-side" data-testid="judge-sandbox-panel">
            <span class="box-cap">judge sandbox</span>
            {#if showSandbox}
              <div class="box-close">
                <Button variant="ghost" size="icon-xs" title="Hide the judge sandbox" onclick={() => (showSandbox = false)}>
                  <X />
                </Button>
              </div>
              <JudgeSandboxPanel {id} {workspace} node={selectedNode} {version} />
            {:else}
              <div class="pv-sandbox-open">
                <span>Try this judge on inputs of your own or of a run, outside any run.</span>
                <Button variant="outline" size="xs" onclick={() => (showSandbox = true)} data-testid="judge-sandbox-open">
                  Open the sandbox
                </Button>
              </div>
            {/if}
          </section>
        {/if}
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

<RunStartDialog
  bind:open={startOpen}
  playbookId={id}
  {workspace}
  block={budgetBlock}
  params={runParams}
  busy={starting}
  retry={startRetry}
  launch={fit?.launch ?? null}
  onrun={startWith}
/>

{#if moveTarget}
  <MakeCurrentDialog
    bind:open={moveOpen}
    playbookId={id}
    from={currentVersion}
    to={moveTarget}
    submit={submitMove}
    onstale={reload}
    ondone={moved}
  />
{/if}

<LaunchConfirmDialog
  bind:open={launchOpen}
  question={launchAsked}
  onconfirm={() => launchRetry?.()}
/>

<!-- 0.24.0 irreversible consent -->
<IrreversibleRunDialog
  bind:open={irreversibleOpen}
  playbookId={id}
  sources={irreversibleSources}
  onconfirm={confirmIrreversibleRun}
/>

<style>
  .issues-slot {
    margin-top: 12px;
    border-top: 1px solid var(--c-line);
    padding-top: 8px;
  }

  /* The graph layers over the canvas. */
  .layers {
    position: absolute;
    top: 8px;
    left: 8px;
    z-index: 5;
    display: flex;
    flex-direction: column;
    gap: 4px;
    max-width: min(420px, calc(100% - 16px));
    font-size: 11px;
  }
  .layer-bar {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .layer-toggle,
  .layer-run,
  .pick {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 2px 8px;
    border: 1px solid var(--c-line-2);
    background: var(--c-bg);
    color: var(--c-fg-2);
    font: inherit;
    cursor: pointer;
  }
  .layer-toggle[aria-pressed='true'] {
    border-color: var(--c-violet);
    color: var(--c-violet);
  }
  .layer-toggle:focus-visible,
  .layer-run:focus-visible,
  .pick:focus-visible {
    outline: 2px solid var(--ring);
    outline-offset: 1px;
  }
  .layer-note {
    margin: 0;
    padding: 3px 8px;
    border: 1px solid var(--c-line);
    background: var(--c-bg);
    color: var(--muted-foreground);
  }
  .layer-note.warn {
    border-color: var(--c-amber);
    color: var(--c-amber);
  }
  .layer-note ul {
    margin: 4px 0 0;
    padding-left: 14px;
  }
  .badge {
    padding: 0 4px;
    border: 1px solid currentColor;
    font-size: 10px;
  }
  .badge.ok {
    color: var(--c-green);
  }
  .badge.warn {
    color: var(--c-amber);
  }
  .badge.muted,
  .muted {
    color: var(--muted-foreground);
  }
  .picker {
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-height: 50vh;
    margin: 0;
    padding: 4px;
    overflow: auto;
    border: 1px solid var(--c-line-2);
    background: var(--c-bg);
    list-style: none;
  }
  .pick {
    width: 100%;
    flex-wrap: wrap;
    border-color: transparent;
    text-align: left;
  }
  .pick.chosen {
    border-color: var(--c-violet);
  }
  .pick-id {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .pick-reason {
    flex-basis: 100%;
  }
  .pick.older {
    justify-content: center;
  }

  .current-moves {
    margin-top: 10px;
    border-top: 1px solid var(--c-line);
    padding-top: 8px;
  }

  /* A refused start and what it needs first. */
  .start-refusal {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 14px 16px;
    font-size: 13px;
    --k: var(--c-red);
  }
  .start-refusal-message {
    margin: 0;
    color: var(--destructive);
    overflow-wrap: anywhere;
  }
  /* Plain CSS over the app's variables. */
  .pv-side {
    padding: 14px 16px;
  }
  .pv-sandbox-open {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    color: var(--muted-foreground);
    font-size: 0.75rem;
  }
  .candidate-slot {
    display: flex;
    flex-direction: column;
    gap: 16px;
    margin-bottom: 12px;
  }
  .candidate-chip {
    margin-left: 4px;
    background: transparent;
    color: var(--warning);
    font: inherit;
    font-size: 11px;
    cursor: pointer;
  }
  .candidate-chip:focus-visible {
    outline: 2px solid var(--ring);
    outline-offset: 2px;
  }
  .pv-count {
    min-width: 1.1rem;
    padding: 0 0.3rem;
    background: var(--warning);
    color: var(--warning-foreground);
    font-size: 11px;
    line-height: 1.1rem;
    text-align: center;
  }
</style>
