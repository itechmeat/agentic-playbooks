<script lang="ts">
  import { fetchRuns } from '../lib/api'
  import { filterByProject, projectFilter } from '../lib/projectfilter'
  import { groupRunsByParent } from '../lib/runlist'
  import { unknownEventsNote } from '../lib/runusage'
  import { formatAge } from '../lib/runview'
  import { formatDuration } from '../lib/format'
  import { candidateTrialLabel } from '../lib/versioninfo'
  import { subscribeChanges } from '../lib/ws'
  import type { RunSummary } from '../lib/types'
  import Topbar from '$lib/components/Topbar.svelte'
  import PageScroll from '$lib/components/PageScroll.svelte'
  import ProjectFilter from '$lib/components/ProjectFilter.svelte'
  import StatusBar, { type StatusItem } from '$lib/components/StatusBar.svelte'
  import RunProgress from '$lib/RunProgress.svelte'
  import * as Empty from '$lib/components/ui/empty'
  import { Skeleton } from '$lib/components/ui/skeleton'
  import { isTerminalRunStatus, showsDriverDead } from '../lib/status'
  import { statusColor } from '../lib/profilecolor'
  import { pauseLine, pauseMarker } from '../lib/pause'
  import { toast } from 'svelte-sonner'
  import PlayCircle from '@lucide/svelte/icons/play-circle'

  let items = $state<RunSummary[]>([])
  let loaded = $state(false)
  let now = $state(Date.now())

  const filtered = $derived(filterByProject(items, $projectFilter))

  const count = (pred: (s: string) => boolean) => filtered.filter((r) => pred(r.status.toLowerCase())).length
  const statusItems = $derived<StatusItem[]>([
    { label: 'runs', value: filtered.length, tone: 'violet' },
    { label: 'running', value: count((s) => s === 'running'), tone: 'info' },
    { label: 'paused', value: count((s) => s === 'paused'), tone: 'warning' },
    { label: 'interrupted', value: count((s) => s === 'interrupted'), tone: 'warning' },
    { label: 'failed', value: count((s) => s === 'failed'), tone: 'danger' },
    { label: 'aborted', value: count((s) => s === 'aborted'), tone: 'muted' },
  ])

  async function load() {
    try {
      // Reverse-chronological order otherwise puts a child run above its
      // parent (review R1-M4) - group so each child sits right after its
      // parent instead.
      items = groupRunsByParent(await fetchRuns())
      now = Date.now()
    } catch (e) {
      toast.error('Failed to load runs', { description: String(e) })
    } finally {
      loaded = true
    }
  }

  $effect(() => {
    load()
    return subscribeChanges(load)
  })

  // Ages and running durations move with the clock between list changes.
  $effect(() => {
    const t = setInterval(() => (now = Date.now()), 30_000)
    return () => clearInterval(t)
  })

  const href = (r: RunSummary) =>
    `#/run/${encodeURIComponent(r.workspace_id)}/${encodeURIComponent(r.run_id)}`

  // A finished run's bar: where its progress ended, in its status color.
  const endPercent = (r: RunSummary) =>
    r.progress?.percent ?? (r.status.toLowerCase().includes('succeed') ? 100 : 0)

  const cols =
    'md:grid md:grid-cols-[minmax(0,1.5fr)_minmax(0,1.4fr)_96px_92px_76px_minmax(120px,1fr)_minmax(0,0.7fr)] md:items-center md:gap-4'
</script>

<Topbar active="runs" />

