<script lang="ts">
  import {
    approveConnector,
    fetchConnector,
    fetchConnectorStats,
    runConnectorHealthcheck,
    type HealthcheckResult,
  } from '../lib/api'
  import { accountReady, trustBadge, type ConnectorAccount, type ConnectorDetail } from '../lib/connectors'
  import { errorRate, formatDurationMs, outcomeSummary, type ConnectorStats } from '../lib/connectorstats'
  import { subscribeChanges } from '../lib/ws'
  import Topbar from '$lib/components/Topbar.svelte'
  import { Button } from '$lib/components/ui/button'
  import { Badge } from '$lib/components/ui/badge'
  import * as Card from '$lib/components/ui/card'
  import * as Table from '$lib/components/ui/table'
  import { Skeleton } from '$lib/components/ui/skeleton'
  import { Spinner } from '$lib/components/ui/spinner'
  import { toast } from 'svelte-sonner'
  import ShieldCheck from '@lucide/svelte/icons/shield-check'
  import Stethoscope from '@lucide/svelte/icons/stethoscope'
  import ExternalLink from '@lucide/svelte/icons/external-link'
  import ChartNoAxesColumn from '@lucide/svelte/icons/chart-no-axes-column'

  let { name, workspace = '' }: { name: string; workspace?: string } = $props()

  let detail = $state<ConnectorDetail | null>(null)
  let loaded = $state(false)
  let stats = $state<ConnectorStats | null>(null)
  let statsLoaded = $state(false)
  let approving = $state<string | null>(null) // account name, or '' for the connector itself
  let probing = $state<string | null>(null)
  let probeResults = $state<Record<string, HealthcheckResult>>({})

  // Monotonic token: an in-flight fetch that resolves after a newer load
  // started (fast navigation between connectors) must not overwrite the
  // current view.
  let loadToken = 0

  async function load(token: number) {
    try {
      const d = await fetchConnector(name, workspace)
      if (token !== loadToken) return
      detail = d
    } catch (e) {
      if (token === loadToken) toast.error('Failed to load connector', { description: String(e) })
    } finally {
      if (token === loadToken) loaded = true
    }
  }

  // Usage stats are read-only and best-effort: a failure to load them must
  // not block or blank out the rest of the connector detail page, so this is
  // a separate try/catch/loading flag from `load`.
  async function loadStats(token: number) {
    try {
      const s = await fetchConnectorStats(name, workspace)
      if (token !== loadToken) return
      stats = s
    } catch {
      if (token === loadToken) stats = null
    } finally {
      if (token === loadToken) statsLoaded = true
    }
  }

  $effect(() => {
    void name
    void workspace
    loaded = false
    detail = null
    statsLoaded = false
    stats = null
    probeResults = {}
    const token = ++loadToken
    load(token)
    loadStats(token)
    return subscribeChanges(() => {
      load(token)
      loadStats(token)
    })
  })

  const badgeClass = {
    ok: 'border-success/30 bg-success/15 text-success',
    warn: 'border-warning/30 bg-warning/15 text-warning',
    danger: 'border-destructive/30 bg-destructive/15 text-destructive',
    muted: '',
  } as const

  async function approve(account: string | null) {
    approving = account ?? ''
    try {
      await approveConnector(name, account, workspace)
      toast.success(account ? `Approved account "${account}"` : 'Approved connector')
      await load(loadToken)
    } catch (e) {
      toast.error('Approve failed', { description: String(e) })
    } finally {
      approving = null
    }
  }

  async function probe(account: ConnectorAccount) {
    probing = account.name
    try {
      const result = await runConnectorHealthcheck(name, account.name, workspace)
      probeResults = { ...probeResults, [account.name]: result }
      if (!result.ok) {
        toast.error('Healthcheck failed', { description: result.error?.message ?? 'unknown error' })
      }
    } catch (e) {
      toast.error('Healthcheck request failed', { description: String(e) })
    } finally {
      probing = null
    }
  }

  const outcomeCode = (r: HealthcheckResult) => (r.ok ? 'ok' : (r.error?.code ?? 'error'))
  const outcomeTone = (r: HealthcheckResult) => (r.ok ? 'ok' : 'danger') as keyof typeof badgeClass

  const fieldEntries = (a: ConnectorAccount) => Object.entries(a.fields)
</script>

