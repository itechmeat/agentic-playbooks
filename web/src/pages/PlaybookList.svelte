<script lang="ts">
  import { deletePlaybook, fetchPlaybook, fetchPlaybooks } from '../lib/api'
  import { storeDraftYaml, suggestDuplicateId } from '../lib/playbookdupe'
  import { subscribeChanges } from '../lib/ws'
  import type { PlaybookSummary } from '../lib/types'
  import Topbar from '$lib/components/Topbar.svelte'
  import PageScroll from '$lib/components/PageScroll.svelte'
  import { Button } from '$lib/components/ui/button'
  import { Badge } from '$lib/components/ui/badge'
  import * as Card from '$lib/components/ui/card'
  import * as Empty from '$lib/components/ui/empty'
  import * as AlertDialog from '$lib/components/ui/alert-dialog'
  import { Skeleton } from '$lib/components/ui/skeleton'
  import { toast } from 'svelte-sonner'
  import Plus from '@lucide/svelte/icons/plus'
  import Copy from '@lucide/svelte/icons/copy'
  import Trash2 from '@lucide/svelte/icons/trash-2'
  import Snowflake from '@lucide/svelte/icons/snowflake'
  import BookMarked from '@lucide/svelte/icons/book-marked'

  let items = $state<PlaybookSummary[]>([])
  let loaded = $state(false)
  let deleting = $state<string | null>(null)
  let target = $state<PlaybookSummary | null>(null)
  let confirmOpen = $state(false)

  // Playbooks grouped by their owning project, so the global dashboard shows
  // affiliation instead of one flat mixed list.
  const groups = $derived.by(() => {
    const m = new Map<string, { key: string; project: string; items: PlaybookSummary[] }>()
    for (const w of items) {
      const k = w.workspace_id || '_'
      if (!m.has(k)) m.set(k, { key: k, project: w.project || 'this project', items: [] })
      m.get(k)!.items.push(w)
    }
    return [...m.values()].sort((a, b) => a.project.localeCompare(b.project))
  })

  async function load() {
    try {
      items = await fetchPlaybooks()
    } catch (e) {
      toast.error('Failed to load playbooks', { description: String(e) })
    } finally {
      loaded = true
    }
  }

  $effect(() => {
    load()
    return subscribeChanges(load)
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

  const key = (w: PlaybookSummary) => `${w.workspace_id}/${w.id}`
</script>

<Topbar active="playbooks">
  {#snippet actions()}
    <Button href="#/new" size="sm" class="max-sm:px-2">
      <Plus data-icon="inline-start" />
      <span class="max-sm:sr-only">Create</span>
    </Button>
  {/snippet}
</Topbar>

<PageScroll>
  <div class="mx-auto w-full max-w-4xl px-4 py-6">
    {#if !loaded}
      <div class="flex flex-col gap-3">
        {#each Array(3) as _, i (i)}<Skeleton class="h-24 w-full" />{/each}
      </div>
    {:else if items.length === 0}
      <Empty.Root class="border border-dashed">
        <Empty.Header>
          <Empty.Media variant="icon"><BookMarked /></Empty.Media>
          <Empty.Title>No playbooks yet</Empty.Title>
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
      {#each groups as g (g.key)}
        <section class="mb-8">
          <h2
            class="mb-3 text-xs font-semibold uppercase tracking-wider text-muted-foreground"
          >
            {g.project}
          </h2>
          <div class="flex flex-col gap-3">
            {#each g.items as w (key(w))}
              <Card.Root class="transition-colors hover:border-ring/40">
                <Card.Header>
                  <div class="flex flex-wrap items-center gap-2">
                    <Card.Title class="text-base">
                      <a
                        href={`#/playbook/${encodeURIComponent(w.workspace_id)}/${encodeURIComponent(w.id)}`}
                        class="hover:underline"
                      >
                        {w.name}
                      </a>
                    </Card.Title>
                    {#if w.frozen}
                      <Badge variant="outline" class="gap-1 border-info/30 bg-info/15 text-info">
                        <Snowflake class="size-3" />
                        frozen
                      </Badge>
                    {/if}
                  </div>
                  <Card.Description class="font-mono text-xs">
                    {w.id} · v{w.current}
                  </Card.Description>
                  <Card.Action class="flex gap-1">
                    <Button
                      variant="ghost"
                      size="sm"
                      class="max-sm:px-2"
                      onclick={() => duplicate(w)}
                      title={`Duplicate as ${suggestDuplicateId(w.id)}`}
                    >
                      <Copy data-icon="inline-start" />
                      <span class="max-sm:sr-only">Duplicate</span>
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      class="max-sm:px-2 text-muted-foreground hover:text-destructive"
                      onclick={() => askRemove(w)}
                      disabled={deleting === key(w)}
                    >
                      <Trash2 data-icon="inline-start" />
                      <span class="max-sm:sr-only">Delete</span>
                    </Button>
                  </Card.Action>
                </Card.Header>
                {#if w.description}
                  <Card.Content class="text-sm text-muted-foreground">
                    {w.description}
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

<AlertDialog.Root bind:open={confirmOpen}>
  <AlertDialog.Content>
    <AlertDialog.Header>
      <AlertDialog.Title>Delete playbook?</AlertDialog.Title>
      <AlertDialog.Description>
        "{target?.id}" will be moved to trash. You can restore it from disk if needed.
      </AlertDialog.Description>
    </AlertDialog.Header>
    <AlertDialog.Footer>
      <AlertDialog.Cancel>Cancel</AlertDialog.Cancel>
      <AlertDialog.Action onclick={confirmRemove}>Delete</AlertDialog.Action>
    </AlertDialog.Footer>
  </AlertDialog.Content>
</AlertDialog.Root>
