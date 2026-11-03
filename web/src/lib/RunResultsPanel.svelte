<script lang="ts">
  // The result snapshots of a run: grouped by node, then execution, then
  // list item, each row with its status, the attempt that produced it, the
  // commit it was taken at and its badges (redacted, incomplete, omitted,
  // corrupt, cache hit), every stored entry and every omission with its
  // reason. "View" shows an entry's stored text in a dialog as plain text
  // (a `<pre>`, never rendered markup), up to the display cap at a time;
  // "Download" links the whole entry; "Compare with live" answers for a
  // workspace file in one word, never with live content. Artifact refs
  // recorded without stored bytes are listed apart, and so are the claims
  // each evidence session checked (`EvidenceClaims`). Absent for a run with
  // none of these.
  import * as Card from '$lib/components/ui/card'
  import * as Dialog from '$lib/components/ui/dialog'
  import { Button } from '$lib/components/ui/button'
  import EvidenceClaims from './EvidenceClaims.svelte'
  import { compareResultLive, fetchResultEntry, refusalText, resultRawUrl } from './api'
  import {
    DISPLAY_CAP,
    canCompareLive,
    contentNote,
    entryName,
    formatBytes,
    groupResults,
    isRedactedPath,
    legacyLines,
    notStoredLines,
    liveWord,
    omissionLines,
    rowBadges,
    shortDigest,
    snapshotAnchorId,
    type EntryContent,
    type EntryView,
    type ResultRow,
    type ResultsAnswer,
  } from './results'

  let { runId, view = null, workspace = '' }: { runId: string; view?: ResultsAnswer | null; workspace?: string } = $props()

  const evidence = $derived(view?.evidence ?? [])

  const groups = $derived(groupResults(view?.rows ?? []))
  const legacy = $derived(legacyLines(view?.legacy ?? []))
  const notStored = $derived(notStoredLines(view?.not_stored ?? []))

  // The ids a node group is reached by, safe as an element id.
  const groupId = (node: string) => `run-results-${encodeURIComponent(node)}`

  function scrollTo(node: string) {
    document.getElementById(groupId(node))?.scrollIntoView({ block: 'start' })
  }

  interface Viewing {
    row: ResultRow
    entry: EntryView
    content: EntryContent | null
    error: string | null
    loading: boolean
  }
  let viewing = $state<Viewing | null>(null)
  let open = $state(false)

  async function loadMore(v: Viewing) {
    const offset = v.content ? v.content.range[1] : 0
    v.loading = true
    try {
      const next = await fetchResultEntry(
        runId,
        v.row.snapshot_id,
        v.entry.entry,
        { offset, limit: DISPLAY_CAP },
        workspace,
      )
      v.content = v.content
        ? { ...next, text: v.content.text + next.text, range: [v.content.range[0], next.range[1]] }
        : next
      v.error = null
    } catch (e) {
      v.error = refusalText(e)
    } finally {
      v.loading = false
    }
  }

  function show(row: ResultRow, entry: EntryView) {
    viewing = { row, entry, content: null, error: null, loading: false }
    open = true
    void loadMore(viewing)
  }

  // The live comparison per snapshot entry: a word, or why it failed.
  let live = $state<Record<string, string>>({})
  const liveKey = (row: ResultRow, e: EntryView) => `${row.snapshot_id}/${e.entry}`

  async function compare(row: ResultRow, e: EntryView) {
    const key = liveKey(row, e)
    live[key] = 'comparing'
    try {
      live[key] = liveWord(await compareResultLive(runId, row.snapshot_id, e.entry, workspace))
    } catch (err) {
      live[key] = `comparison failed: ${refusalText(err)}`
    }
  }

  const rowHead = (row: ResultRow, ordinal: number) => (row.item ? `#${ordinal} · item ${row.item.id}` : `#${ordinal}`)
</script>

