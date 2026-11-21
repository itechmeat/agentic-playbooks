<script lang="ts">
  // The "MCP server" section of a connector page (design mcp-http-and-surfaces
  // D5 "Dashboard" item 5): how apb reaches the server, one table per account
  // row with the approval of each mapped tool, and a live listing a person
  // asks for per row, in the project that row names. Every string a server
  // chose is rendered as text (`{value}`), never as markup. No digest and no
  // approve command: approving tools is a person's act at a terminal.
  //
  // Only a signed-in session lists (the server answers 403 to a dashboard
  // without keys and to a bearer key): the button is not offered when the
  // page knows it is not signed in, and a 403 shows its own refused state
  // with fixed words. Each row's result is a live region, busy while it lists.
  import { listMcpTools, type McpConnectorView, type McpToolsResult } from '$lib/api/connectors'
  import { authState } from '$lib/auth.svelte'
  import { Button } from '$lib/components/ui/button'
  import * as Table from '$lib/components/ui/table'
  import { LISTING_REFUSED, listButtonLabel, listingFailure, needsApproval, statusLabel } from '$lib/mcpTools'

  let {
    connector,
    mcp,
    workspace = '',
  }: {
    connector: string
    mcp: McpConnectorView
    /** The page's project, for the page's own use only: a listing is sent
     * with the workspace of its row, never this one. */
    workspace?: string
  } = $props()

  type Live =
    | { loading: true }
    | { loading: false; result: McpToolsResult }
    | { loading: false; refused: true }
    | { loading: false; failure: string }

  let live = $state<Record<string, Live>>({})

  const authSnapshot = authState()
  // A dashboard without keys is never signed in; with keys the app shows
  // the login screen until it is, so a checked status that requires no key
  // is the one case the page knows the listing would be refused.
  const keyless = $derived(authSnapshot.checked && !authSnapshot.required)

  const rowKey = (ws: string, account: string) => `${ws}\u0000${account}`

  async function list(ws: string, account: string) {
    const key = rowKey(ws, account)
    live[key] = { loading: true }
    try {
      const result = await listMcpTools(connector, account, ws)
      live[key] = { loading: false, result }
    } catch (e) {
      const failed = listingFailure(e)
      live[key] = failed.refused ? { loading: false, refused: true } : { loading: false, failure: failed.message }
    }
  }

  const shown = (v: unknown) => JSON.stringify(v, null, 2)
</script>

