<script lang="ts">
  import {
    ApiError,
    approveConnector,
    fetchConnector,
    fetchConnectorStats,
    installConnector,
    runConnectorHealthcheck,
    uninstallConnector,
    type HealthcheckResult,
  } from '../lib/api'
  import {
    connectorActionMessage,
    needsForce,
    DISCONNECT_KEEPS_CONFIG,
  } from '../lib/connectorinstall'
  import { accountReady, trustBadge, type ConnectorAccount, type ConnectorDetail } from '../lib/connectors'
  import { errorRate, formatDurationMs, outcomeSummary, type ConnectorStats } from '../lib/connectorstats'
  import { renderMarkdown } from '../lib/markdown'
  import { subscribeChanges } from '../lib/ws'
  import Topbar from '$lib/components/Topbar.svelte'
  import ConnectorPlaygroundPanel from '$lib/components/ConnectorPlaygroundPanel.svelte'
  import { Button } from '$lib/components/ui/button'
  import { Badge } from '$lib/components/ui/badge'
  import * as Card from '$lib/components/ui/card'
  import * as Table from '$lib/components/ui/table'
  import * as Empty from '$lib/components/ui/empty'
  import { Skeleton } from '$lib/components/ui/skeleton'
  import { Spinner } from '$lib/components/ui/spinner'
  import { toast } from 'svelte-sonner'
  import * as Alert from '$lib/components/ui/alert'
  import ShieldCheck from '@lucide/svelte/icons/shield-check'
  import Plug from '@lucide/svelte/icons/plug'
  import Unplug from '@lucide/svelte/icons/unplug'
  import Replace from '@lucide/svelte/icons/replace'
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
  // Set when the detail endpoint answered 404: the name is neither installed
  // nor offered by the server, so Connect would be guaranteed to fail. Tracked
  // apart from a request that never answered, which must not be read as "this
  // connector does not exist".
  let unknownName = $state(false)
  let busy = $state<'connect' | 'disconnect' | null>(null)
  // A 409 needs_force means a different version is on disk. The replace action
  // is only ever offered as an explicit second click, never retried silently.
  let forceOffered = $state(false)

  // Monotonic token: an in-flight fetch that resolves after a newer load
  // started (fast navigation between connectors) must not overwrite the
  // current view.
  let loadToken = 0

  // The detail endpoint answers for a connector that is merely not connected
  // too (its manifest is embedded in the server binary), so there is no
  // fallback dance here: a 404 means the name is genuinely unknown, and
  // anything else is a real failure worth a toast.
  async function load(token: number) {
    try {
      const d = await fetchConnector(name, workspace)
      if (token !== loadToken) return
      detail = d
      unknownName = false
    } catch (e) {
      if (token !== loadToken) return
      if (e instanceof ApiError && e.status === 404) {
        detail = null
        unknownName = true
      } else {
        toast.error('Failed to load connector', { description: String(e) })
      }
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
    unknownName = false
    forceOffered = false
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

  // `force` replaces a different installed version and is only ever passed
  // from the explicit "Replace installed version" action.
  async function connect(force = false) {
    if (busy !== null) return
    busy = 'connect'
    try {
      const result = await installConnector(name, force)
      forceOffered = false
      if (result.noOp) {
        toast.success(`${name} is already connected at v${result.version}`)
      } else {
        toast.success(`Connected ${name} v${result.version}`)
      }
      if (result.trustWarning) {
        toast.warning('Connected, but trust was not recorded', {
          description: result.trustWarning,
        })
      }
      await refresh()
    } catch (e) {
      const code = e instanceof ApiError ? e.code : undefined
      const detailText = e instanceof ApiError ? e.detail : undefined
      forceOffered = needsForce(code)
      toast.error('Connect failed', {
        description: connectorActionMessage(code, 'connect', detailText ?? String(e)),
      })
    } finally {
      busy = null
    }
  }

  async function disconnect() {
    if (busy !== null) return
    busy = 'disconnect'
    try {
      const result = await uninstallConnector(name)
      toast.success(result.noOp ? `${name} was not connected` : `Disconnected ${name}`, {
        description: DISCONNECT_KEEPS_CONFIG,
      })
      await refresh()
    } catch (e) {
      const code = e instanceof ApiError ? e.code : undefined
      const detailText = e instanceof ApiError ? e.detail : undefined
      toast.error('Disconnect failed', {
        description: connectorActionMessage(code, 'disconnect', detailText ?? String(e)),
      })
    } finally {
      busy = null
    }
  }

  // The server does not necessarily broadcast a change event for install and
  // uninstall, so the page refetches explicitly rather than waiting on the
  // WebSocket subscription.
  // Reuses the current token (the same way `approve` does) so the change
  // subscription set up by the effect keeps matching.
  async function refresh() {
    await load(loadToken)
    void loadStats(loadToken)
  }

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

  // The breadcrumb shows the same label as the card below it, falling back to
  // the slug while the detail is still loading or the name is unknown.
  const heading = $derived(detail?.meta.display_name || name)
  // Everything the manifest describes is public and is shown in both states;
  // only what actually needs bytes on disk is gated on this.
  const installed = $derived(detail?.installed === true)
</script>

<Topbar active="connectors">
  {#snippet title()}
    <span class="truncate text-sm font-medium">{heading}</span>
  {/snippet}
  {#snippet actions()}
    {#if detail && !installed}
      <Button size="sm" class="max-sm:px-2" onclick={() => connect()} disabled={busy !== null}>
        {#if busy === 'connect'}<Spinner data-icon="inline-start" />{:else}<Plug data-icon="inline-start" />{/if}
        <span class="max-sm:sr-only">Connect</span>
      </Button>
    {:else if installed}
      <Button
        size="sm"
        variant="outline"
        class="max-sm:px-2"
        onclick={disconnect}
        disabled={busy !== null}
      >
        {#if busy === 'disconnect'}<Spinner data-icon="inline-start" />{:else}<Unplug data-icon="inline-start" />{/if}
        <span class="max-sm:sr-only">Disconnect</span>
      </Button>
    {/if}
    {#if installed && detail && detail.trust !== 'approved'}
      <Button size="sm" class="max-sm:px-2" onclick={() => approve(null)} disabled={approving !== null}>
        {#if approving === ''}<Spinner data-icon="inline-start" />{:else}<ShieldCheck data-icon="inline-start" />{/if}
        <span class="max-sm:sr-only">Approve connector</span>
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
      <Empty.Root>
        <Empty.Header>
          <Empty.Media variant="icon">
            <Unplug />
          </Empty.Media>
          <Empty.Title>{name}</Empty.Title>
          <Empty.Description>
            {#if unknownName}
              No connector by this name is installed or offered by the server. The name in the
              address may be wrong.
            {:else}
              This connector could not be loaded. Check that the server is reachable and try
              again.
            {/if}
          </Empty.Description>
        </Empty.Header>
      </Empty.Root>
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
        <Card.Footer class="flex flex-col items-start gap-3">
          {#if installed}
            <p class="text-sm text-muted-foreground">{DISCONNECT_KEEPS_CONFIG}</p>
          {:else}
            <p class="text-sm text-muted-foreground">
              Connecting installs the connector files locally. Any account configuration you had
              before is kept in a separate store and is picked up again automatically.
            </p>
          {/if}
          {#if forceOffered}
            <Alert.Root variant="destructive">
              <Replace />
              <Alert.Title>A different version is already installed</Alert.Title>
              <Alert.Description>
                <p>
                  The server refused to connect because another version of this connector is
                  on disk. Replacing it overwrites the installed files and drops their recorded
                  trust. Account configuration is not affected.
                </p>
                <Button
                  size="sm"
                  variant="outline"
                  onclick={() => connect(true)}
                  disabled={busy !== null}
                >
                  {#if busy === 'connect'}<Spinner data-icon="inline-start" />{:else}<Replace data-icon="inline-start" />{/if}
                  Replace installed version
                </Button>
              </Alert.Description>
            </Alert.Root>
          {/if}
        </Card.Footer>
      </Card.Root>

      {#if detail.bodyMd}
        <Card.Root>
          <Card.Header>
            <Card.Title class="text-sm">About</Card.Title>
          </Card.Header>
          <Card.Content>
            <!-- `renderMarkdown` escapes every character coming from the
                 connector folder, so nothing in PUBLIC.md can turn into a live
                 tag or attribute here. See lib/markdown.ts. -->
            <div class="text-foreground">{@html renderMarkdown(detail.bodyMd)}</div>
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
                      {#if !installed}
                        <!-- Approving and probing both act on an installed
                             connector: the probe invokes it, and approving a
                             connector that is not on disk yet decides nothing
                             the user can act on. The account rows themselves
                             stay visible so the required fields can be
                             reviewed first. -->
                        <span class="whitespace-normal text-xs text-muted-foreground">
                          connect to approve or probe
                        </span>
                      {:else}
                        <div class="flex flex-col items-start gap-2">
                          <div class="flex flex-wrap gap-2">
                            {#if a.trust !== 'approved'}
                              <Button
                                size="sm"
                                variant="outline"
                                class="max-sm:px-2"
                                onclick={() => approve(a.name)}
                                disabled={approving !== null}
                              >
                                {#if approving === a.name}<Spinner data-icon="inline-start" />{:else}<ShieldCheck data-icon="inline-start" />{/if}
                                <span class="max-sm:sr-only">Approve</span>
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
                                class="max-sm:px-2"
                                onclick={() => probe(a)}
                                disabled={a.trust !== 'approved' || probing !== null}
                              >
                                {#if probing === a.name}<Spinner data-icon="inline-start" />{:else}<Stethoscope data-icon="inline-start" />{/if}
                                <span class="max-sm:sr-only">Probe</span>
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
                      {/if}
                    </Table.Cell>
                  </Table.Row>
                {/each}
              </Table.Body>
            </Table.Root>
          {/if}
        </Card.Content>
      </Card.Root>

      <!-- The playground actually invokes the connector, so it is the one
           panel that genuinely needs the installed files and stays gated. -->
      {#if installed}
        <ConnectorPlaygroundPanel
          {name}
          {workspace}
          functions={detail.functions}
          accounts={detail.accounts}
        />
      {/if}

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
