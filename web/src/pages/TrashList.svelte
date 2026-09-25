<script lang="ts">
  import { ApiError, fetchTrash, restoreFromTrash } from '../lib/api'
  import { filterByProject, projectFilter } from '../lib/projectfilter'
  import { subscribeChanges } from '../lib/ws'
  import type { TrashListEntry } from '../lib/types'
  import Topbar from '$lib/components/Topbar.svelte'
  import PageScroll from '$lib/components/PageScroll.svelte'
  import ProjectFilter from '$lib/components/ProjectFilter.svelte'
  import { Button } from '$lib/components/ui/button'
  import { Badge } from '$lib/components/ui/badge'
  import * as Card from '$lib/components/ui/card'
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

<Topbar active="playbooks">
  {#snippet title()}
    <span class="truncate text-sm font-semibold">Trash</span>
  {/snippet}
  {#snippet actions()}
    <Button href="#/" variant="outline" size="sm" class="max-sm:px-2">
      <ArrowLeft data-icon="inline-start" />
      <span class="max-sm:sr-only">Playbooks</span>
    </Button>
  {/snippet}
</Topbar>

<PageScroll>
  <div class="mx-auto w-full max-w-4xl px-4 py-6">
    <ProjectFilter {items} />
    {#if !loaded}
      <div class="flex flex-col gap-3">
        {#each Array(2) as _, i (i)}<Skeleton class="h-20 w-full" />{/each}
      </div>
    {:else if items.length === 0}
      <Empty.Root class="border border-dashed">
        <Empty.Header>
          <Empty.Media variant="icon"><Trash2 /></Empty.Media>
          <Empty.Title>The trash is empty</Empty.Title>
          <Empty.Description>
            A deleted playbook lands here with all its versions until you restore it.
          </Empty.Description>
        </Empty.Header>
      </Empty.Root>
    {:else}
      {#each groups as g (g.key)}
        <section class="mb-8">
          <h2
            class="mb-3 text-xs font-semibold uppercase tracking-wider text-muted-foreground"
          >
            {g.project}
          </h2>
          <div class="flex flex-col gap-3">
            {#each g.items as e (key(e))}
              <Card.Root>
                <Card.Header>
                  <div class="flex flex-wrap items-center gap-2">
                    <Card.Title class="font-mono text-base">{e.id}</Card.Title>
                    {#if e.conflict}
                      <Badge variant="outline" class="border-warning/40 text-warning">
                        id in use again
                      </Badge>
                    {/if}
                  </div>
                  <Card.Description class="text-xs">
                    Deleted {fmtTime(e.deleted_at_ms)} · {e.versions.length}
                    version{e.versions.length === 1 ? '' : 's'}{e.current
                      ? ` · current v${e.current}`
                      : ''}
                  </Card.Description>
                  <Card.Action>
                    <Button
                      variant="outline"
                      size="sm"
                      class="max-sm:px-2"
                      onclick={() => restore(e)}
                      disabled={restoring === key(e)}
                    >
                      <Undo2 data-icon="inline-start" />
                      <span class="max-sm:sr-only">Restore</span>
                    </Button>
                  </Card.Action>
                </Card.Header>
                {#if e.conflict}
                  <Card.Content>
                    <p class="flex items-start gap-2 text-sm text-warning">
                      <TriangleAlert class="mt-0.5 size-4 shrink-0" />
                      <span>{conflictMessage(e)}</span>
                    </p>
                  </Card.Content>
                {/if}
              </Card.Root>
            {/each}
          </div>
        </section>
      {/each}
    {/if}
  </div>
</PageScroll>
