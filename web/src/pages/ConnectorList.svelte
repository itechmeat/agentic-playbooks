<script lang="ts">
  import { fetchConnectors } from '../lib/api'
  import { trustBadge, type ConnectorCard } from '../lib/connectors'
  import { subscribeChanges } from '../lib/ws'
  import Topbar from '$lib/components/Topbar.svelte'
  import { Badge } from '$lib/components/ui/badge'
  import * as Card from '$lib/components/ui/card'
  import * as Empty from '$lib/components/ui/empty'
  import { Skeleton } from '$lib/components/ui/skeleton'
  import { toast } from 'svelte-sonner'
  import Plug from '@lucide/svelte/icons/plug'

  let items = $state<ConnectorCard[]>([])
  let loaded = $state(false)

  async function load() {
    try {
      items = await fetchConnectors()
    } catch (e) {
      toast.error('Failed to load connectors', { description: String(e) })
    } finally {
      loaded = true
    }
  }

  $effect(() => {
    load()
    return subscribeChanges(load)
  })

  const badgeClass = {
    ok: 'border-success/30 bg-success/15 text-success',
    warn: 'border-warning/30 bg-warning/15 text-warning',
    danger: 'border-destructive/30 bg-destructive/15 text-destructive',
    muted: '',
  } as const

  // `wsId` in App.svelte always expects a `<workspace>/<name>` pair (workspace
  // empty on the pinned-root server, same convention as #/playbook/<ws>/<id>).
  const detailHref = (c: ConnectorCard) => `#/connector//${encodeURIComponent(c.name)}`
</script>

<Topbar active="connectors" />

<div class="min-h-0 flex-1 overflow-auto">
  <div class="mx-auto w-full max-w-4xl px-4 py-6">
    {#if !loaded}
      <div class="flex flex-col gap-3">
        {#each Array(3) as _, i (i)}<Skeleton class="h-24 w-full" />{/each}
      </div>
    {:else if items.length === 0}
      <Empty.Root class="border border-dashed">
        <Empty.Header>
          <Empty.Media variant="icon"><Plug /></Empty.Media>
          <Empty.Title>No connectors installed</Empty.Title>
          <Empty.Description>
            Connectors give playbook nodes call access to external services.
          </Empty.Description>
        </Empty.Header>
      </Empty.Root>
    {:else}
      <div class="flex flex-col gap-3">
        {#each items as c (c.name)}
          {@const badge = trustBadge(c.trust)}
          <Card.Root class="transition-colors hover:border-ring/40">
            <Card.Header>
              <div class="flex flex-wrap items-center gap-2">
                <Card.Title class="text-base">{c.displayName || c.name}</Card.Title>
                <span class="font-mono text-xs text-muted-foreground">v{c.version}</span>
                <Badge variant="outline" class={badgeClass[badge.tone]}>{badge.label}</Badge>
              </div>
              <Card.Description>{c.summary}</Card.Description>
              <Card.Action>
                <a
                  href={detailHref(c)}
                  class="text-sm font-medium text-primary hover:underline"
                >
                  View
                </a>
              </Card.Action>
            </Card.Header>
            <Card.Content class="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
              {#each c.tags as tag (tag)}
                <Badge variant="secondary">{tag}</Badge>
              {/each}
              <span class="ml-auto">
                {c.accountsReady}/{c.accountsTotal} accounts ready
              </span>
            </Card.Content>
          </Card.Root>
        {/each}
      </div>
    {/if}
  </div>
</div>
