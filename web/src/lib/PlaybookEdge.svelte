<script lang="ts">
  import { BaseEdge, EdgeLabel, getBezierPath, type EdgeProps } from '@xyflow/svelte'
  import type { FlowEdgeData } from './graph'
  import { edgeRunLabel } from './runoverlay'

  // The graph edge of the terminal look. Register it as the default edge type
  // so every edge `toFlow` builds uses it:
  //   <SvelteFlow edgeTypes={{ default: PlaybookEdge }} ...>
  // Its look comes from `data` (FlowEdgeData in graph.ts): `tone` (plain,
  // cond, fail, else) from the playbook, the optional `run` look from a run
  // graph (taken, not taken, not established) and an issue marker.
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
  const run = $derived(d.run)
  // The state class; `live` decorates an established delivery only.
  const state = $derived(run ? `run-${run.state.replace('_', '-')}${run.live ? ' run-live' : ''}` : '')
  const runText = $derived(edgeRunLabel(run))

  let [path, labelX, labelY] = $derived(
    getBezierPath({ sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition }),
  )

  // One marker per edge, colored by the same class as the line (a shared
  // marker could not follow each edge's color).
  const markerId = $derived(`apb-arrow-${String(id).replace(/[^A-Za-z0-9_-]/g, '_')}`)
  const lineClass = $derived(
    ['apb-edge', `tone-${tone}`, state, d.highlight ? 'issue-target' : '', d.diff ? `diff-${d.diff}` : '']
      .filter(Boolean)
      .join(' '),
  )
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
    <path d="M0,0 L8,4 L0,8 z" class={['apb-arrow', `tone-${tone}`, state]} />
  </marker>
</defs>

<BaseEdge {id} {path} {interactionWidth} markerEnd={`url(#${markerId})`} class={lineClass} />

{#if label || runText || d.issue || run?.state === 'not_established'}
  <EdgeLabel x={labelX} y={labelY} selectEdgeOnClick>
    <span
      class={['edge-label', `tone-${tone}`, run && `run-${run.state}`]}
      title={[
        run?.title,
        d.issue?.text,
        d.before || d.after ? `before ${d.before ?? 'none'}, after ${d.after ?? 'none'}` : '',
      ]
        .filter(Boolean)
        .join('\n') || undefined}
      data-testid="edge-label"
    >
      {#if d.issue}<span class={['edge-issue', d.issue.error && 'error']} aria-label="validation issue">!</span>{/if}
      {#if label}<span>{label}</span>{/if}
      {#if runText}<span class="edge-run">{runText}</span>{/if}
      {#if run?.state === 'not_established' && !label && !runText}<span class="edge-run">?</span>{/if}
    </span>
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

  /* Run states, from the run graph only: taken solid, not taken dimmed, not
     established dotted. */
  :global(path.apb-edge.run-taken) {
    stroke: color-mix(in oklab, var(--c-green) 60%, var(--c-bg));
    stroke-width: 1.6;
    stroke-dasharray: none;
    opacity: 1;
  }
  .apb-arrow.run-taken {
    fill: color-mix(in oklab, var(--c-green) 60%, var(--c-bg));
    opacity: 1;
  }
  :global(path.apb-edge.run-not-taken) {
    opacity: 0.3;
  }
  .apb-arrow.run-not-taken {
    opacity: 0.3;
  }
  :global(path.apb-edge.run-not-established) {
    stroke: var(--c-amber);
    stroke-dasharray: 1 4;
    stroke-linecap: round;
    opacity: 0.8;
  }
  .apb-arrow.run-not-established {
    fill: var(--c-amber);
    opacity: 0.8;
  }
  /* Decoration on an established delivery into a node running now. */
  :global(path.apb-edge.run-taken.run-live) {
    stroke: var(--c-blue);
    stroke-dasharray: 5 5;
    animation: apb-edge-flow 0.8s linear infinite;
  }
  .apb-arrow.run-live {
    fill: var(--c-blue);
  }
  @media (prefers-reduced-motion: reduce) {
    :global(path.apb-edge.run-taken.run-live) {
      animation: none;
    }
  }

  /* The target of a selected validation issue. */
  :global(path.apb-edge.issue-target) {
    stroke: var(--c-amber);
    stroke-width: 2.4;
    opacity: 1;
  }

  /* The diff canvas: a removed edge is a ghost, an added one in the
     success tone, a changed one in the warning tone. */
  :global(path.apb-edge.diff-removed) {
    stroke-dasharray: 3 4;
    opacity: 0.35;
  }
  :global(path.apb-edge.diff-added) {
    stroke: var(--c-green);
    stroke-width: 1.8;
  }
  :global(path.apb-edge.diff-changed) {
    stroke: var(--c-amber);
    stroke-width: 1.8;
  }

  /* The editor's selected edge. */
  :global(.svelte-flow__edge.selected path.apb-edge),
  :global(.svelte-flow__edge:focus-visible path.apb-edge) {
    stroke: var(--c-violet);
    opacity: 1;
  }

  .edge-label {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .edge-label.tone-fail {
    color: var(--c-red);
  }
  .edge-label.tone-else,
  .edge-label.run-not_taken {
    opacity: 0.5;
  }
  .edge-run {
    color: var(--c-fg-2);
    font-variant-numeric: tabular-nums;
  }
  .edge-label.run-not_established .edge-run {
    color: var(--c-amber);
  }
  .edge-issue {
    padding: 0 3px;
    border: 1px solid var(--c-amber);
    color: var(--c-amber);
    font-weight: 700;
    line-height: 1.1;
  }
  .edge-issue.error {
    border-color: var(--c-red);
    color: var(--c-red);
  }

  @keyframes apb-edge-flow {
    to {
      stroke-dashoffset: -10;
    }
  }
</style>
