<script lang="ts">
  import { accentVar, profileColor } from './profilecolor'
  import { shortModel } from './format'

  // The legend row under the top bar of the run page: each profile the run
  // uses with its color (and model when known), then the gate kinds the
  // playbook has, then the run states the graph cards show.
  let {
    profiles = [],
    decides = false,
    humanGate = false,
  }: { profiles?: { name: string; model?: string }[]; decides?: boolean; humanGate?: boolean } = $props()

  const states = [
    { label: 'running', color: accentVar('blue') },
    { label: 'done', color: accentVar('green') },
    { label: 'failure', color: accentVar('red') },
  ]
</script>

<div
  class="flex shrink-0 flex-wrap items-center gap-x-5 gap-y-1 border-b border-line px-4 py-1.5 text-xs text-fg-2"
  data-testid="run-legend"
>
  {#each profiles as p (p.name)}
    <span class="legend-sq whitespace-nowrap" style:--k={profileColor(p.name)}
      >{p.name}{#if p.model}<span class="text-muted-foreground" title={p.model}>{` · ${shortModel(p.model)}`}</span>{/if}</span
    >
  {/each}
  {#if decides || humanGate}
    {#if profiles.length}<span class="text-faint" aria-hidden="true">|</span>{/if}
    {#if decides}<span class="legend-sq whitespace-nowrap" style:--k={accentVar('green')}>decides</span>{/if}
    {#if humanGate}<span class="legend-sq whitespace-nowrap" style:--k={accentVar('amber')}>human gate</span>{/if}
  {/if}
  {#if profiles.length || decides || humanGate}<span class="text-faint" aria-hidden="true">|</span>{/if}
  {#each states as s (s.label)}
    <span class="legend-sq whitespace-nowrap" style:--k={s.color}>{s.label}</span>
  {/each}
</div>
