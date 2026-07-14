<script lang="ts">
  import { SvelteFlow, Background, Controls } from '@xyflow/svelte'
  import '@xyflow/svelte/dist/style.css'
  import { fetchRun, fetchRunReport, postReview } from '../lib/api'
  import { toFlow, type FlowEdge, type FlowNode } from '../lib/graph'
  import { interventionJournal } from '../lib/journal'
  import { pendingReviews } from '../lib/reviews'
  import { pendingWaits } from '../lib/waits'
  import { subscribeChanges } from '../lib/ws'
  import PlaybookNode from '../lib/PlaybookNode.svelte'
  import type { RunDetail } from '../lib/types'

  let { id }: { id: string } = $props()

  let nodes = $state.raw<FlowNode[]>([])
  let edges = $state.raw<FlowEdge[]>([])
  let detail = $state<RunDetail | null>(null)
  let error = $state<string | null>(null)
  let report = $state<string | null>(null)
  let tab = $state<'events' | 'journal' | 'report'>('events')
  let reviewError = $state<string | null>(null)
  let deciding = $state<string | null>(null)

  const nodeTypes = { playbookNode: PlaybookNode }
  const journal = $derived(detail ? interventionJournal(detail.events) : [])
  const pending = $derived(detail ? pendingReviews(detail.events) : [])
  const waiting = $derived(detail ? pendingWaits(detail.events) : [])
  const hooks = $derived(detail?.hooks ?? {})
  const hookEntries = $derived(Object.entries(hooks))

  async function decide(node: string, decision: string) {
    reviewError = null
    deciding = `${node}:${decision}`
    try {
      await postReview(id, node, decision)
      await load()
    } catch (e) {
      reviewError = String(e)
    } finally {
      deciding = null
    }
  }

  async function load() {
    try {
      const d = await fetchRun(id)
      detail = d
      if (d.model) {
        const flow = toFlow(d.model, null, d.nodes)
        nodes = flow.nodes
        edges = flow.edges
      }
      error = null
    } catch (e) {
      error = String(e)
    }
    try {
      const r = await fetchRunReport(id)
      report = r.report
    } catch {
      report = null
    }
  }

  $effect(() => {
    load()
    return subscribeChanges(load)
  })
</script>

