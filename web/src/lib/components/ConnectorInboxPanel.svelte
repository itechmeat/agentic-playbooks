<script lang="ts">
  import {
    UNTRUSTED_NOTICE,
    formatReceived,
    inboxPanelState,
    previewBody,
    type ConnectorInbox,
    type InboxEventRow,
  } from '../connectorinbox'
  import { Badge } from '$lib/components/ui/badge'
  import { Button } from '$lib/components/ui/button'
  import * as Table from '$lib/components/ui/table'
  import * as Alert from '$lib/components/ui/alert'
  import { Skeleton } from '$lib/components/ui/skeleton'
  import Copy from '@lucide/svelte/icons/copy'
  import TriangleAlert from '@lucide/svelte/icons/triangle-alert'
  import { untrack } from 'svelte'
  import { toast } from 'svelte-sonner'

  let {
    name,
    inbox = null,
    loaded = false,
    failed = false,
    events = [],
    eventsAccount = '',
    expandedSeqs = [],
    onExpand = undefined,
  }: {
    name: string
    inbox?: ConnectorInbox | null
    loaded?: boolean
    failed?: boolean
    events?: InboxEventRow[]
    eventsAccount?: string
    // Seqs whose body is revealed. A prop so a server-render test can force
    // one open; the panel manages it from there.
    expandedSeqs?: number[]
    onExpand?: (account: string) => void
  } = $props()

  // Named `panelState`, not `state`: a local binding named `state` shadows
  // the `$state` rune's disambiguation and svelte-check misreads `$state(...)`
  // below as a store auto-subscription instead of the rune.
  const panelState = $derived(inboxPanelState(loaded, failed, inbox))

  // Bodies are revealed one event at a time, never a whole account at once
  // (spec 2026-08-16-webhook-ingest-design). Showing an account's events puts
  // their metadata on the page; reading what a stranger actually wrote is a
  // second, deliberate click per event.
  //
  // Read through `untrack` so this is an initial value and not a reactive
  // read: `expandedSeqs` is a prop only a server-render test sets, and the
  // panel owns the state from here.
  let expanded = $state<number[]>(untrack(() => [...expandedSeqs]))

  // Seqs are per account and every account starts at 1, so an expand under
  // one account must never carry over to another: expanding event #2 of
  // account A and then showing account B would otherwise reveal B's event #2
  // with no click at all, which is exactly the control this state implements.
  // Keyed on the account rather than reset on every render, so the prop above
  // still decides the first paint.
  let shownAccount = untrack(() => eventsAccount)
  $effect(() => {
    if (eventsAccount !== shownAccount) {
      shownAccount = eventsAccount
      expanded = []
    }
  })

  const isOpen = (seq: number) => expanded.includes(seq)
  const toggle = (seq: number) => {
    expanded = isOpen(seq) ? expanded.filter((s) => s !== seq) : [...expanded, seq]
  }

  // The URL is short and safe to put on the clipboard; a failure is reported
  // rather than swallowed, since the operator is about to paste it somewhere.
  async function copy(url: string) {
    try {
      await navigator.clipboard.writeText(url)
      toast.success('Callback URL copied')
    } catch (e) {
      toast.error('Could not copy the callback URL', { description: String(e) })
    }
  }

  // Column captions of the tables in this view (terminal look).
  const TH = 'h-8 text-[11px] font-normal tracking-[0.08em] text-muted-foreground uppercase'
</script>

