<script lang="ts">
  import { fetchAvailableConnectors, fetchConnectors } from '../lib/api'
  import { trustBadge, type ConnectorCard } from '../lib/connectors'
  import {
    addButtonState,
    connectorListState,
    type AvailableConnector,
  } from '../lib/connectorinstall'
  import { connectorHref } from '../lib/route'
  import { subscribeChanges } from '../lib/ws'
  import Topbar from '$lib/components/Topbar.svelte'
  import ConnectorPickerDialog from '$lib/components/ConnectorPickerDialog.svelte'
  import { Badge } from '$lib/components/ui/badge'
  import { Button } from '$lib/components/ui/button'
  import * as Card from '$lib/components/ui/card'
  import * as Empty from '$lib/components/ui/empty'
  import { Skeleton } from '$lib/components/ui/skeleton'
  import { toast } from 'svelte-sonner'
  import Plug from '@lucide/svelte/icons/plug'
  import Plus from '@lucide/svelte/icons/plus'
  import RefreshCw from '@lucide/svelte/icons/refresh-cw'
  import TriangleAlert from '@lucide/svelte/icons/triangle-alert'

  let items = $state<ConnectorCard[]>([])
  let loaded = $state(false)
  let available = $state<AvailableConnector[]>([])
  let availableLoading = $state(true)
  // A failed /available fetch is tracked separately from an empty list: the
  // page must never claim "nothing to connect" because a request failed.
  let availableFailed = $state(false)
  let pickerOpen = $state(false)

  async function loadInstalled() {
    try {
      items = await fetchConnectors()
    } catch (e) {
      toast.error('Failed to load connectors', { description: String(e) })
    } finally {
      loaded = true
    }
  }

  async function loadAvailable() {
    availableLoading = true
    try {
      available = await fetchAvailableConnectors()
      availableFailed = false
    } catch {
      available = []
      availableFailed = true
    } finally {
      availableLoading = false
    }
  }

  function loadAll() {
    loadInstalled()
    loadAvailable()
  }

  $effect(() => {
    loadAll()
    return subscribeChanges(loadAll)
  })

  const badgeClass = {
    ok: 'border-success/30 bg-success/15 text-success',
    warn: 'border-warning/30 bg-warning/15 text-warning',
    danger: 'border-destructive/30 bg-destructive/15 text-destructive',
    muted: '',
  } as const

  // Connectors are installed machine-wide, so the route is just the name -
  // no workspace segment, unlike #/playbook/<ws>/<id>.
  const detailHref = connectorHref

  const listState = $derived(
    connectorListState({
      installedCount: items.length,
      availableCount: available.length,
      availableFailed,
    }),
  )
  const addState = $derived(
    addButtonState({ availableCount: available.length, availableFailed }),
  )
</script>

<Topbar active="connectors" />

<div class="min-h-0 flex-1 overflow-auto">
  <div class="mx-auto w-full max-w-4xl px-4 py-6">
    {#if !loaded}
      <div class="flex flex-col gap-3">
        {#each Array(3) as _, i (i)}<Skeleton class="h-24 w-full" />{/each}
      </div>
    {:else if listState === 'first-connect'}
      <Empty.Root class="border border-dashed">
        <Empty.Header>
          <Empty.Media variant="icon"><Plug /></Empty.Media>
          <Empty.Title>Connect your first connector</Empty.Title>
          <Empty.Description>
            Connectors give playbook nodes call access to external services.
          </Empty.Description>
        </Empty.Header>
        <Empty.Content>
          <Button onclick={() => (pickerOpen = true)}>
            <Plus data-icon="inline-start" />
            Connect a connector
          </Button>
        </Empty.Content>
      </Empty.Root>
    {:else if listState === 'available-failed'}
      <Empty.Root class="border border-dashed">
        <Empty.Header>
          <Empty.Media variant="icon"><TriangleAlert /></Empty.Media>
          <Empty.Title>Could not load available connectors</Empty.Title>
          <Empty.Description>
            No connectors are installed, and the list of connectors you could connect could
            not be loaded.
          </Empty.Description>
        </Empty.Header>
        <Empty.Content>
          <Button variant="outline" onclick={loadAvailable}>
            <RefreshCw data-icon="inline-start" />
            Retry
          </Button>
        </Empty.Content>
      </Empty.Root>
    {:else if listState === 'nothing-available'}
      <Empty.Root class="border border-dashed">
        <Empty.Header>
          <Empty.Media variant="icon"><Plug /></Empty.Media>
          <Empty.Title>No connectors installed</Empty.Title>
          <Empty.Description>
            There are no connectors available to connect right now. Connectors give playbook
            nodes call access to external services.
          </Empty.Description>
        </Empty.Header>
      </Empty.Root>
    {:else}
      <div class="flex flex-col gap-4">
        <div class="flex flex-col items-start gap-1">
          <Button
            class="max-sm:px-2"
            disabled={addState.disabled}
            onclick={() => (pickerOpen = true)}
          >
            <Plus data-icon="inline-start" />
            <span class="max-sm:sr-only">Connect a connector</span>
          </Button>
          {#if addState.note}
            <span class="text-xs text-muted-foreground">{addState.note}</span>
          {/if}
        </div>

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
                  <Button size="sm" variant="outline" href={detailHref(c.name)}>View</Button>
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
      </div>
    {/if}
  </div>
</div>

<ConnectorPickerDialog
  bind:open={pickerOpen}
  {available}
  loading={availableLoading}
  failed={availableFailed}
  onretry={loadAvailable}
  href={detailHref}
/>
