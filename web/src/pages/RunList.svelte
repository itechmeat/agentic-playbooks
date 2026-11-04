<script lang="ts">
  import { untrack } from 'svelte'
  import { fetchRunsSearch } from '../lib/api'
  import {
    fieldNames,
    fieldsText,
    isSearchRefusal,
    searchErrorText,
    searchFooter,
    type SearchTerm,
  } from '../lib/runsearch'
  import RunSearchFilter from '../lib/RunSearchFilter.svelte'
  import { ALL_PROJECTS, filterByProject, projectFilter } from '../lib/projectfilter'
  import { groupRunsByParent, needsLookLabel } from '../lib/runlist'
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
  // Search fields (U45): the terms applied, the names the unfiltered runs
  // offer, and what the last answer could not search. The committed terms
  // change only after a successful answer, so a refused search never leaves
  // a chip that matches nothing.
  let terms = $state<SearchTerm[]>([])
  let names = $state<string[]>([])
  let counts = $state({ not_indexed: 0, withheld: 0 })
  // not_indexed and withheld are counted over every workspace the server
  // listed, while the rows can be narrowed to one project client side, so
  // the footer only claims them while the listing is not narrowed.
  const footer = $derived(
    terms.length && $projectFilter === ALL_PROJECTS ? searchFooter(counts, true) : null,
  )
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

  // One sequence number per request: only the latest answer may touch the
  // page, so a slower older answer never overwrites a newer one.
  let loadSeq = 0
  async function load(requested?: SearchTerm[]) {
    const seq = ++loadSeq
    const wanted = requested ?? terms
    try {
      // Reverse-chronological order otherwise puts a child run above its
      // parent (review R1-M4) - group so each child sits right after its
      // parent instead.
      const answer = await fetchRunsSearch(wanted)
      if (seq !== loadSeq) return
      items = groupRunsByParent(answer.runs)
      counts = { not_indexed: answer.not_indexed, withheld: answer.withheld }
      // The names come from the unfiltered answer; while a filter is on,
      // new field names the filtered runs offer are merged in, so the
      // picker never waits for every term to be removed.
      if (!wanted.length) names = fieldNames(answer.runs)
      else names = [...new Set([...names, ...fieldNames(answer.runs)])]
      now = Date.now()
      terms = wanted
    } catch (e) {
      if (seq !== loadSeq) return
      // A refused search keeps the committed terms and says why, instead of
      // showing the request URL. Only a filtered request can be refused:
      // a first load, a websocket reload or a network failure is a plain
      // load failure, never a refusal.
      const refused = wanted.length > 0 && isSearchRefusal(e)
      toast.error(refused ? 'Search refused' : 'Failed to load runs', {
        description: searchErrorText(e),
      })
    } finally {
      if (seq === loadSeq) loaded = true
    }
  }

  $effect(() => {
    // The terms are read inside load; leaving them untracked here keeps the
    // effect to the initial load and the websocket, while the filter's
    // onChange drives the reloads for new terms.
    untrack(() => void load())
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
    <RunSearchFilter
      {names}
      {terms}
      {footer}
      onChange={(t) => {
        load(t)
      }}
    />
    {#if !loaded}
      <div class="flex flex-col gap-2">
        {#each Array(4) as _, i (i)}<Skeleton class="h-12 w-full" />{/each}
      </div>
    {:else if filtered.length === 0}
      <Empty.Root class="border border-dashed">
        <Empty.Header>
          <Empty.Media variant="icon"><PlayCircle /></Empty.Media>
          {#if terms.length}
            <Empty.Title>No run matches the search</Empty.Title>
            <Empty.Description>Remove a term to see more runs.</Empty.Description>
          {:else}
            <Empty.Title>No runs yet</Empty.Title>
            <Empty.Description>Start a playbook to see runs here.</Empty.Description>
          {/if}
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
          {@const live = r.status === 'running' && r.started_ts > 0 && !r.driver_dead}
          {@const look = needsLookLabel(r.needs_look)}
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
            <div class="relative z-[1] w-fit max-w-full min-w-0 truncate text-fg-2" role="cell" title={r.playbook}>
              {r.playbook}
              {#if fieldsText(r.search)}
                <span class="search-fields" data-testid="run-search-fields">{fieldsText(r.search)}</span>
              {/if}
            </div>
            <div class="relative z-[1] flex w-fit flex-wrap items-center gap-1" role="cell">
              <span class="tag" style:color={statusColor(r.status)}>
                {#if r.status === 'running'}<span class="spin"></span>{/if}{r.status}
              </span>
              {#if r.pause}
                <span class="tag text-warning" title={pauseLine(r.pause)} data-testid="run-pause-marker">
                  {pauseMarker(r.pause)}
                </span>
              {/if}
              {#if look}
                <span class="tag needs-look" title={look.title} data-testid="run-needs-look">{look.label}</span>
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
            <div class="relative z-[1] w-fit text-xs text-fg-2" role="cell" title={r.started_ts > 0 ? new Date(r.started_ts).toLocaleString() : undefined}>
              <span class="text-muted-foreground md:hidden">started&nbsp;</span>{r.started_ts > 0 ? formatAge(r.started_ts, now) : '-'}
            </div>
            <!-- A finished run's duration is its active time (pauses left
                 out) from the listing; a running run counts up from its
                 start, less the time it spent paused so far. A paused or
                 interrupted run, or one whose driver is gone, is not running,
                 so it shows none. -->
            <div class={['relative z-[1]', live || r.duration_ms !== undefined ? 'text-xs text-fg-2' : 'hidden text-xs text-fg-2 md:block']} role="cell">
              {#if live}
                <span class="text-muted-foreground md:hidden">running&nbsp;</span>{formatDuration(now - r.started_ts - (r.paused_ms ?? 0))}
              {:else if r.duration_ms !== undefined}
                <span class="text-muted-foreground md:hidden">took&nbsp;</span>{formatDuration(r.duration_ms)}
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

<style>
  .search-fields {
    display: block;
    overflow: hidden;
    color: var(--muted-foreground);
    font-size: 0.6875rem;
    text-overflow: ellipsis;
  }
  .needs-look {
    color: var(--warning);
  }
</style>
