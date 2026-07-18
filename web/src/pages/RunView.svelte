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
  import RunProgress from '$lib/RunProgress.svelte'
  import Topbar from '$lib/components/Topbar.svelte'
  import { Button } from '$lib/components/ui/button'
  import { Badge } from '$lib/components/ui/badge'
  import * as Card from '$lib/components/ui/card'
  import * as Tabs from '$lib/components/ui/tabs'
  import { runStatusClass } from '../lib/status'
  import { toast } from 'svelte-sonner'

  let { id, workspace = '' }: { id: string; workspace?: string } = $props()

  let nodes = $state.raw<FlowNode[]>([])
  let edges = $state.raw<FlowEdge[]>([])
  let detail = $state<RunDetail | null>(null)
  let report = $state<string | null>(null)
  let tab = $state<'events' | 'journal' | 'report'>('events')
  let deciding = $state<string | null>(null)

  const nodeTypes = { playbookNode: PlaybookNode }
  const journal = $derived(detail ? interventionJournal(detail.events) : [])
  const pending = $derived(detail ? pendingReviews(detail.events) : [])
  const waiting = $derived(detail ? pendingWaits(detail.events) : [])
  const hookEntries = $derived(Object.entries(detail?.hooks ?? {}))
  const children = $derived(detail?.children ?? [])

  async function decide(node: string, decision: string) {
    deciding = `${node}:${decision}`
    try {
      await postReview(id, node, decision, '', workspace)
      await load()
    } catch (e) {
      toast.error('Review failed', { description: String(e) })
    } finally {
      deciding = null
    }
  }

  async function load() {
    try {
      const d = await fetchRun(id, workspace)
      detail = d
      if (d.model) {
        const flow = toFlow(d.model, d.layout, d.nodes)
        nodes = flow.nodes
        edges = flow.edges
      }
    } catch (e) {
      toast.error('Failed to load run', { description: String(e) })
    }
    try {
      const r = await fetchRunReport(id, workspace)
      report = r.report
    } catch {
      report = null
    }
  }

  $effect(() => {
    // Track the route target so browser back/forward between two runs reloads.
    void id
    void workspace
    load()
    return subscribeChanges(load)
  })
</script>

