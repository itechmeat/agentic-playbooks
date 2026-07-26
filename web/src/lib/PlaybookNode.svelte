<script lang="ts">
  import { Handle, Position } from '@xyflow/svelte'
  import { cn } from '$lib/utils'
  import { Badge } from '$lib/components/ui/badge'
  import OctagonX from '@lucide/svelte/icons/octagon-x'
  import CornerDownRight from '@lucide/svelte/icons/corner-down-right'
  import type { FailureEffect, NodeExits } from './graph'

  let {
    data,
  }: {
    data: {
      title: string
      kind: string
      status?: string
      cached?: boolean
      // What the playbook's failure policy does here when no edge carries
      // this node's failure (see `failureEffect` in graph.ts).
      failure?: FailureEffect | null
      // The node's named ways out, when it has more than one (see `nodeExits`).
      exits?: NodeExits | null
    }
  } = $props()

  // start - entry point, must have no incoming edge;
  // finish - terminal, must have no outgoing edge.
  const hasTarget = $derived(data.kind !== 'start')
  // One anonymous dot at the bottom, which is also what an editor drags a new
  // edge from. A node with named exits draws one dot per exit instead.
  const hasSource = $derived(data.kind !== 'finish' && !data.exits)

  const exitTone = { success: 'text-success', failure: 'text-destructive', default: 'text-muted-foreground' } as const

  const modeBadge = $derived.by(() => {
    if (!data.exits) return null
    const n = data.exits.list.length
    return data.exits.mode === 'all'
      ? { text: `all ${n}`, title: `All ${n} exits are taken: the branches run in parallel.` }
      : {
          text: `1 of ${n}`,
          title: `Exactly one of these ${n} exits is taken: the first whose condition matches, in the numbered order.`,
        }
  })

  const statusRing = $derived.by(() => {
    switch (data.status) {
      case 'running':
        return 'border-chart-1 ring-2 ring-chart-1/40'
      case 'succeeded':
        return 'border-success'
      case 'failed':
      case 'timed_out':
        return 'border-destructive'
      case 'interrupted':
      case 'unknown':
        return 'border-warning'
      default:
        return 'border-border'
    }
  })
</script>

<div
  class={cn(
    'min-w-40 rounded-lg border bg-card px-3 py-2 text-card-foreground shadow-sm',
    data.kind === 'condition' && 'border-dashed',
    statusRing,
  )}
>
  {#if hasTarget}<Handle type="target" position={Position.Top} />{/if}
  <div class="flex items-center gap-1">
    <span class="text-[11px] text-muted-foreground">{data.kind}</span>
    {#if modeBadge}
      <!-- Several lines leaving one node mean two opposite things - all of them
           run, or exactly one does - and the graph alone cannot say which. -->
      <Badge
        variant="outline"
        class="h-4 rounded-sm px-1 py-0 text-[10px] leading-none text-muted-foreground"
        title={modeBadge.title}
      >
        {modeBadge.text}
      </Badge>
    {/if}
  </div>
  <strong class="block text-sm">{data.title}</strong>
  {#if data.status || data.cached || data.failure}
    <div class="mt-0.5 flex items-center gap-1">
      {#if data.status}
        <span class="text-[11px] text-muted-foreground">{data.status}</span>
      {/if}
      {#if data.cached}
        <Badge variant="secondary" class="h-4 rounded-sm px-1 py-0 text-[10px] leading-none">cached</Badge>
      {/if}
      {#if data.failure}
        <!-- The failure branch that is not drawn: without this the graph reads
             as if nobody thought about what happens when this node fails. -->
        <span
          class="flex items-center gap-0.5 text-[10px] leading-none text-destructive"
          title={data.failure.kind === 'stop'
            ? 'Failure ends the run'
            : `Failure goes to ${data.failure.node}`}
        >
          {#if data.failure.kind === 'stop'}
            <OctagonX class="size-3" />
            stop on failure
          {:else}
            <CornerDownRight class="size-3" />
            on failure: {data.failure.node}
          {/if}
        </span>
      {/if}
    </div>
  {/if}

  {#if data.exits}
    <!-- One dot per exit, each under its own caption, so no two lines share a
         point and every line says on the node itself why it is taken. -->
    <div class="-mx-1 mt-1.5 flex items-start border-t border-border/60 pt-1">
      {#each data.exits.list as exit, i (exit.id)}
        <span
          class={cn('flex-1 px-0.5 text-center text-[9px] leading-tight', exitTone[exit.tone])}
          title={exit.title}
        >
          {exit.label}
        </span>
        <Handle
          type="source"
          id={exit.id}
          position={Position.Bottom}
          style={`left: ${(((i + 0.5) / data.exits.list.length) * 100).toFixed(2)}%`}
        />
      {/each}
    </div>
  {/if}
  {#if hasSource}<Handle type="source" position={Position.Bottom} />{/if}
</div>