{#if groups.length || legacy.length || notStored.length || evidence.length}
  <Card.Root data-testid="run-results">
    <Card.Header>
      <Card.Title>Results</Card.Title>
    </Card.Header>
    <Card.Content>
      <div class="results">
        {#if groups.length > 1}
          <nav class="index" data-testid="run-results-index" aria-label="Results by node">
            {#each groups as g (g.node)}
              <button type="button" class="jump" onclick={() => scrollTo(g.node)}>
                <span class="node">{g.node}</span> Results ({g.count})
              </button>
            {/each}
          </nav>
        {/if}

        {#each groups as g (g.node)}
          <section class="group" id={groupId(g.node)} data-testid="run-results-group">
            <h4 class="node">{g.node}</h4>
            {#each g.executions as x (x.ordinal)}
              {#each x.rows as row (row.settlement_seq)}
                {@const ok = row.verified === 'ok'}
                <div class="row" id={snapshotAnchorId(row.snapshot_id)} data-testid="run-results-row">
                  <div class="head">
                    <span class="code">{rowHead(row, x.ordinal)}</span>
                    <span class="muted">{row.status}</span>
                    {#if row.attempt_number !== undefined}
                      <span class="muted" title={`attempt id ${row.produced_by_attempt}`}>attempt {row.attempt_number}</span>
                    {/if}
                    {#if row.provenance.head}
                      <span class="muted" title={row.provenance.head}>taken at {row.provenance.head.slice(0, 12)}</span>
                    {/if}
                    {#each rowBadges(row) as b (b.text)}
                      <span class="badge {b.tone}" data-testid="run-results-badge">{b.text}</span>
                    {/each}
                  </div>
                  <ul class="entries">
                    {#each row.entries as e (e.entry)}
                      {@const key = liveKey(row, e)}
                      <li class="entry">
                        <span class="code name" data-testid="run-results-entry-name">
                          {entryName(e)}{#if isRedactedPath(e.path)}{' '}<span class="badge warn">path redacted</span>{/if}
                        </span>
                        <span data-testid="run-results-entry-stored">
                          {#if e.size !== undefined && e.digest !== undefined}
                            <span class="muted">{formatBytes(e.size)}</span>
                            <span class="code" title={e.digest}>{shortDigest(e.digest)}</span>
                          {:else}
                            <span class="muted">unverified</span>
                          {/if}
                        </span>
                        {#if e.format === 'records'}<span class="muted">records</span>{/if}
                        {#if ok}
                          <span class="actions">
                            <Button variant="outline" size="xs" data-testid="run-results-view" onclick={() => show(row, e)}>
                              View
                            </Button>
                            <a
                              class="link"
                              data-testid="run-results-download"
                              href={resultRawUrl(runId, row.snapshot_id, e.entry, workspace)}
                              download
                            >
                              Download
                            </a>
                            {#if canCompareLive(e)}
                              <Button
                                variant="outline"
                                size="xs"
                                data-testid="run-results-compare"
                                disabled={live[key] === 'comparing'}
                                onclick={() => compare(row, e)}
                              >
                                Compare with live
                              </Button>
                            {/if}
                          </span>
                        {/if}
                        {#if live[key] && live[key] !== 'comparing'}
                          <span class="live" data-testid="run-results-live">{live[key]}</span>
                        {/if}
                      </li>
                    {/each}
                  </ul>
                  {#if row.omitted.length}
                    <ul class="omitted">
                      {#each omissionLines(row.omitted) as line, i (i)}
                        <li data-testid="run-results-omission">{line}</li>
                      {/each}
                    </ul>
                  {/if}
                </div>
              {/each}
            {/each}
          </section>
        {/each}

        {#if notStored.length}
          <section class="group" data-testid="run-results-not-stored">
            <h4>Not stored</h4>
            <ul class="omitted">
              {#each notStored as line, i (i)}
                <li>{line}</li>
              {/each}
            </ul>
          </section>
        {/if}

        {#if evidence.length}
          <EvidenceClaims rows={evidence} />
        {/if}

        {#if legacy.length}
          <section class="group" data-testid="run-results-legacy">
            <h4>Recorded digests (no stored bytes)</h4>
            <ul class="omitted">
              {#each legacy as line, i (i)}
                <li class="code">{line}</li>
              {/each}
            </ul>
          </section>
        {/if}
      </div>
    </Card.Content>
  </Card.Root>
{/if}

<Dialog.Root bind:open>
  <Dialog.Content class="results-dialog">
    {#if viewing}
      {@const v = viewing}
      <Dialog.Header>
        <Dialog.Title>{v.row.execution.node} #{v.row.execution.ordinal} · {entryName(v.entry)}</Dialog.Title>
        <Dialog.Description>
          {#if v.entry.size !== undefined && v.entry.digest !== undefined}
            <span class="code" title={v.entry.digest}>{shortDigest(v.entry.digest)}</span>
            · {formatBytes(v.entry.size)}
          {/if}
          {#if v.content?.redacted_at_capture}· redacted at capture{/if}
          {#if v.content?.redacted_on_read}· redacted on read{/if}
        </Dialog.Description>
      </Dialog.Header>
      <!-- A failed "Show more" keeps the text already loaded; its error
           shows next to it. -->
      {#if v.content}
        {@const note = contentNote(v.content)}
        <pre class="text" data-testid="run-results-text">{v.content.text}</pre>
        {#if note}
          <p class="muted">{note}</p>
          {#if v.content.range[1] < v.content.size}
            <Button variant="outline" size="xs" disabled={v.loading} onclick={() => loadMore(v)}>Show more</Button>
          {/if}
        {/if}
      {/if}
      {#if v.error}
        <p class="error">{v.error}</p>
      {:else if !v.content}
        <p class="muted">Loading</p>
      {/if}
    {/if}
  </Dialog.Content>
</Dialog.Root>

<style>
  /* Plain CSS over the app's variables. */
  .results {
    display: flex;
    flex-direction: column;
    gap: 12px;
    font-size: 12px;
  }
  .index {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
  }
  .jump {
    background: none;
    border: 0;
    padding: 0;
    color: var(--primary);
    cursor: pointer;
    font: inherit;
  }
  .jump:hover {
    text-decoration: underline;
  }
  .group {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-width: 0;
    scroll-margin-top: 16px;
  }
  h4 {
    margin: 0;
    font-size: 12px;
    font-weight: 600;
  }
  .row {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding-left: 8px;
    border-left: 2px solid var(--border);
    min-width: 0;
  }
  .head,
  .entry {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 8px;
  }
  .node,
  .code {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .name {
    overflow-wrap: anywhere;
    min-width: 0;
  }
  .muted,
  .code:not(.name) {
    color: var(--muted-foreground);
  }
  .badge {
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 1px 8px;
    margin-left: 4px;
  }
  .badge.bad {
    border-color: var(--destructive);
    color: var(--destructive);
  }
  .badge.warn {
    border-color: var(--c-amber, var(--border));
    color: var(--c-amber, inherit);
  }
  ul {
    margin: 0;
    padding-left: 18px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    overflow-wrap: anywhere;
  }
  .actions {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .link {
    color: var(--primary);
  }
  .link:hover {
    text-decoration: underline;
  }
  .live {
    color: var(--foreground);
  }
  .omitted {
    color: var(--muted-foreground);
  }
  .error {
    color: var(--destructive);
    margin: 0;
  }
  .text {
    margin: 0;
    max-height: 60vh;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 12px;
    background: var(--muted, transparent);
    padding: 8px;
  }
  :global(.results-dialog) {
    max-width: min(56rem, calc(100% - 2rem));
  }
  /* A long entry name wraps clear of the close button in the corner. */
  :global(.results-dialog [data-slot='dialog-title']) {
    padding-right: 2rem;
    overflow-wrap: anywhere;
  }
</style>
