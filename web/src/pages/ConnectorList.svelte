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
  import PageScroll from '$lib/components/PageScroll.svelte'
  import ConnectorPickerDialog from '$lib/components/ConnectorPickerDialog.svelte'
  import PageHead from '$lib/components/PageHead.svelte'
  import StatusBar, { type StatusItem } from '$lib/components/StatusBar.svelte'
  import { profileColor } from '../lib/profilecolor'
  import { Button } from '$lib/components/ui/button'
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

  // Outlined tag color per trust tone.
  const toneText = {
    ok: 'text-green',
    warn: 'text-amber',
    danger: 'text-red',
    muted: 'text-muted-foreground',
  } as const

  const statusItems = $derived.by((): StatusItem[] => {
    const ready = items.reduce((n, c) => n + c.accountsReady, 0)
    const total = items.reduce((n, c) => n + c.accountsTotal, 0)
    return [
      { label: 'installed', value: items.length, tone: 'info' },
      {
        label: 'approved',
        value: items.filter((c) => c.trust === 'approved').length,
        tone: 'success',
      },
      {
        label: 'available',
        value: availableLoading ? '...' : availableFailed ? 'unknown' : available.length,
        tone: availableFailed ? 'warning' : 'violet',
      },
      { label: 'accounts ready', value: `${ready}/${total}`, tone: ready < total ? 'warning' : 'muted' },
    ]
  })

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

<PageScroll>
  <div class="mx-auto w-full max-w-6xl px-4 py-5 sm:px-5">
    <PageHead title="Connectors">
      {#snippet meta()}
        <span>installed on this machine</span>
        {#if loaded}
          <span class="text-muted-foreground">
            {items.length} installed{#if !availableLoading && !availableFailed}{` · ${available.length} available`}{/if}
          </span>
        {/if}
      {/snippet}
      {#snippet actions()}
        {#if loaded && listState === 'installed'}
          {#if addState.note}
            <span class="text-xs text-muted-foreground">{addState.note}</span>
          {/if}
          <Button
            size="sm"
            class="max-sm:px-2"
            disabled={addState.disabled}
            onclick={() => (pickerOpen = true)}
          >
            <Plus data-icon="inline-start" />
            <span class="max-sm:sr-only">Connect a connector</span>
          </Button>
        {/if}
      {/snippet}
    </PageHead>
    {#if !loaded}
      <div class="flex flex-col gap-2.5">
        {#each Array(3) as _, i (i)}<Skeleton class="h-16 w-full" />{/each}
      </div>
    {:else if listState === 'first-connect'}
      <Empty.Root class="border border-dashed border-line-2 bg-panel">
        <Empty.Header>
          <Empty.Media variant="icon" class="border border-line-2 bg-transparent text-muted-foreground">
            <Plug />
          </Empty.Media>
          <Empty.Title class="text-sm font-semibold">Connect your first connector</Empty.Title>
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
      <Empty.Root class="border border-dashed border-line-2 bg-panel">
        <Empty.Header>
          <Empty.Media variant="icon" class="border border-line-2 bg-transparent text-amber">
            <TriangleAlert />
          </Empty.Media>
          <Empty.Title class="text-sm font-semibold">Could not load available connectors</Empty.Title>
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
      <Empty.Root class="border border-dashed border-line-2 bg-panel">
        <Empty.Header>
          <Empty.Media variant="icon" class="border border-line-2 bg-transparent text-muted-foreground">
            <Plug />
          </Empty.Media>
          <Empty.Title class="text-sm font-semibold">No connectors installed</Empty.Title>
          <Empty.Description>
            There are no connectors available to connect right now. Connectors give playbook
            nodes call access to external services.
          </Empty.Description>
        </Empty.Header>
      </Empty.Root>
    {:else}
      <ul class="flex flex-col gap-2.5">
        {#each items as c (c.name)}
          {@const badge = trustBadge(c.trust)}
          <li
            class="relative grid grid-cols-[28px_minmax(0,1fr)_auto] items-center gap-x-4 gap-y-2 border border-line-2 bg-panel px-3.5 py-3 transition-colors hover:border-violet lg:grid-cols-[28px_minmax(0,2fr)_minmax(0,1fr)_150px_auto]"
          >
            <span
              class="grid size-7 place-items-center border border-(--k) text-(--k)"
              style:--k={profileColor(c.name)}
              aria-hidden="true"><Plug class="size-3.5" /></span
            >
            <div class="min-w-0">
              <div class="flex min-w-0 flex-wrap items-center gap-2">
                <a
                  href={detailHref(c.name)}
                  class="truncate font-semibold after:absolute after:inset-0 hover:underline focus-visible:outline-none focus-visible:after:outline focus-visible:after:outline-1 focus-visible:after:outline-violet"
                >
                  {c.displayName || c.name}
                </a>
                <span class="text-xs text-muted-foreground">v{c.version}</span>
                <span class="tag {toneText[badge.tone]}">{badge.label}</span>
                {#if c.updateAvailable}
                  <span class="tag text-amber">built-in v{c.updateAvailable} available</span>
                {/if}
              </div>
              {#if c.summary}
                <p class="prose-text truncate text-xs text-muted-foreground">{c.summary}</p>
              {/if}
            </div>
            <div class="col-span-full flex min-w-0 flex-wrap gap-1 lg:col-span-1">
              {#each c.tags as tag (tag)}
                <span class="tag text-fg-2">{tag}</span>
              {/each}
            </div>
            <div class="col-span-full text-xs whitespace-nowrap lg:col-span-1">
              <span class={c.accountsReady < c.accountsTotal ? 'text-amber' : 'text-fg-2'}>
                {c.accountsReady}/{c.accountsTotal}
              </span>
              <span class="text-muted-foreground">accounts ready</span>
            </div>
            <Button
              size="sm"
              variant="outline"
              href={detailHref(c.name)}
              class="relative z-10 col-start-3 row-start-1 lg:order-last lg:col-start-auto lg:row-start-auto"
            >
              View
            </Button>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
</PageScroll>

<StatusBar items={statusItems} />

<ConnectorPickerDialog
  bind:open={pickerOpen}
  {available}
  loading={availableLoading}
  failed={availableFailed}
  onretry={loadAvailable}
  href={detailHref}
/>
