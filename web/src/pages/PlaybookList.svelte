<script lang="ts">
  import { deletePlaybook, fetchPlaybook, fetchPlaybooks, fetchRuns, fetchStats } from '../lib/api'
  import { formatDuration } from '../lib/format'
  import { storeDraftYaml, suggestDuplicateId } from '../lib/playbookdupe'
  import { ALL_PROJECTS, filterByProject, projectFilter } from '../lib/projectfilter'
  import {
    finishedCount,
    glyphLetter,
    medianRunTime,
    playbookKey,
    runHistory,
    runsByPlaybook,
    startedToday,
    successRate,
  } from '../lib/playbooklist'
  import { profileColor, statusColor } from '../lib/profilecolor'
  import { slowRefresh, subscribeChanges } from '../lib/ws'
  import type { PlaybookSummary, RunSummary } from '../lib/types'
  import type { StatsReport } from '../lib/api.gen'
  import Topbar from '$lib/components/Topbar.svelte'
  import PageScroll from '$lib/components/PageScroll.svelte'
  import PageHead from '$lib/components/PageHead.svelte'
  import StatusBar, { type StatusItem } from '$lib/components/StatusBar.svelte'
  import ProjectFilter from '$lib/components/ProjectFilter.svelte'
  import SuggestionsSection from '$lib/components/SuggestionsSection.svelte'
  import { Button } from '$lib/components/ui/button'
  import * as Empty from '$lib/components/ui/empty'
  import * as AlertDialog from '$lib/components/ui/alert-dialog'
  import { Skeleton } from '$lib/components/ui/skeleton'
  import { toast } from 'svelte-sonner'
  import Plus from '@lucide/svelte/icons/plus'
  import Copy from '@lucide/svelte/icons/copy'
  import Trash2 from '@lucide/svelte/icons/trash-2'
  import BookMarked from '@lucide/svelte/icons/book-marked'

  let items = $state<PlaybookSummary[]>([])
  let runs = $state<RunSummary[]>([])
  let runsLoaded = $state(false)
  let runsFailed = $state(false)
  let loaded = $state(false)
  let deleting = $state<string | null>(null)
  let target = $state<PlaybookSummary | null>(null)
  let confirmOpen = $state(false)

  // Median run times come from one project-wide stats report per project
  // (not one request per playbook), reloaded only when another run of that
  // project finished. A failed report waits STATS_RETRY_MS before the next try.
  let stats = $state<Record<string, StatsReport>>({})
  const statsFor = new Map<string, number>()
  const statsSeq = new Map<string, number>()
  const statsFailedAt = new Map<string, number>()
  const STATS_RETRY_MS = 60_000

  const filtered = $derived(filterByProject(items, $projectFilter))
  const filteredRuns = $derived(filterByProject(runs, $projectFilter))
  const runsOf = $derived(runsByPlaybook(runs))

  // Playbooks grouped by their owning project, so the global dashboard shows
  // affiliation instead of one flat mixed list.
  const groups = $derived.by(() => {
    const m = new Map<string, { key: string; project: string; items: PlaybookSummary[] }>()
    for (const w of filtered) {
      const k = w.workspace_id || '_'
      if (!m.has(k)) m.set(k, { key: k, project: w.project || 'this project', items: [] })
      m.get(k)!.items.push(w)
    }
    return [...m.values()].sort((a, b) => a.project.localeCompare(b.project))
  })

  // The project shown in the heading and the top bar: the filtered one, or
  // the only one there is.
  const projectLabel = $derived(
    groups.length === 1 && ($projectFilter !== ALL_PROJECTS || items.length > 0)
      ? groups[0].project
      : '',
  )

  const statusItems = $derived.by((): StatusItem[] => {
    const today = filteredRuns.filter((r) => startedToday(r)).length
    const running = filteredRuns.filter((r) => r.status === 'running').length
    const paused = filteredRuns.filter((r) => r.status === 'paused').length
    const frozen = filtered.filter((w) => w.frozen).length
    return [
      { label: 'playbooks', value: filtered.length, tone: 'info' },
      { label: 'runs today', value: today, tone: 'violet' },
      { label: 'running', value: running, tone: 'info' },
      { label: 'paused', value: paused, tone: 'warning' },
      { label: 'frozen', value: frozen, tone: frozen ? 'info' : 'muted' },
    ]
  })

  function loadStats(list: RunSummary[]) {
    const byWorkspace = new Map<string, RunSummary[]>()
    for (const r of list) byWorkspace.set(r.workspace_id, [...(byWorkspace.get(r.workspace_id) ?? []), r])
    for (const [ws, wsRuns] of byWorkspace) {
      const done = finishedCount(wsRuns)
      if (done === 0 || statsFor.get(ws) === done) continue
      const failedAt = statsFailedAt.get(ws)
      if (failedAt !== undefined && Date.now() - failedAt < STATS_RETRY_MS) continue
      statsFor.set(ws, done)
      const seq = (statsSeq.get(ws) ?? 0) + 1
      statsSeq.set(ws, seq)
      fetchStats('', ws)
        .then((r) => {
          if (statsSeq.get(ws) !== seq) return
          statsFailedAt.delete(ws)
          stats[ws] = r
        })
        .catch(() => {
          if (statsSeq.get(ws) !== seq) return
          // Forget the count so the next pass after the back-off retries.
          statsFor.delete(ws)
          statsFailedAt.set(ws, Date.now())
        })
    }
  }

  // The playbook listing is cheap (it carries each row's node count and
  // profiles), so it follows every change; a response older than the newest
  // request is dropped.
  let listSeq = 0
  async function load() {
    const seq = ++listSeq
    try {
      const pbs = await fetchPlaybooks()
      if (seq !== listSeq) return
      items = pbs
    } catch (e) {
      if (seq === listSeq) toast.error('Failed to load playbooks', { description: String(e) })
    } finally {
      if (seq === listSeq) loaded = true
    }
  }

  // The run listing reads every journal and can take seconds, so it never
  // holds up the list and never follows the change stream frame by frame: it
  // reloads at most once per RUNS_INTERVAL_MS and only while the tab is
  // visible. One request at a time; a change meanwhile asks for one more pass.
  const RUNS_INTERVAL_MS = 10_000
  let runsBusy = false
  let runsAgain = false
  async function loadRuns() {
    if (runsBusy) {
      runsAgain = true
      return
    }
    runsBusy = true
    try {
      do {
        runsAgain = false
        try {
          runs = await fetchRuns()
          runsLoaded = true
          runsFailed = false
          loadStats(runs)
        } catch {
          // The run columns keep their last values; the list itself is fine.
          if (!runsLoaded) runsFailed = true
        }
      } while (runsAgain)
    } finally {
      runsBusy = false
    }
  }

  $effect(() => {
    const slow = slowRefresh(() => void loadRuns(), RUNS_INTERVAL_MS)
    load()
    slow.request()
    const unsubscribe = subscribeChanges(() => {
      load()
      slow.request()
    })
    return () => {
      unsubscribe()
      slow.dispose()
    }
  })

  async function duplicate(w: PlaybookSummary) {
    try {
      const detail = await fetchPlaybook(w.id, w.workspace_id)
      storeDraftYaml(detail.yaml, suggestDuplicateId(w.id))
      location.hash = '#/new'
    } catch (e) {
      toast.error('Duplicate failed', { description: String(e) })
    }
  }

  function askRemove(w: PlaybookSummary) {
    target = w
    confirmOpen = true
  }

  async function confirmRemove() {
    const w = target
    if (!w) return
    confirmOpen = false
    deleting = key(w)
    try {
      await deletePlaybook(w.id, w.workspace_id)
      await load()
      toast.success(`Moved "${w.id}" to trash`)
    } catch (e) {
      toast.error('Delete failed', { description: String(e) })
    } finally {
      deleting = null
      target = null
    }
  }

  const key = (w: PlaybookSummary) => playbookKey(w.workspace_id, w.id)
  const href = (w: PlaybookSummary) =>
    `#/playbook/${encodeURIComponent(w.workspace_id)}/${encodeURIComponent(w.id)}`
  const runHref = (r: RunSummary) =>
    `#/run/${encodeURIComponent(r.workspace_id)}/${encodeURIComponent(r.run_id)}`
  const fmtTime = (ms: number) => new Date(ms).toLocaleString()

  // One grid for the column captions and every row, from lg up; below lg a
  // row stacks its facts under the name.
  const cols =
    'lg:grid-cols-[28px_minmax(220px,2.2fr)_72px_52px_minmax(120px,1.2fr)_150px_96px_72px_64px]'
