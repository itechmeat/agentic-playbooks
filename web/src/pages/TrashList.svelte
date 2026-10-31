<script lang="ts">
  import { ApiError, fetchTrash, restoreFromTrash } from '../lib/api'
  import { filterByProject, projectFilter } from '../lib/projectfilter'
  import { subscribeChanges } from '../lib/ws'
  import type { TrashListEntry } from '../lib/types'
  import Topbar from '$lib/components/Topbar.svelte'
  import PageScroll from '$lib/components/PageScroll.svelte'
  import ProjectFilter from '$lib/components/ProjectFilter.svelte'
  import PageHead from '$lib/components/PageHead.svelte'
  import StatusBar, { type StatusItem } from '$lib/components/StatusBar.svelte'
  import { Button } from '$lib/components/ui/button'
  import * as Empty from '$lib/components/ui/empty'
  import { Skeleton } from '$lib/components/ui/skeleton'
  import { toast } from 'svelte-sonner'
  import Trash2 from '@lucide/svelte/icons/trash-2'
  import Undo2 from '@lucide/svelte/icons/undo-2'
  import TriangleAlert from '@lucide/svelte/icons/triangle-alert'
  import ArrowLeft from '@lucide/svelte/icons/arrow-left'

  let items = $state<TrashListEntry[]>([])
  let loaded = $state(false)
  let restoring = $state<string | null>(null)

  const filtered = $derived(filterByProject(items, $projectFilter))

  // Deleted playbooks grouped by their project, like the playbook list.
  const groups = $derived.by(() => {
    const m = new Map<string, { key: string; project: string; items: TrashListEntry[] }>()
    for (const e of filtered) {
      const k = e.workspace_id || '_'
      if (!m.has(k)) m.set(k, { key: k, project: e.project || 'this project', items: [] })
      m.get(k)!.items.push(e)
    }
    return [...m.values()].sort((a, b) => a.project.localeCompare(b.project))
  })

  const statusItems = $derived.by((): StatusItem[] => {
    const conflicts = filtered.filter((e) => e.conflict).length
    return [
      { label: 'in trash', value: filtered.length, tone: 'info' },
      { label: 'id in use again', value: conflicts, tone: conflicts ? 'warning' : 'muted' },
    ]
  })

  const key = (e: TrashListEntry) => `${e.workspace_id}/${e.name}`

  function conflictMessage(e: TrashListEntry): string {
    return `A playbook "${e.id}" exists again in ${e.project || 'this project'}. Delete or rename it, then restore this one.`
  }

  async function load() {
    try {
      items = await fetchTrash()
    } catch (e) {
      toast.error('Failed to load the trash', { description: String(e) })
    } finally {
      loaded = true
    }
  }

  $effect(() => {
    load()
    return subscribeChanges(load)
  })

  async function restore(e: TrashListEntry) {
    restoring = key(e)
    try {
      const r = await restoreFromTrash(e.name, e.workspace_id)
      toast.success(`Restored "${r.id}"`, {
        description: `${r.versions.length} version${r.versions.length === 1 ? '' : 's'}, current ${r.current ?? '-'}`,
        action: {
          label: 'Open',
          onClick: () => {
            location.hash = `#/playbook/${encodeURIComponent(e.workspace_id)}/${encodeURIComponent(r.id)}`
          },
        },
      })
      await load()
    } catch (err) {
      if (err instanceof ApiError && err.status === 409) {
        // The id was taken again since the list was read: the reload flags the
        // entry, and its card then carries the same message.
        toast.error('Restore refused', { description: conflictMessage(e) })
        await load()
      } else {
        toast.error('Restore failed', { description: String(err) })
      }
    } finally {
      restoring = null
    }
  }

  const fmtTime = (ms: number) => new Date(ms).toLocaleString()
</script>

<Topbar active="playbooks" />

<PageScroll>
  <div class="mx-auto w-full max-w-5xl px-4 py-5 sm:px-5">
    <PageHead title="Trash">
      {#snippet meta()}
        <span>deleted playbooks</span>
        {#if loaded}
          <span class="text-muted-foreground">{filtered.length} in the trash</span>
        {/if}
      {/snippet}
      {#snippet actions()}
        <ProjectFilter {items} class="" />
        <Button href="#/" variant="outline" size="sm" class="max-sm:px-2">
          <ArrowLeft data-icon="inline-start" />
          <span class="max-sm:sr-only">Playbooks</span>
        </Button>
      {/snippet}
    </PageHead>
    {#if !loaded}
      <div class="flex flex-col gap-3">
        {#each Array(2) as _, i (i)}<Skeleton class="h-16 w-full" />{/each}
      </div>
    {:else if items.length === 0}
      <Empty.Root class="border border-dashed border-line-2 bg-panel">
        <Empty.Header>
          <Empty.Media variant="icon" class="border border-line-2 bg-transparent text-muted-foreground">
            <Trash2 />
          </Empty.Media>
          <Empty.Title class="text-sm font-semibold">The trash is empty</Empty.Title>
          <Empty.Description>
            A deleted playbook lands here with all its versions until you restore it.
          </Empty.Description>
        </Empty.Header>
      </Empty.Root>
    {:else}
      {#each groups as g (g.key)}
        <section class="mb-6">
          <h2 class="mb-2 px-0.5 text-[11px] tracking-[0.08em] text-muted-foreground uppercase">
            {g.project}
          </h2>
          <ul class="flex flex-col gap-2.5">
            {#each g.items as e (key(e))}
              <li
                class="flex flex-col gap-2 border bg-panel px-3.5 py-3 {e.conflict
                  ? 'border-amber/60'
                  : 'border-line-2'}"
              >
                <div class="flex flex-wrap items-center gap-x-4 gap-y-1">
                  <span class="min-w-0 truncate font-semibold">{e.id}</span>
                  {#if e.conflict}
                    <span class="tag text-amber">id in use again</span>
                  {/if}
                  <span class="text-xs text-muted-foreground">
                    deleted {fmtTime(e.deleted_at_ms)}
                  </span>
                  <span class="text-xs text-fg-2">
                    {e.versions.length} version{e.versions.length === 1 ? '' : 's'}{e.current
                      ? ` · current v${e.current}`
                      : ''}
                  </span>
                  <Button
                    variant="outline"
                    size="sm"
                    class="ml-auto max-sm:px-2"
                    onclick={() => restore(e)}
                    disabled={restoring === key(e)}
                  >
                    <Undo2 data-icon="inline-start" />
                    <span class="max-sm:sr-only">Restore</span>
                  </Button>
                </div>
                {#if e.conflict}
                  <p class="flex items-start gap-2 text-xs text-amber">
                    <TriangleAlert class="mt-0.5 size-3.5 shrink-0" />
                    <span class="prose-text">{conflictMessage(e)}</span>
                  </p>
                {/if}
              </li>
            {/each}
          </ul>
        </section>
      {/each}
    {/if}
  </div>
</PageScroll>

<StatusBar items={statusItems} />
