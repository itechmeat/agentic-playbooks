<script lang="ts">
  import { SvelteFlow, Background, Controls } from '@xyflow/svelte'
  import '@xyflow/svelte/dist/style.css'
  import { tick } from 'svelte'
  import {
    fetchProfiles,
    fetchProjects,
    fetchRun,
    fetchRunGrades,
    fetchRunReport,
    gradeRun,
    postAnswer,
    postReview,
    reopenRunGrading,
    runReportHtmlUrl,
    type GraderChoice,
  } from '../lib/api'
  import type { Grade } from '../lib/api.gen'
  import { profileModelMap, toFlow, withProfileModels, type FlowEdge, type FlowNode } from '../lib/graph'
  import { interventionJournal } from '../lib/journal'
  import { cachedNodeIds } from '../lib/runcache'
  import { listProgressByNode } from '../lib/listprogress'
  import { runGates } from '../lib/rungates'
  import { formatCost, formatTokens, runUsageSummary, unknownEventsNote } from '../lib/runusage'
  import {
    currentStage,
    edgeRunState,
    decisionSource,
    formatAge,
    lastNote,
    maxParallel,
    nodeFacts,
    nodeRunData,
    runLogLines,
    runProfiles,
    runStartedTs,
    runSteps,
  } from '../lib/runview'
  import { candidateTrialLabel } from '../lib/versioninfo'
  import { subscribeChanges } from '../lib/ws'
  import PlaybookNode from '../lib/PlaybookNode.svelte'
  import PlaybookEdge from '../lib/PlaybookEdge.svelte'
  import FitTop from '../lib/FitTop.svelte'
  import QuestionPanel from '../lib/QuestionPanel.svelte'
  import PublishPanel from '../lib/PublishPanel.svelte'
  import HostTaskPanel from '../lib/HostTaskPanel.svelte'
  import DecisionsPanel from '../lib/DecisionsPanel.svelte'
  import RunOutcomePanel from '../lib/RunOutcomePanel.svelte'
  import RunVerdictPanel from '../lib/RunVerdictPanel.svelte'
  import RunPauseControls from '../lib/RunPauseControls.svelte'
  import RunNotifyControl from '../lib/RunNotifyControl.svelte'
  import AttemptModelsPanel, { type CurrentAttempt } from '../lib/AttemptModelsPanel.svelte'
  import RunLegend from '../lib/RunLegend.svelte'
  import RunLog from '../lib/RunLog.svelte'
  import AttemptIsolationPanel from '../lib/AttemptIsolationPanel.svelte'
  import RunBudgetPanel from '../lib/RunBudgetPanel.svelte'
  import QualityPanel from '../lib/QualityPanel.svelte'
  import RunKnownProblemsCard from '../lib/RunKnownProblemsCard.svelte'
  import { hasRunProblems } from '../lib/knownproblems'
  import { gradeSkippedText, verdictText } from '../lib/quality'
  import ListProgressPanel from '../lib/ListProgressPanel.svelte'
  import type { RunDetail } from '../lib/types'
  import RunProgress from '$lib/RunProgress.svelte'
  import Topbar from '$lib/components/Topbar.svelte'
  import StatusBar, { type StatusItem } from '$lib/components/StatusBar.svelte'
  import { Button } from '$lib/components/ui/button'
  import * as Tabs from '$lib/components/ui/tabs'
  import { isTerminalRunStatus, runStatusClass, showsDriverDead } from '../lib/status'
  import { statusColor } from '../lib/profilecolor'
  import { pauseLine } from '../lib/pause'
  import { toast } from 'svelte-sonner'
  import TriangleAlert from '@lucide/svelte/icons/triangle-alert'

  let { id, workspace = '' }: { id: string; workspace?: string } = $props()

  let flowNodes = $state.raw<FlowNode[]>([])
  let flowEdges = $state.raw<FlowEdge[]>([])
  let profileModels = $state.raw(new Map<string, string>())
  let nodes = $state.raw<FlowNode[]>([])
  let edges = $state.raw<FlowEdge[]>([])
  let detail = $state<RunDetail | null>(null)
  // Why the first load of this run failed; the page shows it with a retry
  // instead of a loading line that never ends.
  let loadError = $state<string | null>(null)
  let report = $state<string | null>(null)
  let tab = $state<'events' | 'journal' | 'report'>('events')
  let deciding = $state<string | null>(null)
  let answering = $state(false)
  let projectName = $state('')
  let now = $state(Date.now())
  let logBox = $state<HTMLElement | null>(null)
  // Run quality: the stored grades, a rules grade in flight, and the journal
  // event a grade's citation opened in the events list.
  let grades = $state<Grade[]>([])
  let grading = $state(false)
  let citedSeq = $state<number | null>(null)

  const nodeTypes = { playbookNode: PlaybookNode }
  const edgeTypes = { default: PlaybookEdge }
  const journal = $derived(detail ? interventionJournal(detail.events) : [])
  const logLines = $derived(detail ? runLogLines(detail) : [])
  // Every open gate comes from the server's derived progress (runGates), the
  // same view `apb wait` and MCP run_wait decide on.
  const gates = $derived(detail ? runGates(detail) : null)
  const pending = $derived(gates?.reviews ?? [])
  const publishes = $derived(gates?.publishes ?? [])
  const questions = $derived(gates?.questions ?? [])
  const waiting = $derived(gates?.waits ?? [])
  const supervisor = $derived(gates?.supervisor ?? null)
  const hostTasks = $derived(gates?.tasks ?? [])
  const hookEntries = $derived(Object.entries(detail?.hooks ?? {}))
  const children = $derived(detail?.children ?? [])
  const usageLine = $derived(detail?.usage ? runUsageSummary(detail.usage) : null)
  const unknownNote = $derived(unknownEventsNote(detail?.unknown_events))

  // The terminal layout's read of the same detail: rail steps, counters,
  // legend, the attempt box and the status bar.
  const steps = $derived(detail ? runSteps(detail) : [])
  const facts = $derived(detail ? nodeFacts(detail.events) : new Map())
  const runData = $derived(detail ? nodeRunData(detail, now) : new Map())
  const profiles = $derived(
    detail
      ? runProfiles(detail).map((p) => {
          const model = p.model ?? profileModels.get(p.key)
          return model ? { name: p.name, key: p.key, model } : { name: p.name, key: p.key }
        })
      : [],
  )
  const stage = $derived(detail ? currentStage(detail) : null)
  const note = $derived(detail ? lastNote(detail.events) : null)
  const startedTs = $derived(detail ? runStartedTs(detail.events) : undefined)
  const decisionMode = $derived(detail?.decisions ? decisionSource(detail.events)?.mode || 'on' : null)
  const status = $derived(detail?.run_status ?? '')
  const terminal = $derived(isTerminalRunStatus(status))
  const doneCount = $derived(steps.filter((s) => s.state === 'done').length)
  const runningCount = $derived(Object.values(detail?.nodes ?? {}).filter((s) => s === 'running').length)
  // Attempts started, from the journal: one meaning whether or not any
  // attempt reported usage.
  const attemptsTotal = $derived([...facts.values()].reduce((n, f) => n + f.attempts, 0))
  // The reported cost reads as approximate when only some attempts reported
  // one or apb estimated part of it, the way the usage line says it.
  const costNote = $derived.by(() => {
    const u = detail?.usage
    if (!u || u.cost_usd === undefined) return null
    const all = Math.max(u.finished_attempts ?? u.attempts, u.attempts)
    const partial = u.cost_attempts < all
    const why = [
      partial ? `reported by ${u.cost_attempts} of ${all} attempts` : 'reported by every attempt',
      u.estimated ? 'partly estimated by apb' : '',
    ]
      .filter(Boolean)
      .join('; ')
    return {
      text: `${partial || u.estimated ? '~' : ''}${formatCost(u.cost_usd)}${partial ? ' partial' : ''}`,
      title: why,
    }
  })
  const parallel = $derived(detail ? maxParallel(detail) : null)
  const supervised = $derived(
    !!supervisor || (detail?.events ?? []).some((e) => e.type === 'supervisor_action' || e.type === 'wake_raised'),
  )
  const humanGate = $derived((detail?.model?.nodes ?? []).some((n) => n.type === 'human_review'))

  // The attempt the run is on, else the last one that ran.
  const current = $derived.by<CurrentAttempt | null>(() => {
    if (!detail) return null
    let node = stage && facts.get(stage)?.attempts ? stage : null
    if (!node) {
      for (let i = detail.events.length - 1; i >= 0; i--) {
        const e = detail.events[i]
        if (e.type === 'attempt_started' && e.node) {
          node = e.node
          break
        }
      }
    }
    if (!node) return null
    const f = facts.get(node)
    const d = runData.get(node) ?? {}
    const own = new Set(detail.events.filter((e) => e.node === node).map((e) => e.seq))
    const tail = logLines
      .filter((l) => own.has(l.seq))
      .slice(-6)
      .map((l) => l.message)
    return {
      node,
      profile: d.profile,
      agent: f?.agent,
      model: d.model ?? f?.model,
      attempt: d.attempt,
      progress: d.progress,
      duration: d.duration,
      status: detail.nodes[node],
      tail,
    }
  })

  const statusItems = $derived<StatusItem[]>(
    detail
      ? [
          { label: 'nodes', value: `${doneCount}/${steps.length}`, tone: 'info' },
          { label: 'stage', value: terminal ? 'end' : (stage ?? 'none'), tone: 'orange' },
          {
            label: 'parallel',
            value: `${runningCount}/${parallel?.value ?? 4}${parallel?.isDefault ? ' default' : ''}`,
            tone: 'info',
            title: parallel?.isDefault
              ? 'The playbook sets no max_parallel: the engine default 4, unless the run config set another'
              : 'max_parallel from the playbook defaults',
          },
          {
            label: 'paused',
            value: detail.pause?.pausing ? 'pausing' : status === 'paused' || detail.pause ? 'yes' : 'no',
            tone: detail.pause || status === 'paused' ? 'warning' : 'muted',
          },
          ...(decisionMode ? [{ label: 'decisions', value: decisionMode, tone: 'success' as const }] : []),
        ]
      : [],
  )

  // Fill the run fields the graph cards read (attempt, duration, progress,
  // note, and the model the attempt actually ran on, which wins over the
  // profile's current one) on top of toFlow's nodes, and give every edge its
  // run state (walked, active into the running node, idle).
  $effect(() => {
    const data = runData
    nodes = withProfileModels(flowNodes, profileModels).map((n) => {
      const { profile: _profile, model, ...rest } = data.get(n.id) ?? {}
      const m = model ?? n.data.model
      return { ...n, data: { ...n.data, ...rest, ...(m ? { model: m } : {}) } }
    })
  })
  $effect(() => {
    const d = detail
    if (!d) return
    const state = edgeRunState(d.events, d.nodes)
    edges = flowEdges.map((e) => ({ ...e, data: { ...e.data, state: state(e.source, e.target) } }))
  })

  // A running run's ages and progress move with the clock, not only with
  // journal changes; a finished run's "started ... ago" still ages, slower.
  $effect(() => {
    const t = setInterval(() => (now = Date.now()), terminal ? 60_000 : 10_000)
    return () => clearInterval(t)
  })

  // `nonce` answers a publish gate: the nonce of the plan shown.
  async function decide(node: string, decision: string, nonce?: string) {
    deciding = `${node}:${decision}`
    try {
      await postReview(id, node, decision, '', workspace, nonce)
      await load()
    } catch (e) {
      // A publish decision fails when the plan moved on (a stale nonce): load
      // the run again so the panel shows the current plan and its nonce.
      toast.error(nonce ? 'Publish decision failed' : 'Review failed', {
        description: String(e),
      })
      if (nonce) await load().catch(() => {})
    } finally {
      deciding = null
    }
  }

  async function answer(node: string, value: string) {
    answering = true
    try {
      await postAnswer(id, { node, answer: value }, workspace)
      await load()
    } catch (e) {
      toast.error('Answer failed', { description: String(e) })
    } finally {
      answering = false
    }
  }

  // A grading request answers for the run it was made on: once the page
  // moved to another run it neither reports nor unlocks that run's buttons
  // (the route effect resets `grading`).
  // A model grade that stored nothing (`skipped`) says why as a warning.
  async function gradeRequest(
    work: () => Promise<{ done: string; warning?: string; skipped?: boolean }>,
    failed: string,
  ) {
    const target = loadTarget()
    const stillHere = () => target === loadTarget()
    grading = true
    try {
      const { done, warning, skipped } = await work()
      if (!stillHere()) return
      if (skipped) {
        toast.warning(done, { description: warning })
        return
      }
      toast.success(done)
      if (warning) toast.warning('The candidate was not re-decided', { description: warning })
      await load()
    } catch (e) {
      if (stillHere()) toast.error(failed, { description: String(e) })
    } finally {
      if (stillHere()) grading = false
    }
  }

  function grade(grader: GraderChoice) {
    return gradeRequest(async () => {
      const graded = await gradeRun(id, grader, workspace)
      const by = grader === 'model' ? 'the decision model' : 'rules'
      if (graded.skipped) {
        return { done: `Not graded by ${by}`, warning: gradeSkippedText(graded.skipped), skipped: true }
      }
      const after = graded.candidate ? `; candidate: ${verdictText(graded.candidate.verdict)}` : ''
      return { done: `Graded by ${by}${after}`, warning: graded.candidate_error }
    }, 'Grading failed')
  }

  // A run takes one host grade: a person lets a host grade it once more.
  function reopenGrading() {
    return gradeRequest(async () => {
      await reopenRunGrading(id, workspace)
      return { done: 'A host may grade this run once more' }
    }, 'Could not reopen the grading')
  }

  // A citation opens its journal event in the events list.
  async function showCite(seq: number) {
    tab = 'events'
    citedSeq = seq
    await tick()
    document.querySelector(`[data-event-seq="${seq}"]`)?.scrollIntoView({ block: 'center' })
  }

  // The run a load was started for: an answer that comes back after the
  // page moved to another run (back/forward, a change notice) is dropped.
  const loadTarget = () => `${workspace}\u0000${id}`

  // Bumped on every route change: a response for the previous run is dropped.
  let routeSeq = 0

  async function load() {
    const seq = routeSeq
    try {
      const d = await fetchRun(id, workspace)
      if (seq !== routeSeq) return
      detail = d
      loadError = null
      now = Date.now()
      if (d.model) {
        const flow = toFlow(
          d.model,
          d.layout,
          d.nodes,
          cachedNodeIds(d.events),
          false,
          listProgressByNode(d.lists),
        )
        flowNodes = flow.nodes
        flowEdges = flow.edges
      }
    } catch (e) {
      if (seq !== routeSeq) return
      // Nothing on screen yet: say why in place of the page. A failed reload
      // of a run already shown keeps it, toasts, and still refreshes the
      // report and the grades below.
      if (!detail) {
        loadError = String(e)
        return
      }
      toast.error('Failed to load run', { description: String(e) })
    }
    try {
      const r = await fetchRunReport(id, workspace)
      if (seq === routeSeq) report = r.report
    } catch {
      if (seq === routeSeq) report = null
    }
    try {
      const g = (await fetchRunGrades(id, workspace)).grades
      if (seq === routeSeq) grades = g
    } catch {
      if (seq === routeSeq) grades = []
    }
  }

  function retry() {
    loadError = null
    void load()
  }

  function showReport() {
    tab = 'report'
    const reduce = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches
    logBox?.scrollIntoView({ block: 'nearest', behavior: reduce ? 'auto' : 'smooth' })
  }

  $effect(() => {
    // Track the route target so browser back/forward between two runs (or a
    // click to a child run) reloads. Clear the previous run first: its graph
    // must not stay on screen, nor be what the new view fits itself to.
    void id
    void workspace
    routeSeq++
    detail = null
    loadError = null
    report = null
    flowNodes = []
    flowEdges = []
    grades = []
    grading = false
    citedSeq = null
    load()
    return subscribeChanges(load)
  })

  $effect(() => {
    const ws = workspace
    let cancelled = false
    fetchProfiles()
      .then((ps) => {
        if (!cancelled) profileModels = profileModelMap(ps, ws)
      })
      .catch(() => {
        if (!cancelled) profileModels = new Map()
      })
    return () => {
      cancelled = true
    }
  })

  // The project chip: cleared at once on a route change, filled only by the
  // answer for the current workspace.
  $effect(() => {
    const ws = workspace
    projectName = ''
    if (!ws) return
    let cancelled = false
    fetchProjects()
      .then((ps) => {
        if (!cancelled) projectName = ps.find((p) => p.workspace_id === ws)?.name ?? ''
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
  })

  const stepMark = { done: 'done', current: 'cur', failed: 'fail', skipped: 'skip', pending: 'pend' } as const
</script>

<Topbar active="runs" project={projectName} />

{#if detail}
  <RunLegend {profiles} decides={!!detail.decisions} {humanGate} />
{/if}

{#if detail}
  <div
    class="grid min-h-0 flex-1 grid-cols-1 gap-x-4 gap-y-5 overflow-auto p-3 sm:p-4 lg:grid-cols-[270px_minmax(0,1fr)_340px] lg:grid-rows-[minmax(0,1fr)_190px] lg:overflow-hidden xl:grid-cols-[290px_minmax(0,1fr)_360px]"
  >
    <!-- Left rail: who and where, the steps, counters and controls. -->
    <div class="flex flex-col pt-2 lg:min-h-0 lg:overflow-auto">
      <section class="box flex flex-1 flex-col gap-3 px-4 pt-4 pb-4" style:--k="var(--c-violet)" data-testid="run-rail">
        <span class="box-cap">run</span>
        <header class="flex flex-col items-center gap-0.5 text-center">
          <h1 class="hd m-0 text-[15px]" style:color={statusColor(status)}>
            Run · <span class={runStatusClass(status) ? '' : 'text-muted-foreground'}>{status}</span>
          </h1>
          <div class="max-w-full truncate text-xs text-fg-2" title={`${detail.playbook}@${detail.version}`}>
            {detail.playbook}@{detail.version}
          </div>
          <div class="max-w-full truncate text-[11px] text-muted-foreground" title={id}>
            run {id}{#if startedTs} · started {formatAge(startedTs, now)}{/if}
          </div>
          <div class="mt-1 flex flex-wrap justify-center gap-1">
            {#if detail.pause?.pausing}
              <span class="tag text-warning" title={pauseLine(detail.pause)} data-testid="run-pause-badge">pausing</span>
            {/if}
            {#if showsDriverDead(detail.run_status, detail.driver_alive === false)}
              <span class="tag text-warning">needs resume</span>
            {/if}
            {#if detail.execution === 'host'}
              <span
                class="tag text-teal"
                title="Host execution mode: the host session runs every agent step with its own subagents; no agent CLI is spawned"
                data-testid="execution-host">host mode</span
              >
            {:else if detail.execution_fallback}
              <span
                class="tag text-teal"
                title="A step whose CLIs cannot start is handed to the host session"
                data-testid="execution-fallback">host fallback</span
              >
            {/if}
          </div>
        </header>

        <RunProgress progress={detail.progress} status={detail.run_status} runKey={`${workspace}/${id}`} />

        <!-- Why a failed run ended. The answer below is what the playbook
             composed on purpose; this is what the engine recorded when it
             could not finish (also `apb doctor --run`). -->
        {#if detail.failure_reason}
          <div class="border border-destructive/50 bg-destructive/10 px-2.5 py-2">
            <div class="flex items-center gap-1.5 text-[11px] font-semibold tracking-[0.08em] text-destructive uppercase">
              <TriangleAlert class="size-3.5" />
              Failed
            </div>
            <pre class="m-0 mt-1 max-h-48 overflow-auto text-xs break-words whitespace-pre-wrap">{detail.failure_reason}</pre>
          </div>
        {/if}

        {#if steps.length}
          <ol class="steps" aria-label="Steps">
            {#each steps as s (s.id)}
              <li class={stepMark[s.state]} title={`${s.title} · ${s.kind} · ${s.status}`}>
                <span class="truncate">{s.id}</span>
                {#if s.state !== 'pending'}<span class="sr-only"> ({s.status})</span>{/if}
              </li>
            {/each}
          </ol>
        {/if}

        {#if detail.answer}
          <div>
            <div class="text-muted-foreground">answer:</div>
            <pre class="m-0 mt-1 max-h-48 overflow-auto text-xs break-words whitespace-pre-wrap text-green">{detail.answer}</pre>
          </div>
        {:else if note}
          <div>
            <div class="text-muted-foreground">last note{note.node ? ` · ${note.node}` : ''}:</div>
            <p class="m-0 mt-1 line-clamp-4 text-xs text-amber" title={note.text}>&raquo; {note.text}</p>
          </div>
        {/if}

        <dl class="kv m-0">
          <dt>attempts</dt>
          <dd>{attemptsTotal}</dd>
          {#if detail.usage}
            <dt>tokens</dt>
            <dd title={usageLine ?? ''}>
              {formatTokens(
                detail.usage.input_tokens +
                  detail.usage.output_tokens +
                  detail.usage.cache_read_tokens +
                  detail.usage.cache_write_tokens,
              )}
            </dd>
            {#if costNote}
              <dt>cost</dt>
              <dd title={costNote.title}>{costNote.text}</dd>
            {/if}
          {/if}
          {#if detail.decisions}
            <dt>decisions</dt>
            <dd
              class="text-green"
              title={detail.decisions.cost_estimated ? 'Estimated from list prices: a provider reported no cost' : 'Reported by the provider'}
            >
              {detail.decisions.cost_estimated ? '~' : ''}{formatCost(detail.decisions.cost_usd)}
            </dd>
          {/if}
          <dt>supervisor</dt>
          <dd class={supervisor ? 'text-warning' : supervised ? 'text-violet' : 'text-muted-foreground'}>
            {supervisor ? 'waiting' : supervised ? 'attached' : 'none'}
          </dd>
          <dt>execution</dt>
          <dd class="text-muted-foreground">{detail.execution}{detail.execution_fallback ? ' + host fallback' : ''}</dd>
          <!-- The run's working tree (issue #67 item 8): where its agent and
               script nodes run and what its busy lock covers. Absent: the
               project root. -->
          <dt>worktree</dt>
          <dd class="truncate text-muted-foreground" title={detail.worktree ?? 'project root'}>
            {detail.worktree ? detail.worktree.split('/').filter(Boolean).pop() : 'project root'}
          </dd>
        </dl>

        <!-- Token usage the run's agent attempts reported (issue #167); absent
             when no attempt's CLI reported any. -->
        {#if usageLine}
          <p class="m-0 text-[11px] text-muted-foreground" data-testid="run-usage">tokens: {usageLine}</p>
        {/if}
        {#if detail.worktree}
          <p class="m-0 text-[11px] break-all text-muted-foreground">working tree: {detail.worktree}</p>
        {/if}

        <!-- Events a newer apb wrote that this binary skipped: the run still
             reads, but the journal views leave those events out. -->
        {#if unknownNote}
          <p class="m-0 flex items-center gap-1.5 text-[11px] text-muted-foreground" data-testid="run-unknown-events">
            <TriangleAlert class="size-3.5 shrink-0" />
            <span title="This journal has events of a type this apb does not know; upgrade apb to see them.">{unknownNote}</span>
          </p>
        {/if}

        <div class="mt-auto flex flex-col gap-2 pt-1">
          <RunPauseControls
            {id}
            {workspace}
            status={detail.run_status}
            pause={detail.pause ?? null}
            budget={detail.budget ?? null}
            driverDead={detail.driver_alive === false}
            playbookId={detail.playbook}
            onchange={load}
          />
          <!-- Who hears about the run, and the report as one page to keep. -->
          <RunNotifyControl
            {id}
            {workspace}
            status={detail.run_status}
            notify={detail.notify ?? null}
            onchange={load}
          >
            {#snippet actions()}
              <a
                class="save-report"
                href={runReportHtmlUrl(id, workspace)}
                download
                data-testid="run-save-report"
                title="The run report as one self-contained HTML page: no script, no network">Save report</a
              >
            {/snippet}
          </RunNotifyControl>
          <div class="flex gap-1.5">
            <Button variant="outline" size="sm" onclick={showReport} data-testid="run-report-button">Report</Button>
          </div>
        </div>
      </section>
    </div>

    <!-- Center: the graph. -->
    <div class="relative h-[60vh] min-h-80 min-w-0 border border-line lg:mt-2 lg:h-auto lg:min-h-0">
      <SvelteFlow
        bind:nodes
        bind:edges
        {nodeTypes}
        {edgeTypes}
        nodesDraggable={false}
        nodesConnectable={false}
        elementsSelectable={false}
      >
        <Background />
        <Controls />
        <FitTop {nodes} key={`${workspace}/${detail.run_id}`} />
      </SvelteFlow>
    </div>

    <!-- Right column: what needs a person, then the decision layer, the
         attempt and what the run produced. -->
    <div class="flex flex-col gap-5 pt-2 lg:min-h-0 lg:overflow-auto lg:pr-1">
      {#each questions as q (q.node)}
        <QuestionPanel question={q} posting={answering} onAnswer={answer} />
      {/each}

      <PublishPanel runId={id} views={detail.publish} gates={publishes} posting={deciding} onDecide={decide} />

      {#if pending.length}
        <section class="box glow flex flex-col gap-3 px-4 pt-4 pb-3" style:--k="var(--c-amber)">
          <span class="box-cap">Human review</span>
          {#each pending as pr (pr.node)}
            <div class="flex flex-col gap-1">
              <span class="text-xs text-amber">{pr.node}</span>
              {#if pr.prompt}
                <pre class="prose-text m-0 text-sm break-words whitespace-pre-wrap">{pr.prompt}</pre>
              {/if}
              {#if pr.recommendation}
                <p class="m-0 text-xs text-muted-foreground" data-testid="review-recommendation">{pr.recommendation}</p>
              {/if}
              <div class="flex flex-wrap gap-1">
                {#each pr.options as opt (opt)}
                  <Button
                    variant="outline"
                    size="sm"
                    class="h-7"
                    onclick={() => decide(pr.node, opt)}
                    disabled={deciding === `${pr.node}:${opt}`}
                  >
                    {opt}
                  </Button>
                {/each}
              </div>
            </div>
          {/each}
        </section>
      {/if}

      {#if supervisor}
        <section class="box glow flex flex-col gap-2 px-4 pt-4 pb-3 text-xs" style:--k="var(--c-amber)">
          <span class="box-cap">Waiting for supervisor</span>
          <div>
            <span class="text-amber">{supervisor.node}</span>
            <span class="text-muted-foreground"> · {supervisor.trigger}</span>
          </div>
          {#if supervisor.instruction}
            <p class="m-0 text-muted-foreground">{supervisor.instruction}</p>
          {/if}
          {#if supervisor.options.length}
            <div class="flex flex-wrap gap-1">
              {#each supervisor.options as opt (opt)}
                <span class="tag text-fg-2">{opt}</span>
              {/each}
            </div>
          {/if}
        </section>
      {/if}

      <HostTaskPanel tasks={hostTasks} />

      {#if waiting.length}
        <section class="box flex flex-col gap-1 px-4 pt-4 pb-3" style:--k="var(--c-amber)">
          <span class="box-cap">Waiting</span>
          {#each waiting as node (node)}
            <div class="text-xs">
              <span class="text-amber">{node}</span>
              <span class="text-muted-foreground"> · awaiting signal or timer</span>
            </div>
          {/each}
        </section>
      {/if}

      <RunVerdictPanel outcome={detail.outcome} />
      {#key `${workspace}/${id}`}
        <ListProgressPanel lists={detail.lists} />
      {/key}
      <DecisionsPanel decisions={detail.decisions} events={detail.events} />
      <AttemptModelsPanel models={detail.attempt_models} {current} />
      <AttemptIsolationPanel rows={detail.isolation} />
      <RunBudgetPanel budget={detail.budget ?? null} estimate={detail.estimate ?? null} />
      <RunOutcomePanel goal={detail.goal} commits={detail.commits} runId={id} />
      <QualityPanel
        {grades}
        events={detail.events}
        ended={isTerminalRunStatus(detail.run_status)}
        {grading}
        onGrade={grade}
        onReopen={reopenGrading}
        onCite={showCite}
      />
      {#if hasRunProblems(detail.known_problems)}
        <RunKnownProblemsCard problems={detail.known_problems} {workspace} playbook={detail.playbook} />
      {/if}

      {#if children.length}
        <section class="box flex flex-col gap-1.5 px-4 pt-4 pb-3">
          <span class="box-cap">Child runs</span>
          {#each children as c (c.run_id)}
            <div class="flex flex-wrap items-center gap-2 text-xs">
              <a
                href={`#/run/${encodeURIComponent(workspace)}/${encodeURIComponent(c.run_id)}`}
                class="text-foreground hover:underline"
              >
                {c.node_id}
              </a>
              <span class="tag" style:color={statusColor(c.status)}>{c.status}</span>
            </div>
          {/each}
        </section>
      {/if}

      {#if detail.candidate_trial}
        <section class="box px-4 pt-4 pb-3 text-xs" style:--k="var(--c-pink)">
          <span class="box-cap">Candidate trial</span>
          <span>{candidateTrialLabel(detail.candidate_trial)}</span>
          {#if detail.candidate_trial.reason}
            <span class="text-muted-foreground"> · {detail.candidate_trial.reason}</span>
          {/if}
        </section>
      {/if}

      {#if hookEntries.length}
        <section class="box flex flex-col gap-2 px-4 pt-4 pb-3">
          <span class="box-cap">Webhooks</span>
          {#each hookEntries as [key, path] (key)}
            <div>
              <span class="text-xs font-semibold">{key}</span>
              <code class="block text-[11px] break-all text-muted-foreground">{location.origin}{path}</code>
            </div>
          {/each}
        </section>
      {/if}

      {#if detail.instruction}
        <section class="box px-4 pt-4 pb-3">
          <span class="box-cap">Run input</span>
          <pre class="prose-text m-0 max-h-60 overflow-auto text-xs break-words whitespace-pre-wrap text-muted-foreground">{detail.instruction}</pre>
        </section>
      {/if}
    </div>

    <!-- Bottom: the run log, the supervisor journal and the report. -->
    <section
      bind:this={logBox}
      class="box flex h-72 flex-col px-4 pt-3 pb-2 lg:col-span-3 lg:h-auto lg:min-h-0"
      data-testid="run-log-box"
    >
      <span class="box-cap">run log</span>
      <Tabs.Root bind:value={tab} class="flex min-h-0 flex-1 flex-col gap-1">
        <Tabs.List class="absolute -top-3 right-3 bg-background px-1 group-data-horizontal/tabs:h-6">
          <Tabs.Trigger value="events">events</Tabs.Trigger>
          <Tabs.Trigger value="journal">
            journal{#if journal.length}&nbsp;({journal.length}){/if}
          </Tabs.Trigger>
          <Tabs.Trigger value="report">report</Tabs.Trigger>
        </Tabs.List>

        <Tabs.Content value="events" class="min-h-0 flex-1">
          <RunLog lines={logLines} cited={citedSeq} />
        </Tabs.Content>

        <Tabs.Content value="journal" class="min-h-0 flex-1 overflow-auto">
          {#if journal.length}
            <ol class="m-0 flex list-none flex-col p-0">
              {#each journal as entry (entry.seq)}
                <li class="grid grid-cols-[64px_minmax(0,1fr)] gap-3 py-px text-xs sm:grid-cols-[72px_minmax(0,1fr)]">
                  <span class={entry.kind === 'wake' ? 'text-amber' : 'text-violet'}>{entry.kind}</span>
                  <span class="min-w-0">
                    <span class="font-semibold">{entry.label}</span>
                    {#if entry.node}<span class="text-muted-foreground"> {entry.node}</span>{/if}
                    {#if entry.detail}<span class="text-fg-2"> · {entry.detail}</span>{/if}
                  </span>
                </li>
              {/each}
            </ol>
          {:else}
            <p class="m-0 text-xs text-muted-foreground">no interventions yet</p>
          {/if}
        </Tabs.Content>

        <Tabs.Content value="report" class="min-h-0 flex-1 overflow-auto">
          {#if report}
            <pre class="m-0 text-xs break-words whitespace-pre-wrap">{report}</pre>
          {:else}
            <p class="m-0 text-xs text-muted-foreground">no report yet</p>
          {/if}
        </Tabs.Content>
      </Tabs.Root>
    </section>
  </div>

  <StatusBar items={statusItems}>
    {#snippet right()}
      <span class="max-w-[40vw] truncate" title={id}>run {id}</span>
    {/snippet}
  </StatusBar>
{:else if loadError}
  <div class="flex flex-1 flex-col items-center justify-center gap-3 px-4 text-center text-xs" role="alert">
    <span class="flex items-center gap-1.5 text-destructive">
      <TriangleAlert class="size-3.5" aria-hidden="true" />
      Failed to load run {id}
    </span>
    <span class="max-w-xl break-words text-muted-foreground">{loadError}</span>
    <Button variant="outline" size="sm" onclick={retry}>Retry</Button>
  </div>
{:else}
  <div class="flex flex-1 items-center justify-center text-xs text-muted-foreground">loading run {id}</div>
{/if}

<style>
  /* The rail's step list: a dashed spine with one glyph per step, so the
     state never rests on color alone: a check when done, a play mark on the
     current step, a cross when failed, a dash (and strike-through) when
     skipped, a hollow circle while pending. */
  .steps {
    list-style: none;
    margin: 0 0 0 6px;
    padding: 4px 0 0 4px;
    border-left: 1px dashed var(--c-faint);
  }
  .steps li {
    position: relative;
    display: flex;
    min-width: 0;
    padding: 2px 6px 2px 16px;
    font-size: 12px;
    color: var(--c-muted);
  }
  .steps li::before {
    position: absolute;
    left: -11px;
    top: 2px;
    width: 12px;
    text-align: center;
    background: var(--c-panel);
    font-size: 11px;
  }
  .steps li.pend::before {
    content: '\25CB';
    color: var(--c-muted);
  }
  .steps li.done {
    color: var(--c-fg-2);
  }
  .steps li.done::before {
    content: '\2713';
    color: var(--c-green);
  }
  .steps li.skip {
    color: var(--c-muted);
    text-decoration: line-through;
  }
  .steps li.skip::before {
    content: '\2013';
    color: var(--c-muted);
  }
  .steps li.cur {
    color: var(--c-fg);
    background: color-mix(in oklab, var(--c-violet) 18%, transparent);
  }
  .steps li.cur::before {
    content: '\25B6';
    color: var(--c-violet);
    font-size: 9px;
    top: 4px;
  }
  .steps li.fail {
    color: var(--c-red);
  }
  .steps li.fail::before {
    content: '\2715';
    color: var(--c-red);
  }
  /* The report as one page to keep: a link styled like an outline button. */
  .save-report {
    display: inline-flex;
    align-items: center;
    height: 28px;
    padding: 0 10px;
    border: 1px solid var(--c-line-2);
    color: var(--foreground);
    font-size: 12px;
    text-decoration: none;
    white-space: nowrap;
    transition: background-color 0.15s, color 0.15s;
  }
  .save-report:hover {
    background: var(--c-panel-2);
  }
  .save-report:focus-visible {
    outline: 2px solid var(--ring);
    outline-offset: 2px;
  }
</style>