<Topbar active="runs">
  {#snippet title()}
    <span class="truncate font-mono text-sm font-medium">{id}</span>
    {#if detail}
      <span class="text-sm text-muted-foreground">{detail.playbook}</span>
      <Badge
        variant={runStatusClass(detail.run_status) ? 'outline' : 'secondary'}
        class={runStatusClass(detail.run_status)}
      >
        {detail.run_status}
      </Badge>
    {/if}
  {/snippet}
</Topbar>

{#if detail}
  <div class="border-b border-border px-4 py-2">
    <RunProgress progress={detail.progress} status={detail.run_status} runKey={`${workspace}/${id}`} />
  </div>
{/if}

{#if detail?.answer}
  <div class="border-b border-border px-4 py-2">
    <div class="text-xs font-semibold text-muted-foreground">Answer</div>
    <pre class="mt-1 whitespace-pre-wrap break-words text-sm">{detail.answer}</pre>
  </div>
{/if}

<div class="flex min-h-0 flex-1">
  <div class="relative min-h-0 min-w-0 flex-1">
    <SvelteFlow
      bind:nodes
      bind:edges
      {nodeTypes}
      fitView
      nodesDraggable={false}
      nodesConnectable={false}
      elementsSelectable={false}
    >
      <Background />
      <Controls />
    </SvelteFlow>
  </div>

  <aside class="flex min-h-0 w-80 shrink-0 flex-col gap-3 overflow-auto border-l border-border p-3">
    {#if pending.length}
      <Card.Root class="border-primary/60">
        <Card.Header>
          <Card.Title class="text-sm">Human review</Card.Title>
        </Card.Header>
        <Card.Content class="flex flex-col gap-3">
          {#each pending as pr (pr.node)}
            <div class="flex flex-wrap items-center gap-2">
              <span class="font-mono text-xs">{pr.node}</span>
              <div class="flex flex-wrap gap-1">
                {#each pr.options as opt (opt)}
                  <Button
                    variant="outline"
                    size="sm"
                    class="h-7"
                    onclick={() => decide(pr.node, opt)}
                    disabled={deciding === `${pr.node}:${opt}`}
                  >
                    {opt}
                  </Button>
                {/each}
              </div>
            </div>
          {/each}
        </Card.Content>
      </Card.Root>
    {/if}

    {#if children.length}
      <Card.Root>
        <Card.Header><Card.Title class="text-sm">Child runs</Card.Title></Card.Header>
        <Card.Content class="flex flex-col gap-2">
          {#each children as c (c.run_id)}
            <div class="flex flex-wrap items-center gap-2">
              <a
                href={`#/run/${encodeURIComponent(workspace)}/${encodeURIComponent(c.run_id)}`}
                class="font-mono text-xs hover:underline"
              >
                {c.node_id}
              </a>
              <Badge
                variant={runStatusClass(c.status) ? 'outline' : 'secondary'}
                class={runStatusClass(c.status)}
              >
                {c.status}
              </Badge>
            </div>
          {/each}
        </Card.Content>
      </Card.Root>
    {/if}

    {#if hookEntries.length}
      <Card.Root>
        <Card.Header><Card.Title class="text-sm">Webhooks</Card.Title></Card.Header>
        <Card.Content class="flex flex-col gap-2">
          {#each hookEntries as [key, path] (key)}
            <div>
              <span class="font-mono text-xs font-semibold">{key}</span>
              <code class="block break-all text-[11px] text-muted-foreground"
                >{location.origin}{path}</code
              >
            </div>
          {/each}
        </Card.Content>
      </Card.Root>
    {/if}

    {#if waiting.length}
      <Card.Root>
        <Card.Header><Card.Title class="text-sm">Waiting</Card.Title></Card.Header>
        <Card.Content class="flex flex-col gap-1">
          {#each waiting as node (node)}
            <div class="text-xs">
              <span class="font-mono">{node}</span>
              <span class="text-muted-foreground"> · awaiting signal or timer</span>
            </div>
          {/each}
        </Card.Content>
      </Card.Root>
    {/if}

    {#if detail?.instruction}
      <Card.Root>
        <Card.Header><Card.Title class="text-sm">Run input</Card.Title></Card.Header>
        <Card.Content>
          <pre class="whitespace-pre-wrap break-words text-xs text-muted-foreground">{detail.instruction}</pre>
        </Card.Content>
      </Card.Root>
    {/if}

    <Tabs.Root bind:value={tab} class="min-h-0 flex-1">
      <Tabs.List class="w-full">
        <Tabs.Trigger value="events">events</Tabs.Trigger>
        <Tabs.Trigger value="journal">
          journal{#if journal.length}&nbsp;({journal.length}){/if}
        </Tabs.Trigger>
        <Tabs.Trigger value="report">report</Tabs.Trigger>
      </Tabs.List>

      <Tabs.Content value="events">
        {#if detail}
          <ol class="flex flex-col gap-1 text-xs">
            {#each detail.events as e (e.seq)}
              <li class="flex items-center gap-1.5">
                <code class="rounded bg-muted px-1 py-0.5">{e.type}</code>
                {#if e.node}<span class="text-muted-foreground">{e.node}</span>{/if}
              </li>
            {/each}
          </ol>
        {/if}
      </Tabs.Content>

      <Tabs.Content value="journal">
        {#if journal.length}
          <ol class="flex flex-col gap-2">
            {#each journal as entry (entry.seq)}
              <li class="rounded-md border border-border p-2 text-xs">
                <span class="mr-1.5 text-[10px] uppercase tracking-wide text-primary">{entry.kind}</span>
                <span class="font-semibold">{entry.label}</span>
                {#if entry.node}<span class="text-muted-foreground"> {entry.node}</span>{/if}
                {#if entry.detail}<p class="mt-1 text-muted-foreground">{entry.detail}</p>{/if}
              </li>
            {/each}
          </ol>
        {:else}
          <p class="text-xs text-muted-foreground">no interventions yet</p>
        {/if}
      </Tabs.Content>

      <Tabs.Content value="report">
        {#if report}
          <pre class="whitespace-pre-wrap break-words text-xs">{report}</pre>
        {:else}
          <p class="text-xs text-muted-foreground">no report yet</p>
        {/if}
      </Tabs.Content>
    </Tabs.Root>
  </aside>
</div>
