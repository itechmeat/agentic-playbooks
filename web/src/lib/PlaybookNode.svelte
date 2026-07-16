<script lang="ts">
  import { Handle, Position } from '@xyflow/svelte'
  import { cn } from '$lib/utils'

  let { data }: { data: { title: string; kind: string; status?: string } } = $props()

  // start - entry point, must have no incoming edge;
  // finish - terminal, must have no outgoing edge.
  const hasTarget = $derived(data.kind !== 'start')
  const hasSource = $derived(data.kind !== 'finish')

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
  <span class="block text-[11px] text-muted-foreground">{data.kind}</span>
  <strong class="block text-sm">{data.title}</strong>
  {#if data.status}
    <span class="mt-0.5 block text-[11px] text-muted-foreground">{data.status}</span>
  {/if}
  {#if hasSource}<Handle type="source" position={Position.Bottom} />{/if}
</div>
