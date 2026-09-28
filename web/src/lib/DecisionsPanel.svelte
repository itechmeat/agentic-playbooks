<script lang="ts">
  import type { RunDecisions } from './api.gen'
  import type { WfEvent } from './types'
  import * as Card from '$lib/components/ui/card'
  import { decisionRows, decisionsSummary, decisionUseLines } from './rundecisions'

  // The run's decision-model block (issue #165 Part 4): totals, one line per
  // use, one row per decision. Renders nothing when the run journaled no
  // decision, so an unconfigured run's page is unchanged.
  let { decisions, events }: { decisions?: RunDecisions | null; events: WfEvent[] } = $props()

  const rows = $derived(decisions ? decisionRows(events) : [])
</script>

{#if decisions}
  <Card.Root data-testid="run-decisions">
    <Card.Header><Card.Title class="text-sm">Decisions</Card.Title></Card.Header>
    <Card.Content class="flex flex-col gap-2 text-xs">
      <p class="font-mono" data-testid="run-decisions-summary">{decisionsSummary(decisions)}</p>
      {#each decisionUseLines(decisions) as u (u.use)}
        <div>
          <span class="font-mono font-semibold">{u.use}</span>
          <span class="text-muted-foreground"> · {u.text}</span>
        </div>
      {/each}
      {#if rows.length}
        <ol class="flex flex-col gap-1.5 border-t border-border pt-2">
          {#each rows as r (r.seq)}
            <li class="flex flex-col" data-testid="run-decision-row">
              <span>
                <span class="font-mono">{r.use}</span>
                {#if r.node}<span class="text-muted-foreground"> {r.node}</span>{/if}
                <span class="text-muted-foreground"> · {r.outcome}</span>
              </span>
              <span class="break-words">{r.answer}</span>
              <span class="text-muted-foreground">{r.source}{r.latency ? ` · ${r.latency}` : ''}</span>
            </li>
          {/each}
        </ol>
      {/if}
    </Card.Content>
  </Card.Root>
{/if}
