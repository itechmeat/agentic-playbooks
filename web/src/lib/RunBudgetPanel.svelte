<script lang="ts">
  // A run's budget (limits, spent and left per dimension, the attempts that
  // reported no usage, and the node a budget pause holds the run before),
  // with the estimate the run started with. Absent for a run without a
  // budget.
  import type { BudgetStatus, RunEstimate } from './api.gen'
  import * as Card from '$lib/components/ui/card'
  import { budgetRows, pausedText } from './runbudget'
  import { estimateRows } from './runestimate'

  let {
    budget = null,
    estimate = null,
  }: { budget?: BudgetStatus | null; estimate?: RunEstimate | null } = $props()

  const rows = $derived(budget ? budgetRows(budget) : [])
  const paused = $derived(budget ? pausedText(budget) : null)
  const expected = $derived(estimate ? estimateRows(estimate).filter((r) => r.label !== 'time') : [])
</script>

{#if budget}
  <Card.Root data-testid="run-budget">
    <Card.Header>
      <Card.Title>Budget</Card.Title>
    </Card.Header>
    <Card.Content>
      <div class="budget">
        {#each rows as r (r.dim)}
          <div class="row" class:over={r.over} data-testid="run-budget-row">
            <span class="dim">{r.dim === 'usd' ? 'dollars' : 'tokens'}</span>
            <span>{r.spent} of {r.limit} spent, {r.left} left</span>
            {#if r.unknown}<span class="muted">{r.unknown}</span>{/if}
          </div>
        {/each}
        {#if paused}<p class="paused" data-testid="run-budget-paused">{paused}</p>{/if}
        {#if budget.waiver}
          <p class="muted">{budget.waiver} may run once over the budget</p>
        {/if}
        <p class="muted">
          on no estimate: {budget.on_no_estimate}{budget.sets > 1 ? `, set ${budget.sets} times` : ''}
        </p>
        {#each expected as e (e.label)}
          <p class="muted">expected {e.label}: {e.value}</p>
        {/each}
      </div>
    </Card.Content>
  </Card.Root>
{/if}

<style>
  /* Plain CSS over the app's variables. */
  .budget {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 12px;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 8px;
    align-items: baseline;
  }
  .dim {
    min-width: 56px;
    font-weight: 600;
  }
  .over .dim,
  .paused {
    color: var(--destructive);
  }
  .muted {
    color: var(--muted-foreground);
  }
  p {
    margin: 0;
    overflow-wrap: anywhere;
  }
</style>