<section class="box mcp">
  <h2 class="box-cap">MCP server</h2>
  <dl class="facts">
    <dt>transport</dt>
    <dd>{mcp.transport} ({mcp.protocol})</dd>
    {#if mcp.command}
      <dt>command</dt>
      <dd class="mono">{mcp.command}</dd>
    {/if}
    {#if mcp.url}
      <dt>url</dt>
      <dd class="mono">{mcp.url}</dd>
    {/if}
    {#if mcp.auth}
      <dt>auth</dt>
      <dd>{mcp.auth}</dd>
    {/if}
    {#if mcp.env.length > 0}
      <dt>env</dt>
      <dd class="mono">{mcp.env.join(', ')}</dd>
    {/if}
  </dl>
  {#if mcp.pinsError}
    <p class="warn">The tool approvals could not be read: {mcp.pinsError}</p>
  {/if}
  {#if mcp.accounts.length === 0}
    <p class="muted">No accounts configured for this connector yet.</p>
  {/if}
  {#each mcp.accounts as a (rowKey(a.workspace, a.account))}
    {@const lv = live[rowKey(a.workspace, a.account)]}
    <div class="account">
      <div class="account-head">
        <h3>
          <span class="mono">{a.account}</span>
          {#if a.rootLabel}<span class="muted">in {a.rootLabel}</span>{/if}
        </h3>
        {#if !keyless}
          <Button
            size="xs"
            variant="outline"
            aria-label={listButtonLabel(a.account, a.rootLabel)}
            disabled={lv?.loading}
            onclick={() => list(a.workspace, a.account)}
          >
            {lv?.loading ? 'Listing...' : 'List tools'}
          </Button>
        {/if}
      </div>
      <Table.Root class="mcp-table">
        <Table.Header>
          <Table.Row>
            <Table.Head>Function</Table.Head>
            <Table.Head>Tool</Table.Head>
            <Table.Head>Approval</Table.Head>
            <Table.Head>Description</Table.Head>
          </Table.Row>
        </Table.Header>
        <Table.Body>
          {#each a.functions as f (f.function)}
            <Table.Row>
              <Table.Cell class="mono">{f.function}</Table.Cell>
              <Table.Cell class="mono">{f.tool}</Table.Cell>
              <Table.Cell class={f.approved ? 'ok' : 'warn'}>{f.approved ? 'approved' : 'not approved'}</Table.Cell>
              <Table.Cell>{f.description ?? ''}</Table.Cell>
            </Table.Row>
          {/each}
        </Table.Body>
      </Table.Root>
      {#if keyless}
        <p class="muted">{LISTING_REFUSED}</p>
      {/if}
      <div
        class="result"
        aria-live="polite"
        aria-busy={lv?.loading ?? false}
        aria-label={`Live listing of ${a.account}${a.rootLabel ? ` in ${a.rootLabel}` : ''}`}
      >
        {#if lv && !lv.loading}
          {#if 'refused' in lv}
            <p class="refused">{LISTING_REFUSED}</p>
          {:else if 'failure' in lv}
            <p class="warn">{lv.failure}</p>
          {:else if !lv.result.ok}
            <p class="warn">{lv.result.error.message}</p>
            {#each lv.result.warnings as w, i (i)}
              <p class="warn">{w}</p>
            {/each}
          {:else}
            {@const listing = lv.result.listing}
            <div class="live">
              <p class="muted">
                Live from <span class="mono">{listing.server.program}</span> ({listing.era})
              </p>
              {#if listing.cleanup}
                <p class="warn">{listing.cleanup}</p>
              {/if}
              <Table.Root class="mcp-table">
                <Table.Header>
                  <Table.Row>
                    <Table.Head>Function</Table.Head>
                    <Table.Head>Tool</Table.Head>
                    <Table.Head>Status</Table.Head>
                    <Table.Head>Description</Table.Head>
                  </Table.Row>
                </Table.Header>
                <Table.Body>
                  {#each listing.rows as r (r.function)}
                    <Table.Row>
                      <Table.Cell class="mono">{r.function}</Table.Cell>
                      <Table.Cell class="mono"
                        >{r.tool}{#if r.title}<span class="muted"> {r.title}</span>{/if}</Table.Cell
                      >
                      <Table.Cell class={r.status === 'pinned' ? 'ok' : 'warn'}>
                        {statusLabel(r.status)}{#if r.members.length > 0}<span class="muted">
                            ({r.members.join(', ')})</span
                          >{/if}
                      </Table.Cell>
                      <Table.Cell>
                        {r.description ?? ''}
                        {#if r.pinnedDescription !== null && r.pinnedDescription !== r.description}
                          <div class="muted">approved: {r.pinnedDescription}</div>
                        {/if}
                        {#if r.annotations}
                          <details>
                            <summary>annotations (shown, not used)</summary>
                            <pre>{shown(r.annotations)}</pre>
                          </details>
                        {/if}
                        {#if r.inputSchema}
                          <details>
                            <summary>input schema</summary>
                            <pre>{shown(r.inputSchema)}</pre>
                          </details>
                        {/if}
                      </Table.Cell>
                    </Table.Row>
                  {/each}
                </Table.Body>
              </Table.Root>
              {#if listing.unmapped.length > 0}
                <p class="muted">Not mapped by any function: <span class="mono">{listing.unmapped.join(', ')}</span></p>
              {/if}
              {#if needsApproval(listing)}
                <p class="warn">A person approves these tools again at a terminal (docs/CONNECTORS.md, MCP servers).</p>
              {/if}
            </div>
          {/if}
        {/if}
      </div>
    </div>
  {/each}
</section>

<style>
  .mcp {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 20px 16px 16px;
  }
  .facts {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 2px 12px;
    margin: 0;
    font-size: 12px;
  }
  .facts dt {
    color: var(--c-muted);
  }
  .facts dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .muted {
    color: var(--c-muted);
  }
  p {
    margin: 0;
    font-size: 12px;
  }
  .account {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding-top: 8px;
    border-top: 1px solid var(--c-line);
  }
  .account-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .account-head h3 {
    display: flex;
    gap: 8px;
    margin: 0;
    font-size: 12px;
    font-weight: 500;
  }
  .refused {
    color: var(--c-muted);
  }
  .result {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .mcp :global(.mcp-table) {
    font-size: 12px;
  }
  .mcp :global(.mcp-table td),
  .mcp :global(.mcp-table th) {
    vertical-align: top;
    white-space: normal;
    overflow-wrap: anywhere;
  }
  .mcp :global(.mono) {
    font-family: var(--font-mono, monospace);
  }
  .mcp :global(.ok) {
    color: var(--c-green);
  }
  .mcp :global(.warn) {
    color: var(--c-amber);
  }
  .live {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  details summary {
    color: var(--c-muted);
    cursor: pointer;
  }
  pre {
    max-height: 240px;
    margin: 4px 0 0;
    overflow: auto;
    font-family: var(--font-mono, monospace);
    font-size: 11px;
    white-space: pre-wrap;
  }
</style>
