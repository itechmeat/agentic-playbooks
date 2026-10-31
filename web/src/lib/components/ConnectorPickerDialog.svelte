<script lang="ts">
  import type { AvailableConnector } from '$lib/connectorinstall'
  import { Button } from '$lib/components/ui/button'
  import * as Dialog from '$lib/components/ui/dialog'
  import * as Empty from '$lib/components/ui/empty'
  import { Skeleton } from '$lib/components/ui/skeleton'
  import Plug from '@lucide/svelte/icons/plug'
  import RefreshCw from '@lucide/svelte/icons/refresh-cw'
  import TriangleAlert from '@lucide/svelte/icons/triangle-alert'

  let {
    open = $bindable(false),
    available,
    loading = false,
    failed = false,
    onretry,
    href,
  }: {
    open?: boolean
    available: AvailableConnector[]
    loading?: boolean
    failed?: boolean
    onretry: () => void
    href: (name: string) => string
  } = $props()
</script>

<Dialog.Root bind:open>
  <!-- Below `sm` the dialog fills the viewport instead of sitting as a small
       centered box, so a long list is usable on a phone. The centering
       transforms and the rounding are unset for that breakpoint only. -->
  <Dialog.Content
    class="max-h-[85dvh] grid-rows-[auto_minmax(0,1fr)] sm:max-w-lg max-sm:top-0 max-sm:left-0 max-sm:h-dvh max-sm:max-h-dvh max-sm:w-screen max-sm:max-w-none max-sm:translate-x-0 max-sm:translate-y-0 max-sm:rounded-none"
  >
    <Dialog.Header>
      <Dialog.Title>Connect a connector</Dialog.Title>
      <Dialog.Description class="prose-text text-xs">
        Official connectors that are not installed yet. Open one to review it and connect.
      </Dialog.Description>
    </Dialog.Header>

    <div class="min-h-0 overflow-auto">
      {#if loading}
        <div class="flex flex-col gap-3">
          {#each Array(3) as _, i (i)}<Skeleton class="h-16 w-full" />{/each}
        </div>
      {:else if failed}
        <Empty.Root class="border border-dashed border-line-2">
          <Empty.Header>
            <Empty.Media variant="icon" class="border border-line-2 bg-transparent text-muted-foreground"><TriangleAlert /></Empty.Media>
            <Empty.Title class="text-sm font-semibold">Could not load available connectors</Empty.Title>
            <Empty.Description>
              The list could not be fetched, so there may still be connectors to connect.
            </Empty.Description>
          </Empty.Header>
          <Empty.Content>
            <Button variant="outline" size="sm" onclick={onretry}>
              <RefreshCw data-icon="inline-start" />
              Retry
            </Button>
          </Empty.Content>
        </Empty.Root>
      {:else if available.length === 0}
        <Empty.Root class="border border-dashed border-line-2">
          <Empty.Header>
            <Empty.Media variant="icon" class="border border-line-2 bg-transparent text-muted-foreground"><Plug /></Empty.Media>
            <Empty.Title class="text-sm font-semibold">Nothing left to connect</Empty.Title>
            <Empty.Description>
              Every official connector is already installed.
            </Empty.Description>
          </Empty.Header>
        </Empty.Root>
      {:else}
        <ul class="flex flex-col gap-2">
          {#each available as c (c.name)}
            <li
              class="flex flex-wrap items-start gap-3 border border-line-2 bg-panel p-3 transition-colors hover:border-violet"
            >
              <div class="flex min-w-0 flex-1 flex-col gap-1">
                <div class="flex flex-wrap items-center gap-2">
                  <span class="font-semibold">{c.displayName || c.name}</span>
                  <span class="font-mono text-xs text-muted-foreground">v{c.version}</span>
                  {#each c.tags as tag (tag)}
                    <span class="tag text-fg-2">{tag}</span>
                  {/each}
                </div>
                <p class="prose-text text-xs text-muted-foreground">{c.summary}</p>
              </div>
              <Button size="sm" href={href(c.name)} onclick={() => (open = false)}>
                <Plug data-icon="inline-start" />
                Open
              </Button>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  </Dialog.Content>
</Dialog.Root>
