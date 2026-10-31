<script lang="ts">
  import type { RunDecisions } from './api.gen'
  import type { WfEvent } from './types'
  import { decisionsSummary, decisionUseLines } from './rundecisions'
  import { decisionBars, decisionSource } from './runview'

  // The run's decision layer (issue #165 Part 4): totals, one line per use,
  // and one bar per decision with its node, how decisive the answer was
  // (green when applied or sharp, orange when split or advise only), the
  // value and the verdict. Renders nothing when the run journaled no
  // decision, so an unconfigured run's page is unchanged.
  let { decisions, events }: { decisions?: RunDecisions | null; events: WfEvent[] } = $props()

  const bars = $derived(decisions ? decisionBars(events) : [])
  const source = $derived(decisions ? decisionSource(events) : null)
</script>

{#if decisions}
  <section class="box px-4 pt-4 pb-3" style:--k="var(--c-green)" data-testid="run-decisions">
    <span class="box-cap">Decisions</span>
    <div class="mb-2 flex items-baseline justify-between gap-2">
      <span class="hd text-green">Decision layer</span>
      <span class="text-xs text-muted-foreground">asks <b class="text-green">{decisions.decisions}</b></span>
    </div>
    {#if bars.length}
      <ol class="flex max-h-72 flex-col overflow-auto pr-1">
        {#each bars as b (b.seq)}
          <li
            class="grid grid-cols-[minmax(0,1fr)_64px_auto] items-center gap-x-2 py-1 text-xs"
            data-testid="run-decision-row"
            title={`${b.answer} · ${b.outcome} · ${b.source}${b.latency ? ` · ${b.latency}` : ''}`}
          >
            <span class="min-w-0">
              <span class="block truncate text-foreground">{b.use}</span>
              <span class="block truncate text-[11px] text-muted-foreground">{b.node ?? 'run'} · {b.outcome}</span>
            </span>
            <span
              class="hatch-bar"
              style:--k={b.verdict === 'sharp' ? 'var(--c-green)' : 'var(--c-orange)'}
              aria-hidden="true"><i style:width="{Math.round(b.strength * 100)}%"></i></span
            >
            <span class="min-w-0 text-right whitespace-nowrap">
              <span class="text-foreground">{b.value || '-'}</span>
              <span class={b.verdict === 'sharp' ? 'text-green' : 'text-orange'}>{b.verdict}</span>
            </span>
          </li>
        {/each}
      </ol>
    {/if}
    <div class="mt-2 flex flex-col gap-0.5 border-t border-line pt-2 text-[11px] text-muted-foreground">
      <p class="m-0">
        <span class="text-green">sharp -> applied</span> · <span class="text-orange">split -> advise only</span>
        {#if source}· mode {source.mode || 'unknown'} · {source.source}{/if}
      </p>
      <p class="m-0 text-fg-2" data-testid="run-decisions-summary">{decisionsSummary(decisions)}</p>
      {#each decisionUseLines(decisions) as u (u.use)}
        <p class="m-0"><span class="text-fg-2">{u.use}</span> · {u.text}</p>
      {/each}
    </div>
  </section>
{/if}