<header class="topbar">
  <a href="#/runs">runs</a>
  <h1>{id}</h1>
  {#if detail}<span class="muted">{detail.playbook} · {detail.run_status}</span>{/if}
  <span class="spacer"></span>
  {#if error}<span class="badge err" title={error}>error</span>{/if}
</header>

<div class="run-body">
  <div class="graph">
    <SvelteFlow bind:nodes bind:edges {nodeTypes} fitView
      nodesDraggable={false} nodesConnectable={false} elementsSelectable={false}>
      <Background />
      <Controls />
    </SvelteFlow>
  </div>
  <aside class="side">
    {#if pending.length}
      <section class="reviews">
        <h2>Human review</h2>
        {#if reviewError}<p class="err-text">{reviewError}</p>{/if}
        {#each pending as pr (pr.node)}
          <div class="review">
            <span class="node">{pr.node}</span>
            <div class="opts">
              {#each pr.options as opt (opt)}
                <button onclick={() => decide(pr.node, opt)} disabled={deciding === `${pr.node}:${opt}`}>
                  {deciding === `${pr.node}:${opt}` ? '...' : opt}
                </button>
              {/each}
            </div>
          </div>
        {/each}
      </section>
    {/if}
    {#if hookEntries.length}
      <section class="hooks">
        <h2>Webhooks</h2>
        {#each hookEntries as [key, path] (key)}
          <div class="hook">
            <span class="hkey">{key}</span>
            <code class="hurl">{location.origin}{path}</code>
          </div>
        {/each}
      </section>
    {/if}
    {#if waiting.length}
      <section class="waiting">
        <h2>Waiting</h2>
        {#each waiting as node (node)}
          <div class="wnode"><span class="node">{node}</span> <span class="muted">awaiting signal or timer</span></div>
        {/each}
      </section>
    {/if}
    <nav class="tabs">
      <button class:active={tab === 'events'} onclick={() => (tab = 'events')}>events</button>
      <button class:active={tab === 'journal'} onclick={() => (tab = 'journal')}>journal{#if journal.length} ({journal.length}){/if}</button>
      <button class:active={tab === 'report'} onclick={() => (tab = 'report')}>report</button>
    </nav>
    {#if tab === 'events'}
      {#if detail}
        <ol class="events">
          {#each detail.events as e (e.seq)}
            <li><code>{e.type}</code>{#if e.node} <span class="muted">{e.node}</span>{/if}</li>
          {/each}
        </ol>
      {/if}
    {:else if tab === 'journal'}
      {#if journal.length}
        <ol class="journal">
          {#each journal as entry (entry.seq)}
            <li class="entry {entry.kind}">
              <span class="kind">{entry.kind}</span>
              <span class="label">{entry.label}</span>
              {#if entry.node}<span class="muted">{entry.node}</span>{/if}
              {#if entry.detail}<p class="detail">{entry.detail}</p>{/if}
            </li>
          {/each}
        </ol>
      {:else}
        <p class="muted">no interventions yet</p>
      {/if}
    {:else if tab === 'report'}
      {#if report}
        <pre class="report">{report}</pre>
      {:else}
        <p class="muted">no report yet</p>
      {/if}
    {/if}
  </aside>
</div>

<style>
  .run-body { flex: 1 1 auto; min-height: 0; display: flex; }
  .graph { flex: 1 1 auto; min-height: 0; position: relative; }
  .side {
    flex: 0 0 300px;
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: auto;
    border-left: 1px solid var(--border);
    padding: 8px 10px;
    font-size: 12px;
  }
  .tabs {
    display: flex;
    gap: 4px;
    margin-bottom: 8px;
    border-bottom: 1px solid var(--border);
  }
  .tabs button {
    background: none;
    border: none;
    padding: 4px 8px;
    font-size: 12px;
    color: var(--muted);
    cursor: pointer;
  }
  .tabs button.active {
    color: var(--fg);
    border-bottom: 2px solid var(--accent);
  }
  .events, .journal { margin: 0; padding-left: 18px; }
  .events code { font-size: 11px; }
  .journal { list-style: none; padding-left: 0; }
  .journal .entry {
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 6px 8px;
    margin-bottom: 6px;
  }
  .journal .kind {
    display: inline-block;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--accent);
    margin-right: 6px;
  }
  .journal .label { font-weight: 600; }
  .journal .detail { margin: 4px 0 0; color: var(--muted); }
  .report {
    white-space: pre-wrap;
    word-break: break-word;
    font-size: 12px;
    margin: 0;
  }
  .muted { opacity: 0.6; }
  .reviews {
    border: 1px solid var(--accent);
    border-radius: 6px;
    padding: 8px;
    margin-bottom: 10px;
  }
  .reviews h2 { margin: 0 0 6px; font-size: 12px; font-weight: 600; }
  .reviews .review { display: flex; align-items: center; gap: 8px; margin-top: 4px; flex-wrap: wrap; }
  .reviews .node { font-family: ui-monospace, monospace; }
  .reviews .opts { display: flex; gap: 6px; }
  .reviews button {
    font-size: 12px;
    padding: 2px 10px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: transparent;
    color: var(--fg);
    cursor: pointer;
  }
  .reviews button:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
  .reviews button:disabled { opacity: 0.6; cursor: default; }
  .err-text { color: var(--err); margin: 0 0 6px; }
  .hooks, .waiting {
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 8px;
    margin-bottom: 10px;
  }
  .hooks h2, .waiting h2 { margin: 0 0 6px; font-size: 12px; font-weight: 600; }
  .hooks .hook { margin-top: 4px; }
  .hooks .hkey { font-weight: 600; font-family: ui-monospace, monospace; }
  .hooks .hurl { display: block; font-size: 11px; word-break: break-all; color: var(--muted); }
  .waiting .wnode { margin-top: 4px; }
  .waiting .node { font-family: ui-monospace, monospace; }
</style>