<Topbar active="connectors">
  {#snippet title()}
    <span class="truncate text-sm font-medium">{name}</span>
  {/snippet}
  {#snippet actions()}
    {#if detail && detail.trust !== 'approved'}
      <Button size="sm" onclick={() => approve(null)} disabled={approving !== null}>
        {#if approving === ''}<Spinner data-icon="inline-start" />{:else}<ShieldCheck data-icon="inline-start" />{/if}
        Approve connector
      </Button>
    {/if}
  {/snippet}
</Topbar>

<div class="min-h-0 flex-1 overflow-auto">
  <div class="mx-auto flex w-full max-w-4xl flex-col gap-6 px-4 py-6">
    {#if !loaded}
      <div class="flex flex-col gap-3">
        {#each Array(3) as _, i (i)}<Skeleton class="h-24 w-full" />{/each}
      </div>
    {:else if !detail}
      <p class="text-sm text-muted-foreground">Connector not found.</p>
    {:else}
      {@const badge = trustBadge(detail.trust)}
      <Card.Root>
        <Card.Header>
          <div class="flex flex-wrap items-center gap-2">
            <Card.Title class="text-base">{detail.meta.display_name ?? detail.name}</Card.Title>
            <span class="font-mono text-xs text-muted-foreground">v{detail.version}</span>
            <Badge variant="outline" class={badgeClass[badge.tone]}>{badge.label}</Badge>
          </div>
          {#if detail.meta.summary}
            <Card.Description>{detail.meta.summary}</Card.Description>
          {/if}
        </Card.Header>
        <Card.Content class="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
          {#each detail.meta.tags ?? [] as tag (tag)}
            <Badge variant="secondary">{tag}</Badge>
          {/each}
          {#if detail.meta.publisher}
            <span>by {detail.meta.publisher}</span>
          {/if}
          {#if detail.meta.homepage}
            <a
              href={detail.meta.homepage}
              target="_blank"
              rel="noreferrer"
              class="inline-flex items-center gap-1 text-primary hover:underline"
            >
              <ExternalLink class="size-3.5" />
              homepage
            </a>
          {/if}
        </Card.Content>
      </Card.Root>

      {#if detail.bodyMd}
        <Card.Root>
          <Card.Header>
            <Card.Title class="text-sm">About</Card.Title>
          </Card.Header>
          <Card.Content>
            <pre class="whitespace-pre-wrap font-sans text-sm text-foreground">{detail.bodyMd}</pre>
          </Card.Content>
        </Card.Root>
      {/if}

      <Card.Root>
        <Card.Header>
          <Card.Title class="text-sm">Functions</Card.Title>
        </Card.Header>
        <Card.Content>
          {#if detail.functions.length === 0}
            <p class="text-sm text-muted-foreground">This connector declares no functions.</p>
          {:else}
            <Table.Root>
              <Table.Header>
                <Table.Row>
                  <Table.Head>Name</Table.Head>
                  <Table.Head>Description</Table.Head>
                  <Table.Head>Read-only</Table.Head>
                  <Table.Head>Deprecated</Table.Head>
                </Table.Row>
              </Table.Header>
              <Table.Body>
                {#each detail.functions as f (f.name)}
                  <Table.Row>
                    <Table.Cell class="font-mono text-xs">{f.name}</Table.Cell>
                    <Table.Cell class="whitespace-normal text-muted-foreground">
                      {f.description}
                    </Table.Cell>
                    <Table.Cell>{f.readOnly ? 'yes' : 'no'}</Table.Cell>
                    <Table.Cell>{f.deprecated ? 'yes' : 'no'}</Table.Cell>
                  </Table.Row>
                {/each}
              </Table.Body>
            </Table.Root>
          {/if}
        </Card.Content>
      </Card.Root>

      <Card.Root>
        <Card.Header>
          <Card.Title class="text-sm">Accounts</Card.Title>
        </Card.Header>
        <Card.Content>
          {#if detail.accounts.length === 0}
            <p class="text-sm text-muted-foreground">No accounts configured for this connector.</p>
          {:else}
            <Table.Root>
              <Table.Header>
                <Table.Row>
                  <Table.Head>Name</Table.Head>
                  <Table.Head>Fields</Table.Head>
                  <Table.Head>Missing env</Table.Head>
                  <Table.Head>Trust</Table.Head>
                  <Table.Head>Actions</Table.Head>
                </Table.Row>
              </Table.Header>
              <Table.Body>
                {#each detail.accounts as a (a.name)}
                  {@const acctBadge = trustBadge(a.trust)}
                  {@const ready = accountReady(a)}
                  {@const result = probeResults[a.name]}
                  <Table.Row>
                    <Table.Cell class="align-top font-mono text-xs">
                      {a.name}{#if a.default}<span class="ml-1 text-muted-foreground">(default)</span>{/if}
                    </Table.Cell>
                    <Table.Cell class="align-top whitespace-normal">
                      {#if fieldEntries(a).length === 0}
                        <span class="text-muted-foreground">-</span>
                      {:else}
                        <ul class="flex flex-col gap-0.5 font-mono text-xs text-muted-foreground">
                          {#each fieldEntries(a) as [k, v] (k)}
                            <li>{k} = {v}</li>
                          {/each}
                        </ul>
                      {/if}
                    </Table.Cell>
                    <Table.Cell class="align-top whitespace-normal">
                      {#if a.missingEnv.length === 0}
                        <span class="text-muted-foreground">-</span>
                      {:else}
                        <div class="flex flex-wrap gap-1">
                          {#each a.missingEnv as v (v)}
                            <Badge variant="outline" class={badgeClass.warn}>{v}</Badge>
                          {/each}
                        </div>
                      {/if}
                    </Table.Cell>
                    <Table.Cell class="align-top">
                      <Badge variant="outline" class={badgeClass[acctBadge.tone]}>
                        {acctBadge.label}
                      </Badge>
                    </Table.Cell>
                    <Table.Cell class="align-top">
                      <div class="flex flex-col items-start gap-2">
                        <div class="flex flex-wrap gap-2">
                          {#if a.trust !== 'approved'}
                            <Button
                              size="sm"
                              variant="outline"
                              onclick={() => approve(a.name)}
                              disabled={approving !== null}
                            >
                              {#if approving === a.name}<Spinner data-icon="inline-start" />{:else}<ShieldCheck data-icon="inline-start" />{/if}
                              Approve
                            </Button>
                          {/if}
                          <span
                            title={a.trust !== 'approved'
                              ? 'Approve this account before probing - the server refuses an unapproved probe with a permission error.'
                              : undefined}
                          >
                            <Button
                              size="sm"
                              variant="outline"
                              onclick={() => probe(a)}
                              disabled={a.trust !== 'approved' || probing !== null}
                            >
                              {#if probing === a.name}<Spinner data-icon="inline-start" />{:else}<Stethoscope data-icon="inline-start" />{/if}
                              Probe
                            </Button>
                          </span>
                        </div>
                        {#if result}
                          <Badge variant="outline" class={badgeClass[outcomeTone(result)]}>
                            {outcomeCode(result)}
                          </Badge>
                        {/if}
                        {#if !ready}
                          <span class="text-xs text-muted-foreground">missing env vars</span>
                        {/if}
                      </div>
                    </Table.Cell>
                  </Table.Row>
                {/each}
              </Table.Body>
            </Table.Root>
          {/if}
        </Card.Content>
      </Card.Root>

      <Card.Root>
        <Card.Header>
          <div class="flex items-center gap-2">
            <ChartNoAxesColumn class="size-4 text-muted-foreground" />
            <Card.Title class="text-sm">Usage</Card.Title>
          </div>
          <Card.Description>
            Calls, error rate, and duration per function and account, read from recent run
            event logs. Read only - no new state is recorded here.
          </Card.Description>
        </Card.Header>
        <Card.Content>
          {#if !statsLoaded}
            <Skeleton class="h-16 w-full" />
          {:else if !stats || stats.calls === 0}
            <p class="text-sm text-muted-foreground">
              No recorded calls{#if stats} in the last {stats.runsScanned} run{stats.runsScanned === 1
                ? ''
                : 's'} scanned{/if}.
            </p>
          {:else}
            <div class="flex flex-col gap-3">
              <p class="text-sm text-muted-foreground">
                {stats.calls} call{stats.calls === 1 ? '' : 's'} across {stats.runsScanned} run{stats.runsScanned ===
                1
                  ? ''
                  : 's'} scanned (most recent {stats.runsScanned}) - {outcomeSummary(stats.byOutcome)}.
              </p>
              <Table.Root>
                <Table.Header>
                  <Table.Row>
                    <Table.Head>Function</Table.Head>
                    <Table.Head>Account</Table.Head>
                    <Table.Head>Calls</Table.Head>
                    <Table.Head>Error rate</Table.Head>
                    <Table.Head>Avg duration</Table.Head>
                  </Table.Row>
                </Table.Header>
                <Table.Body>
                  {#each stats.byFunction as f (`${f.function}:${f.account}`)}
                    <Table.Row>
                      <Table.Cell class="font-mono text-xs">{f.function}</Table.Cell>
                      <Table.Cell class="font-mono text-xs">{f.account}</Table.Cell>
                      <Table.Cell>{f.calls}</Table.Cell>
                      <Table.Cell>
                        {#if f.errors > 0}
                          <Badge variant="outline" class={badgeClass.warn}>{errorRate(f)}</Badge>
                        {:else}
                          {errorRate(f)}
                        {/if}
                      </Table.Cell>
                      <Table.Cell>{formatDurationMs(f.avgDurationMs)}</Table.Cell>
                    </Table.Row>
                  {/each}
                </Table.Body>
              </Table.Root>
            </div>
          {/if}
        </Card.Content>
      </Card.Root>
    {/if}
  </div>
</div>