</script>

<Topbar active="playbooks" project={projectLabel} />

<PageScroll>
  <div class="w-full px-4 py-5 sm:px-5">
    <SuggestionsSection />

    <PageHead title="Playbooks">
      {#snippet meta()}
        {#if projectLabel}<span>project · {projectLabel}</span>{/if}
        {#if loaded}
          <span class="text-muted-foreground">
            {filtered.length} playbook{filtered.length === 1 ? '' : 's'}{#if groups.length > 1}{` · ${groups.length} projects`}{/if}
          </span>
        {/if}
      {/snippet}
      {#snippet actions()}
        <ProjectFilter {items} class="" />
        <Button href="#/trash" variant="outline" size="sm" class="max-sm:px-2">
          <Trash2 data-icon="inline-start" />
          <span class="max-sm:sr-only">Trash</span>
        </Button>
        <Button href="#/new" size="sm" class="max-sm:px-2">
          <Plus data-icon="inline-start" />
          <span class="max-sm:sr-only">New playbook</span>
        </Button>
      {/snippet}
    </PageHead>

    {#if !loaded}
      <div class="flex flex-col gap-2.5">
        {#each Array(3) as _, i (i)}<Skeleton class="h-16 w-full" />{/each}
      </div>
    {:else if items.length === 0}
      <Empty.Root class="border border-dashed border-line-2 bg-panel">
        <Empty.Header>
          <Empty.Media variant="icon" class="border border-line-2 bg-transparent text-muted-foreground">
            <BookMarked />
          </Empty.Media>
          <Empty.Title class="text-sm font-semibold">No playbooks yet</Empty.Title>
          <Empty.Description>
            No playbooks found in any registered project.
          </Empty.Description>
        </Empty.Header>
        <Empty.Content>
          <Button href="#/new" size="sm">
            <Plus data-icon="inline-start" />
            Create playbook
          </Button>
        </Empty.Content>
      </Empty.Root>
    {:else}
      <div
        class="mb-2 hidden gap-4 px-3.5 text-[11px] tracking-[0.08em] text-muted-foreground uppercase lg:grid {cols}"
        aria-hidden="true"
      >
        <span></span><span>playbook</span><span>version</span><span>nodes</span><span>profiles</span>
        <span>last runs</span><span>success</span><span title="Median run time">median</span><span></span>
      </div>
      {#each groups as g (g.key)}
        <section class="mb-6">
          {#if groups.length > 1}
            <h2 class="mb-2 px-0.5 text-[11px] tracking-[0.08em] text-muted-foreground uppercase">
              {g.project}
            </h2>
          {/if}
          <ul class="flex flex-col gap-2.5">
            {#each g.items as w (key(w))}
              {@const k = key(w)}
              {@const pbRuns = runsOf.get(k)}
              {@const rate = successRate(pbRuns)}
              {@const median = medianRunTime(stats[w.workspace_id], w.current, w.id)}
              <li
                class="relative grid grid-cols-[28px_minmax(0,1fr)_auto] items-center gap-x-4 gap-y-2 border border-line-2 bg-panel px-3.5 py-3 transition-colors hover:border-violet {cols}"
              >
                <span
                  class="grid size-7 place-items-center border border-(--k) font-bold text-(--k)"
                  style:--k={profileColor(w.id)}
                  aria-hidden="true">{glyphLetter(w.name, w.id)}</span
                >
                <div class="min-w-0">
                  <div class="flex min-w-0 flex-wrap items-center gap-2">
                    <!-- The name's link covers the whole row (after:inset-0);
                         the run squares and the actions sit above it. -->
                    <a
                      href={href(w)}
                      class="truncate font-semibold after:absolute after:inset-0 hover:underline focus-visible:outline-none focus-visible:after:outline focus-visible:after:outline-1 focus-visible:after:outline-violet"
                    >
                      {w.name}
                    </a>
                    {#if w.frozen}
                      <span class="tag text-blue" title="Frozen: new versions are refused">frozen</span>
                    {/if}
                  </div>
                  <div class="truncate text-xs text-muted-foreground">
                    <span class="font-mono">{w.id}</span>{#if w.description}{' · '}<span
                        class="prose-text">{w.description}</span
                      >{/if}
                  </div>
                </div>

                <div
                  class="relative z-10 col-start-3 row-start-1 flex justify-end gap-0.5 lg:order-last lg:col-start-auto lg:row-start-auto"
                >
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    onclick={() => duplicate(w)}
                    title={`Duplicate as ${suggestDuplicateId(w.id)}`}
                    aria-label="Duplicate"
                  >
                    <Copy />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    class="hover:text-destructive"
                    onclick={() => askRemove(w)}
                    disabled={deleting === k}
                    title="Delete"
                    aria-label="Delete"
                  >
                    <Trash2 />
                  </Button>
                </div>

                <!-- The facts: one wrapped line under the name below lg, grid
                     cells from lg up (`lg:contents`). -->
                <div
                  class="col-span-full flex flex-wrap items-center gap-x-5 gap-y-2 text-xs lg:contents lg:text-[13px]"
                >
                  <span class="text-fg-2" title="Current version"><span class="text-muted-foreground lg:hidden">v</span>{w.current}</span>
                  <span class="text-fg-2" title="Nodes">
                    {#if w.node_count !== undefined}{w.node_count}<span class="text-muted-foreground lg:hidden">{' nodes'}</span>{:else}<span class="text-muted-foreground">-</span>{/if}
                  </span>
                  <span class="flex min-w-0 flex-wrap gap-x-2.5 gap-y-0.5 text-[11px] text-fg-2">
                    {#if w.profiles?.length}
                      {#each w.profiles as p (p)}
                        <span class="legend-sq whitespace-nowrap" style:--k={profileColor(p)}>{p}</span>
                      {/each}
                    {:else if w.profiles}
                      <span class="text-muted-foreground">no profiles</span>
                    {/if}
                  </span>
                  <!-- Mouse shortcuts to the last runs; out of the tab order (the
                       runs page lists them), so a row is not twelve tab stops. -->
                  <span class="relative z-10 flex gap-[3px]" title="Last runs, newest on the right">
                    {#each runHistory(pbRuns) as r, i (r?.run_id ?? `empty-${i}`)}
                      {#if r}
                        <a
                          href={runHref(r)}
                          tabindex="-1"
                          class="block h-4 w-[9px] opacity-85 hover:opacity-100 hover:outline hover:outline-1 hover:outline-foreground"
                          style:background={statusColor(r.status)}
                          title={`${r.status} · ${fmtTime(r.started_ts)} · ${r.run_id}`}
                          aria-label={`${r.status}, ${fmtTime(r.started_ts)}`}
                        ></a>
                      {:else}
                        <i class="block h-4 w-[9px] border border-line-2" aria-hidden="true"></i>
                      {/if}
                    {/each}
                  </span>
                  <span class="w-24 lg:w-auto">
                    {#if rate}
                      <span class="hatch-bar block" style:--k="var(--c-green)">
                        <i style:width={`${rate.pct}%`}></i>
                      </span>
                      <span class="mt-0.5 block text-[11px] text-fg-2" title={`${rate.ok} of ${rate.of} finished runs succeeded`}>
                        {rate.pct}% <span class="text-muted-foreground">{rate.ok}/{rate.of}</span>
                      </span>
                    {:else}
                      <span
                        class="text-[11px] whitespace-nowrap text-muted-foreground"
                        title={runsLoaded ? 'No finished runs' : runsFailed ? 'Runs could not be loaded' : 'Loading runs'}
                        >{runsLoaded || runsFailed ? '-' : '...'}</span
                      >
                    {/if}
                  </span>
                  <span
                    class="text-fg-2"
                    title={median ? `Median run time of v${median.version}` : undefined}
                  >
                    {#if median}{formatDuration(median.ms)}{:else}<span class="text-muted-foreground">-</span>{/if}
                  </span>
                </div>
              </li>
            {/each}
          </ul>
        </section>
      {/each}
    {/if}
  </div>
</PageScroll>

<StatusBar items={statusItems} />

<AlertDialog.Root bind:open={confirmOpen}>
  <AlertDialog.Content>
    <AlertDialog.Header>
      <AlertDialog.Title>Delete playbook?</AlertDialog.Title>
      <AlertDialog.Description>
        "{target?.id}" will be moved to the trash with all its versions. You can
        restore it from Trash.
      </AlertDialog.Description>
    </AlertDialog.Header>
    <AlertDialog.Footer>
      <AlertDialog.Cancel>Cancel</AlertDialog.Cancel>
      <AlertDialog.Action onclick={confirmRemove}>Delete</AlertDialog.Action>
    </AlertDialog.Footer>
  </AlertDialog.Content>
</AlertDialog.Root>
