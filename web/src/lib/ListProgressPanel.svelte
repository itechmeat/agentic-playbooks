<script lang="ts">
  import type { RunList } from './api.gen'
  import OutcomeSection from '$lib/components/OutcomeSection.svelte'
  import {
    ITEM_FILTERS,
    ITEM_ROW_CAP,
    STATE_LABEL,
    batchLabel,
    itemText,
    listChips,
    processedLabel,
    processedPercent,
    stateLabel,
    unlistedPending,
    visibleItems,
    type ItemFilter,
  } from './listprogress'

  // One card per list node of the run: how far its visit got, each state
  // counted, and its items in a table a person can filter. The counts are
  // the engine's; this panel only shows them.
  let { lists }: { lists?: RunList[] | null } = $props()

  const shown = $derived(lists ?? [])
  // Per list node: the state filter and whether the row cap is lifted.
  let filters = $state<Record<string, ItemFilter>>({})
  let expanded = $state<Record<string, boolean>>({})
</script>

{#each shown as l (l.node)}
  {@const filter = filters[l.node] ?? 'all'}
  {@const view = visibleItems(l.items, filter, expanded[l.node] ?? false)}
  {@const batch = batchLabel(l)}
  {@const unlisted = unlistedPending(l)}
  <OutcomeSection title={`List ${l.title ?? l.node}`} testid="run-list">
    <div class="summary">
      <span data-testid="run-list-processed">{processedLabel(l)}</span>
      {#if batch}<span class="muted" data-testid="run-list-batch">{batch}</span>{/if}
    </div>
    <div
      class="bar"
      role="progressbar"
      aria-label={`${l.node}: ${processedLabel(l)}`}
      aria-valuemin="0"
      aria-valuemax="100"
      aria-valuenow={processedPercent(l)}
    >
      <span class="fill" style:width={`${processedPercent(l)}%`}></span>
    </div>
    <ul class="chips">
      {#each listChips(l) as c (c.key)}
        <li class="chip {c.key}" class:zero={c.count === 0} data-testid="run-list-chip" data-key={c.key}>
          <strong>{c.count}</strong>
          {c.label}
        </li>
      {/each}
    </ul>
    {#if l.items.length}
      <label class="filter">
        <span class="muted">Show</span>
        <select
          value={filter}
          onchange={(e) => (filters[l.node] = e.currentTarget.value as ItemFilter)}
          data-testid="run-list-filter"
        >
          {#each ITEM_FILTERS as f (f)}<option value={f}>{STATE_LABEL[f]}</option>{/each}
        </select>
      </label>
      <div class="table-wrap">
        <table>
          <thead>
            <tr><th>Item</th><th>State</th><th>Round</th><th>Note</th></tr>
          </thead>
          <tbody>
            {#each view.rows as i (i.id)}
              <tr class={i.state} data-testid="run-list-item">
                <td><code>{i.id}</code></td>
                <td>{stateLabel(i.state)}</td>
                <td>{i.round}</td>
                <td class="note">{itemText(i)}</td>
              </tr>
            {:else}
              <tr><td colspan="4" class="muted">No item in this state.</td></tr>
            {/each}
          </tbody>
        </table>
      </div>
      {#if view.hidden > 0}
        <button type="button" class="more" onclick={() => (expanded[l.node] = true)} data-testid="run-list-show-all">
          Show all ({view.hidden} more beyond the first {ITEM_ROW_CAP})
        </button>
      {/if}
    {/if}
    {#if unlisted > 0 && (filter === 'all' || filter === 'pending')}
      <p class="muted" data-testid="run-list-unlisted">{unlisted} pending not started yet</p>
    {/if}
  </OutcomeSection>
{/each}

<style>
  .summary {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    justify-content: space-between;
    font-weight: 600;
  }
  .muted {
    color: var(--muted-foreground);
    font-weight: 400;
  }
  .bar {
    height: 0.375rem;
    border-radius: 999px;
    background: var(--muted);
    overflow: hidden;
  }
  .fill {
    display: block;
    height: 100%;
    background: var(--chart-1, var(--primary));
    transition: width 0.3s ease;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 0.375rem;
  }
  .chip {
    padding: 0 0.5rem;
    border: 1px solid var(--border);
    border-radius: 999px;
    line-height: 1.25rem;
  }
  .chip.passed {
    border-color: var(--success);
  }
  .chip.with_note {
    border-color: color-mix(in oklch, var(--success) 50%, var(--warning));
  }
  .chip.needs_look {
    border-color: var(--warning);
    background: color-mix(in oklch, var(--warning) 15%, transparent);
  }
  .chip.running {
    border-color: var(--chart-1, var(--primary));
  }
  .chip.zero {
    opacity: 0.55;
    background: none;
  }
  .filter {
    display: flex;
    align-items: center;
    gap: 0.375rem;
  }
  select {
    padding: 0.125rem 0.375rem;
    border: 1px solid var(--input, var(--border));
    border-radius: calc(var(--radius, 0.5rem) / 2);
    background: var(--background);
    color: var(--foreground);
    font: inherit;
  }
  .table-wrap {
    max-height: 24rem;
    overflow: auto;
  }
  table {
    width: 100%;
    border-collapse: collapse;
  }
  th {
    position: sticky;
    top: 0;
    background: var(--card);
    color: var(--muted-foreground);
    font-weight: 600;
    text-align: left;
  }
  th,
  td {
    padding: 0.125rem 0.375rem;
    border-bottom: 1px solid var(--border);
    vertical-align: top;
    /* The section breaks anywhere; a table cell would shrink to one letter. */
    overflow-wrap: break-word;
    white-space: nowrap;
  }
  td:first-child,
  .note {
    white-space: normal;
  }
  tr.needs_look td:first-child {
    box-shadow: inset 2px 0 0 var(--warning);
  }
  .note {
    width: 100%;
    color: var(--muted-foreground);
  }
  code {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .more {
    align-self: flex-start;
    padding: 0.125rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: calc(var(--radius, 0.5rem) / 2);
    background: var(--secondary);
    color: var(--secondary-foreground);
    font: inherit;
    cursor: pointer;
  }
</style>
