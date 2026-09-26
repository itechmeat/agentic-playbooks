<script lang="ts">
  import { fetchTrust, revokeTrust } from '../lib/api'
  import { subscribeChanges } from '../lib/ws'
  import type { TrustEntry, TrustKind } from '../lib/types'
  import Topbar from '$lib/components/Topbar.svelte'
  import PageScroll from '$lib/components/PageScroll.svelte'
  import { Button } from '$lib/components/ui/button'
  import { Badge } from '$lib/components/ui/badge'
  import * as Card from '$lib/components/ui/card'
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

<Topbar active="trust">
  {#snippet title()}
    <span class="truncate text-sm font-semibold">Approvals</span>
  {/snippet}
</Topbar>

<PageScroll>
  <div class="mx-auto w-full max-w-4xl px-4 py-6">
    <p class="mb-6 text-sm text-muted-foreground">
      Everything approved in this machine's trust store. Revoking an approval means that content
      needs a new approval, or a confirmation, before an agent can run it.
    </p>
    {#if !loaded}
      <div class="flex flex-col gap-3">
        {#each Array(3) as _, i (i)}<Skeleton class="h-16 w-full" />{/each}
      </div>
    {:else if items.length === 0}
      <Empty.Root class="border border-dashed">
        <Empty.Header>
          <Empty.Media variant="icon"><ShieldCheck /></Empty.Media>
          <Empty.Title>Nothing is approved</Empty.Title>
          <Empty.Description>
            Approvals appear here as you save playbooks and profiles, or approve connectors.
          </Empty.Description>
        </Empty.Header>
      </Empty.Root>
    {:else}
      {#each sections as s (s.kind)}
        <section class="mb-8">
          <h2 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
            {s.title}
          </h2>
          <p class="mb-3 text-xs text-muted-foreground">{s.hint}</p>
          <div class="flex flex-col gap-2">
            {#each s.items as e (e.digest)}
              <Card.Root class="py-3">
                <Card.Header class="px-4">
                  <div class="flex min-w-0 flex-wrap items-center gap-2">
                    <Card.Title class="truncate font-mono text-sm">{e.id}</Card.Title>
                    <Badge variant="outline">{ORIGIN_LABEL[e.origin_kind]}</Badge>
                  </div>
                  <Card.Description class="text-xs">
                    Approved {fmtTime(e.approved_at_ms)} ·
                    <span class="font-mono" title={e.digest}>{shortDigest(e.digest)}</span>
                  </Card.Description>
                  <Card.Action>
                    <Button
                      variant="outline"
                      size="sm"
                      class="max-sm:px-2"
                      onclick={() => ask(e)}
                      disabled={revoking === e.digest}
                    >
                      <ShieldOff data-icon="inline-start" />
                      <span class="max-sm:sr-only">Revoke</span>
                    </Button>
                  </Card.Action>
                </Card.Header>
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