{#if panelState !== 'hidden'}
  <section class="box flex flex-col gap-3 px-4 pt-5 pb-4">
    <div class="flex flex-col gap-1">
      <h2 class="box-cap">Inbox</h2>
      <p class="prose-text text-xs text-muted-foreground">
        Events delivered to this machine for {name}. Read only: the dashboard never acknowledges
        anything, so a playbook's cursor is untouched by looking here.
      </p>
    </div>
    <div>
      {#if panelState === 'loading'}
        <Skeleton class="h-16 w-full" />
      {:else if panelState === 'error'}
        <p class="prose-text text-xs text-muted-foreground">
          The inbox could not be read, so this connector's pending depth is unknown here.
        </p>
      {:else if panelState === 'empty'}
        <p class="prose-text text-xs text-muted-foreground">
          Nothing has been delivered yet. An account inbox appears here after its first accepted
          delivery.
        </p>
      {:else if inbox}
        <div class="flex flex-col gap-3">
          {#if !inbox.publicBaseUrlSet}
            <p class="prose-text text-xs text-muted-foreground">
              Set ingest.public_base_url in the global config to see the exact callback URL to
              register with the provider.
            </p>
          {/if}
          <Table.Root>
            <Table.Header>
              <Table.Row>
                <Table.Head class={TH}>Account</Table.Head>
                <Table.Head class={TH}>Pending</Table.Head>
                <Table.Head class={TH}>Dropped</Table.Head>
                <Table.Head class={TH}>Stored</Table.Head>
                <Table.Head class={TH}>Last received</Table.Head>
                <Table.Head class={TH}>Callback URL</Table.Head>
                <Table.Head class={TH}>Events</Table.Head>
              </Table.Row>
            </Table.Header>
            <Table.Body>
              {#each inbox.accounts as a (a.account)}
                <Table.Row>
                  <Table.Cell class="font-mono text-xs">{a.account}</Table.Cell>
                  <Table.Cell>
                    {#if a.pending > 0}
                      <Badge variant="outline">{a.pending}</Badge>
                    {:else}
                      {a.pending}
                    {/if}
                  </Table.Cell>
                  <Table.Cell>
                    {#if a.dropped > 0}
                      <Badge variant="destructive">{a.dropped}</Badge>
                    {:else}
                      {a.dropped}
                    {/if}
                  </Table.Cell>
                  <Table.Cell>{a.total}</Table.Cell>
                  <Table.Cell class="font-mono text-xs">{formatReceived(a.lastReceivedAt)}</Table.Cell>
                  <Table.Cell class="whitespace-normal">
                    {#if a.callbackUrl}
                      <div class="flex items-center gap-2">
                        <code class="text-xs">{a.callbackUrl}</code>
                        <Button
                          size="sm"
                          variant="outline"
                          class="max-sm:px-2"
                          onclick={() => copy(a.callbackUrl ?? '')}
                        >
                          <Copy data-icon="inline-start" />
                          <span class="max-sm:sr-only">Copy</span>
                        </Button>
                      </div>
                    {:else}
                      <span class="text-muted-foreground">public_base_url is not set</span>
                    {/if}
                  </Table.Cell>
                  <Table.Cell>
                    <Button size="sm" variant="outline" onclick={() => onExpand?.(a.account)}>
                      Show
                    </Button>
                  </Table.Cell>
                </Table.Row>
              {/each}
            </Table.Body>
          </Table.Root>

          {#if eventsAccount}
            <div class="flex flex-col gap-2">
              <Alert.Root class="border-amber/50 text-amber">
                <TriangleAlert />
                <Alert.Title>Untrusted content</Alert.Title>
                <Alert.Description>{UNTRUSTED_NOTICE}</Alert.Description>
              </Alert.Root>
              {#if events.length === 0}
                <p class="prose-text text-xs text-muted-foreground">
                  No unread events for {eventsAccount}. Anything a playbook has already acknowledged
                  is not listed here.
                </p>
              {:else}
                <ul class="flex flex-col gap-1">
                  {#each events as e (e.seq)}
                    <li class="flex flex-col gap-0.5 border-t border-line pt-1">
                      <div class="flex items-center gap-2">
                        <span class="font-mono text-xs text-muted-foreground">
                          #{e.seq} {formatReceived(e.receivedAt)}
                        </span>
                        <Button size="sm" variant="ghost" onclick={() => toggle(e.seq)}>
                          {isOpen(e.seq) ? 'Hide body' : 'Show body'}
                        </Button>
                      </div>
                      {#if isOpen(e.seq)}
                        <!-- Marked on the body itself, not only once above the
                             list: the notice at the top scrolls away by event
                             #18, and an operator must not be able to mistake
                             what a stranger wrote for something apb produced. -->
                        <div class="stranger-text flex flex-col gap-1 border p-2">
                          <span
                            class="flex items-center gap-1 text-xs font-medium tracking-wide text-amber uppercase"
                          >
                            <TriangleAlert class="size-3" />
                            Untrusted content, sender authored
                          </span>
                          <!-- Interpolated as text, never as markup: the body
                               is written by whoever sent the message. -->
                          <code class="text-xs break-all">{previewBody(e.body, 2000)}</code>
                        </div>
                      {/if}
                    </li>
                  {/each}
                </ul>
              {/if}
            </div>
          {/if}
        </div>
      {/if}
    </div>
  </section>
{/if}

<style>
  /* Text a stranger wrote: an amber tint unlike apb's own output. */
  .stranger-text {
    border-color: color-mix(in oklab, var(--c-amber) 50%, transparent);
    background: color-mix(in oklab, var(--c-amber) 6%, var(--c-bg));
  }
</style>
