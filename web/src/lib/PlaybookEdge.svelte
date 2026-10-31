<script lang="ts">
  import { BaseEdge, EdgeLabel, getBezierPath, type EdgeProps } from '@xyflow/svelte'
  import type { FlowEdgeData } from './graph'

  // The graph edge of the terminal look. Register it as the default edge type
  // so every edge `toFlow` builds uses it:
  //   <SvelteFlow edgeTypes={{ default: PlaybookEdge }} ...>
  // Its look comes from `data` (FlowEdgeData in graph.ts): `tone` (plain,
  // cond, fail, else) from the playbook, and the optional run `state`
  // (walked, active, idle).
  let {
    id,
    sourceX,
    sourceY,
    targetX,
    targetY,
    sourcePosition,
    targetPosition,
    label,
    data,
    interactionWidth,
  }: EdgeProps = $props()

  const d = $derived((data ?? {}) as Partial<FlowEdgeData>)
  const tone = $derived(d.tone ?? 'plain')
  const state = $derived(d.state)

  let [path, labelX, labelY] = $derived(
    getBezierPath({ sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition }),
  )

  // One marker per edge, colored by the same class as the line (a shared
  // marker could not follow each edge's color).
  const markerId = $derived(`apb-arrow-${String(id).replace(/[^A-Za-z0-9_-]/g, '_')}`)
</script>

<defs>
  <marker
    id={markerId}
    viewBox="0 0 8 8"
    refX="7"
    refY="4"
    markerWidth="7"
    markerHeight="7"
    orient="auto-start-reverse"
  >
    <path d="M0,0 L8,4 L0,8 z" class={['apb-arrow', `tone-${tone}`, state && `state-${state}`]} />
  </marker>
</defs>

<BaseEdge
  {id}
  {path}
  {interactionWidth}
  markerEnd={`url(#${markerId})`}
  class={['apb-edge', `tone-${tone}`, state && `state-${state}`].filter(Boolean).join(' ')}
/>

{#if label}
  <EdgeLabel
    x={labelX}
    y={labelY}
    selectEdgeOnClick
    class={[tone === 'fail' && 'fail', tone === 'else' && 'else', state === 'idle' && 'opacity-40']
      .filter(Boolean)
      .join(' ')}
  >
    {label}
  </EdgeLabel>
{/if}

<style>
  /* Line colors from the palette; failure dashed red, fallback faint. */
  :global(path.apb-edge) {
    stroke: var(--c-faint);
    stroke-width: 1.2;
    transition:
      stroke 0.3s,
      opacity 0.3s;
  }
  .apb-arrow {
    fill: var(--c-faint);
  }
  :global(path.apb-edge.tone-fail) {
    stroke: color-mix(in oklab, var(--c-red) 75%, transparent);
    stroke-dasharray: 4 4;
  }
  .apb-arrow.tone-fail {
    fill: color-mix(in oklab, var(--c-red) 75%, transparent);
  }
  :global(path.apb-edge.tone-else) {
    stroke-dasharray: 2 4;
    opacity: 0.6;
  }
  .apb-arrow.tone-else {
    opacity: 0.6;
  }

  /* Run states (set by a run view): traversed, leading to the running node,
     not reached. */
  :global(path.apb-edge.state-walked) {
    stroke: color-mix(in oklab, var(--c-green) 55%, var(--c-bg));
    stroke-dasharray: none;
    opacity: 1;
  }
  .apb-arrow.state-walked {
    fill: color-mix(in oklab, var(--c-green) 55%, var(--c-bg));
    opacity: 1;
  }
  :global(path.apb-edge.state-active) {
    stroke: var(--c-blue);
    stroke-width: 1.5;
    stroke-dasharray: 5 5;
    opacity: 1;
    animation: apb-edge-flow 0.8s linear infinite;
  }
  .apb-arrow.state-active {
    fill: var(--c-blue);
    opacity: 1;
  }
  :global(path.apb-edge.state-idle) {
    opacity: 0.35;
  }
  .apb-arrow.state-idle {
    opacity: 0.35;
  }

  /* The editor's selected edge. */
  :global(.svelte-flow__edge.selected path.apb-edge),
  :global(.svelte-flow__edge:focus-visible path.apb-edge) {
    stroke: var(--c-violet);
    opacity: 1;
  }

  @keyframes apb-edge-flow {
    to {
      stroke-dashoffset: -10;
    }
  }
</style>