<PageScroll>
  <div class="mx-auto w-full max-w-7xl px-4 py-6">
    <div class="mb-4 flex flex-wrap items-center gap-x-4 gap-y-2">
      <h1 class="hd m-0">Runs</h1>
      {#if loaded}<span class="text-xs text-muted-foreground">{filtered.length} {filtered.length === 1 ? 'run' : 'runs'}</span>{/if}
      <ProjectFilter {items} class="ml-auto" />
    </div>
    {#if !loaded}
      <div class="flex flex-col gap-2">
        {#each Array(4) as _, i (i)}<Skeleton class="h-12 w-full" />{/each}
      </div>
    {:else if filtered.length === 0}
      <Empty.Root class="border border-dashed">
        <Empty.Header>
          <Empty.Media variant="icon"><PlayCircle /></Empty.Media>
          <Empty.Title>No runs yet</Empty.Title>
          <Empty.Description>Start a playbook to see runs here.</Empty.Description>
        </Empty.Header>
      </Empty.Root>
    {:else}
      <div class="flex flex-col gap-2" role="table" aria-label="Runs">
        <div
          class="hidden px-3.5 text-[11px] tracking-[0.08em] text-muted-foreground uppercase {cols}"
          role="row"
        >
          <span role="columnheader">run</span>
          <span role="columnheader">playbook</span>
          <span role="columnheader">status</span>
          <span role="columnheader">started</span>
          <span role="columnheader">duration</span>
          <span role="columnheader">progress</span>
          <span role="columnheader">project</span>
        </div>
        {#each filtered as r (`${r.workspace_id}/${r.run_id}`)}
          {@const terminal = isTerminalRunStatus(r.status)}
          {@const live = r.status === 'running' && r.started_ts > 0}
          <div
            class="box relative flex flex-col gap-2 px-3.5 py-2.5 transition-colors hover:border-violet {cols}"
            role="row"
          >
            <div class="min-w-0 text-xs" class:pl-4={!!r.parent_run} class:border-l={!!r.parent_run} class:border-line-2={!!r.parent_run} role="cell">
              <!-- The run id link covers the whole row (after:inset-0), so
                   the row is one click target with a single real link. -->
              <a
                href={href(r)}
                class="block truncate font-semibold text-foreground after:absolute after:inset-0 hover:underline"
                title={r.run_id}
              >
                {r.run_id}
              </a>
              {#if r.parent_run}
                <span class="relative z-[1] block w-fit max-w-full truncate text-muted-foreground" title={r.parent_run}>child of {r.parent_run}</span>
              {/if}
              {#if r.superseded_by}
                <span class="relative z-[1] block w-fit max-w-full truncate text-muted-foreground" title={r.superseded_by}>superseded by {r.superseded_by}</span>
              {/if}
              {#if r.continued_from}
                <span class="relative z-[1] block w-fit max-w-full truncate text-muted-foreground" title={r.continued_from}>continued from {r.continued_from}</span>
              {/if}
            </div>
            <div class="relative z-[1] w-fit max-w-full min-w-0 truncate text-fg-2" role="cell" title={r.playbook}>{r.playbook}</div>
            <div class="relative z-[1] flex w-fit flex-wrap items-center gap-1" role="cell">
              <span class="tag" style:color={statusColor(r.status)}>
                {#if r.status === 'running'}<span class="spin"></span>{/if}{r.status}
              </span>
              {#if r.pause}
                <span class="tag text-warning" title={pauseLine(r.pause)} data-testid="run-pause-marker">
                  {pauseMarker(r.pause)}
                </span>
              {/if}
              {#if showsDriverDead(r.status, r.driver_dead)}
                <span class="tag text-warning">needs resume</span>
              {/if}
              {#if candidateTrialLabel(r.candidate_trial)}
                <span class="tag text-pink" title={r.candidate_trial?.reason ?? ''}>
                  {candidateTrialLabel(r.candidate_trial)}
                </span>
              {/if}
              {#if unknownEventsNote(r.unknown_events)}
                <span class="tag text-muted-foreground" title="This journal has events of a type this apb does not know">
                  {unknownEventsNote(r.unknown_events)}
                </span>
              {/if}
            </div>
            <div class="relative z-[1] w-fit text-xs text-fg-2" role="cell" title={new Date(r.started_ts).toLocaleString()}>
              <span class="text-muted-foreground md:hidden">started&nbsp;</span>{formatAge(r.started_ts, now)}
            </div>
            <!-- The list carries no end time: a finished run's duration is on
                 its page. A paused or interrupted run is not running, and its
                 wall time since start would count the pause, so only a running
                 run with a known start shows one. -->
            <div class={live ? 'text-xs text-fg-2' : 'hidden text-xs text-fg-2 md:block'} role="cell">
              {#if live}
                <span class="text-muted-foreground md:hidden">running&nbsp;</span>{formatDuration(now - r.started_ts)}
              {:else}
                <span class="text-muted-foreground" title="See the run page">-</span>
              {/if}
            </div>
            <div class="min-w-0" role="cell">
              {#if !terminal}
                <RunProgress progress={r.progress} status={r.status} runKey={`${r.workspace_id}/${r.run_id}`} />
              {:else}
                <div class="flex items-center gap-2">
                  <div class="hatch-bar w-full" style:--k={statusColor(r.status)} aria-hidden="true">
                    <i style:width="{endPercent(r)}%"></i>
                  </div>
                  <span class="shrink-0 text-xs text-fg-2 tabular-nums">{endPercent(r)}%</span>
                </div>
              {/if}
            </div>
            <div class="relative z-[1] w-fit max-w-full min-w-0 truncate text-xs text-muted-foreground" role="cell" title={r.project ?? ''}>{r.project ?? ''}</div>
          </div>
        {/each}
      </div>
    {/if}
  </div>
</PageScroll>

<StatusBar items={statusItems} />
