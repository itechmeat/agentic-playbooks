<script lang="ts">
  import { fetchTrust, revokeTrust } from '../lib/api'
  import { subscribeChanges } from '../lib/ws'
  import type { TrustEntry, TrustKind } from '../lib/types'
  import Topbar from '$lib/components/Topbar.svelte'
  import PageScroll from '$lib/components/PageScroll.svelte'
  import PageHead from '$lib/components/PageHead.svelte'
  import StatusBar, { type StatusItem } from '$lib/components/StatusBar.svelte'
  import { profileColor } from '../lib/profilecolor'
  import { Button } from '$lib/components/ui/button'
  import * as Empty from '$lib/components/ui/empty'
  import * as AlertDialog from '$lib/components/ui/alert-dialog'
  import { Skeleton } from '$lib/components/ui/skeleton'
  import { toast } from 'svelte-sonner'
  import ShieldCheck from '@lucide/svelte/icons/shield-check'
  import ShieldOff from '@lucide/svelte/icons/shield-off'

  let items = $state<TrustEntry[]>([])
  let loaded = $state(false)
  let revoking = $state<string | null>(null)
  let target = $state<TrustEntry | null>(null)
  let confirmOpen = $state(false)

  const SECTIONS: { kind: TrustKind; title: string; hint: string }[] = [
    {
      kind: 'playbook',
      title: 'Playbooks',
      hint: 'An approved digest runs through the MCP gate without asking again.',
    },
    {
      kind: 'profile_bundle',
      title: 'Profiles',
      hint: 'A profile together with the exact content of its skills.',
    },
    {
      kind: 'connector',
      title: 'Connectors',
      hint: 'The connector files a run may use.',
    },
    {
      kind: 'connector_account',
      title: 'Connector accounts',
      hint: 'Where an account sends its secret, and any command it reads it from.',
    },
  ]

  const sections = $derived(
    SECTIONS.map((s) => ({ ...s, items: items.filter((e) => e.kind === s.kind) })).filter(
      (s) => s.items.length > 0,
    ),
  )

  const ORIGIN_LABEL: Record<TrustEntry['origin_kind'], string> = {
    bundled: 'built in',
    agent_generated: 'by an agent',
    locally_approved: 'by you',
    repository_provided: 'from the repository',
  }

  // Outlined tag colors per origin: shipped and own approvals read as
  // settled, an agent's or a repository's as worth a second look.
  const ORIGIN_TONE: Record<TrustEntry['origin_kind'], string> = {
    bundled: 'text-muted-foreground',
    agent_generated: 'text-amber',
    locally_approved: 'text-green',
    repository_provided: 'text-blue',
  }

  const statusItems = $derived.by((): StatusItem[] => [
    { label: 'approved', value: items.length, tone: 'success' },
    ...SECTIONS.map((s) => ({
      label: s.title.toLowerCase(),
      value: items.filter((e) => e.kind === s.kind).length,
      tone: 'muted' as const,
    })),
  ])

  async function load() {
    try {
      items = await fetchTrust()
    } catch (e) {
      toast.error('Failed to load approvals', { description: String(e) })
    } finally {
      loaded = true
    }
  }

  $effect(() => {
    load()
    return subscribeChanges(load)
  })

  function ask(e: TrustEntry) {
    target = e
    confirmOpen = true
  }

  async function confirmRevoke() {
    const e = target
    if (!e) return
    confirmOpen = false
    revoking = e.digest
    try {
      await revokeTrust(e.digest)
      toast.success(`Revoked the approval of "${e.id}"`)
      await load()
    } catch (err) {
      toast.error('Revoke failed', { description: String(err) })
    } finally {
      revoking = null
    }
  }

  const fmtTime = (ms: number) => new Date(ms).toLocaleString()
  const shortDigest = (d: string) => d.replace(/^sha256:/, '').slice(0, 12)
</script>

<Topbar active="trust" />

<PageScroll>
  <div class="mx-auto w-full max-w-5xl px-4 py-5 sm:px-5">
    <PageHead title="Approvals" class="mb-2">
      {#snippet meta()}
        {#if loaded}
          <span class="text-muted-foreground">{items.length} approved</span>
        {/if}
      {/snippet}
    </PageHead>
    <p class="prose-text mb-7 text-xs text-muted-foreground">
      Everything approved in this machine's trust store. Revoking an approval means that content
      needs a new approval, or a confirmation, before an agent can run it.
    </p>
    {#if !loaded}
      <div class="flex flex-col gap-3">
        {#each Array(3) as _, i (i)}<Skeleton class="h-16 w-full" />{/each}
      </div>
    {:else if items.length === 0}
      <Empty.Root class="border border-dashed border-line-2 bg-panel">
        <Empty.Header>
          <Empty.Media variant="icon" class="border border-line-2 bg-transparent text-muted-foreground">
            <ShieldCheck />
          </Empty.Media>
          <Empty.Title class="text-sm font-semibold">Nothing is approved</Empty.Title>
          <Empty.Description>
            Approvals appear here as you save playbooks and profiles, or approve connectors.
          </Empty.Description>
        </Empty.Header>
      </Empty.Root>
    {:else}
      <div class="flex flex-col gap-7">
        {#each sections as s (s.kind)}
          <section class="box px-4 pt-4 pb-2">
            <h2 class="box-cap">{s.title} · {s.items.length}</h2>
            <p class="prose-text mb-2 text-xs text-muted-foreground">{s.hint}</p>
            <ul class="divide-y divide-line">
              {#each s.items as e (e.digest)}
                <li class="flex flex-wrap items-center gap-x-4 gap-y-1 py-2">
                  <span class="min-w-0 flex-1 truncate font-medium" title={e.id}>
                    {#if s.kind === 'profile_bundle'}
                      <span class="legend-sq" style:--k={profileColor(e.id)}>{e.id}</span>
                    {:else}
                      {e.id}
                    {/if}
                  </span>
                  <span class="sm:w-40 sm:text-right">
                    <span class="tag {ORIGIN_TONE[e.origin_kind]}">{ORIGIN_LABEL[e.origin_kind]}</span>
                  </span>
                  <span class="text-xs text-muted-foreground tabular-nums sm:w-44" title="Approved">{fmtTime(e.approved_at_ms)}</span>
                  <span class="w-28 text-xs text-muted-foreground" title={e.digest}>{shortDigest(e.digest)}</span>
                  <Button
                    variant="outline"
                    size="xs"
                    class="max-sm:px-2"
                    onclick={() => ask(e)}
                    disabled={revoking === e.digest}
                  >
                    <ShieldOff data-icon="inline-start" />
                    <span class="max-sm:sr-only">Revoke</span>
                  </Button>
                </li>
              {/each}
            </ul>
          </section>
        {/each}
      </div>
    {/if}
  </div>
</PageScroll>

<StatusBar items={statusItems} />

<AlertDialog.Root bind:open={confirmOpen}>
  <AlertDialog.Content>
    <AlertDialog.Header>
      <AlertDialog.Title>Revoke this approval?</AlertDialog.Title>
      <AlertDialog.Description>
        "{target?.id}" ({target ? shortDigest(target.digest) : ''}) will no longer be approved. An
        agent will need your confirmation before running it again.
      </AlertDialog.Description>
    </AlertDialog.Header>
    <AlertDialog.Footer>
      <AlertDialog.Cancel>Cancel</AlertDialog.Cancel>
      <AlertDialog.Action onclick={confirmRevoke}>Revoke</AlertDialog.Action>
    </AlertDialog.Footer>
  </AlertDialog.Content>
</AlertDialog.Root>
