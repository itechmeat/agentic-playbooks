<script lang="ts">
  import { addTermChecked, chosenName, termRefusalText, type AddTermRefusal, type SearchTerm } from './runsearch'

  // The run list's filter row (U45): pick one of the search fields the
  // listed runs offer, type a value, and the listing keeps the runs whose
  // field equals it (text without case). Applied terms join with AND.
  let {
    names,
    terms,
    footer = null,
    onChange,
  }: {
    names: string[]
    terms: SearchTerm[]
    footer?: string | null
    onChange: (terms: SearchTerm[]) => void
  } = $props()

  let name = $state('')
  let value = $state('')
  let refused = $state<AddTermRefusal | null>(null)
  // A stale or unknown name would submit a field no run offers, so the
  // select always shows a name the runs actually have.
  const chosen = $derived(chosenName(names, name))

  function apply(e: SubmitEvent) {
    e.preventDefault()
    const { next, refused: r } = addTermChecked(terms, { name: chosen, value })
    refused = r
    if (next !== terms) {
      value = ''
      onChange(next)
    }
  }
</script>

<div class="search" data-testid="run-search">
  {#if names.length}
    <form class="row" onsubmit={apply}>
      <label class="muted" for="run-search-field">search</label>
      <select id="run-search-field" bind:value={name} data-testid="run-search-field">
        {#each names as n (n)}<option value={n}>{n}</option>{/each}
      </select>
      <input
        type="text"
        placeholder="equals"
        aria-label="value"
        maxlength={200}
        bind:value
        data-testid="run-search-value"
      />
      <button type="submit" data-testid="run-search-apply">Filter</button>
    </form>
  {/if}
  {#if refused}
    <p class="muted" data-testid="run-search-refused">{termRefusalText(refused)}</p>
  {/if}
  {#if terms.length}
    <ul class="terms" data-testid="run-search-terms">
      {#each terms as t (t.name)}
        <li>
          <span>{t.name} = {t.value}</span>
          <button
            type="button"
            aria-label={`remove ${t.name}`}
            onclick={() => onChange(terms.filter((x) => x.name !== t.name))}>x</button
          >
        </li>
      {/each}
    </ul>
  {/if}
  {#if footer}<p class="muted" data-testid="run-search-footer">{footer}</p>{/if}
</div>

<style>
  .search {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    margin-bottom: 0.75rem;
    font-size: 0.75rem;
  }
  .row,
  .terms {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem;
  }
  .terms li {
    display: flex;
    align-items: center;
    gap: 0.25rem;
    padding: 0 0.5rem;
    border: 1px solid var(--c-line-2);
    min-width: 0;
  }
  .terms li span {
    overflow-wrap: anywhere;
  }
  select,
  input,
  button {
    padding: 0.125rem 0.5rem;
    border: 1px solid var(--c-line-2);
    background: transparent;
    color: inherit;
    font: inherit;
  }
  input {
    min-width: 0;
    flex: 1 1 10rem;
    max-width: 20rem;
  }
  button {
    cursor: pointer;
  }
  .muted {
    color: var(--muted-foreground);
  }
</style>
