<script lang="ts">
  import type { InterventionStats } from './api.gen'
  import {
    categoryLabel,
    categoryText,
    interventionsHeadText,
    interventionsPartlyKnown,
    kindsText,
    knownCountText,
    withoutAnyText,
  } from './playbookstats'

  // Interventions over a version's finished runs (U24), from
  // `VersionStats.interventions`. Counts sit beside their denominators; a
  // figure an older apb did not record says where it is known.
  let { stats }: { stats: InterventionStats } = $props()

  const engine = $derived(kindsText(stats.engine))
  const unknownActor = $derived(kindsText(stats.actor_unknown))
  const other = $derived(kindsText(stats.other))
</script>

<section class="interventions" data-testid="interventions-card">
  <div class="head">
    <span class="label">interventions</span>
    <span data-testid="interventions-total">{interventionsHeadText(stats)}</span>
  </div>
  <dl>
    <dt>runs without any</dt>
    <dd data-testid="interventions-without-any">{withoutAnyText(stats)}</dd>
    {#each stats.categories as c (c.category)}
      <dt>{categoryLabel(c.category)}</dt>
      <dd data-testid="interventions-category">{categoryText(c)}</dd>
    {/each}
    <dt>automatic total</dt>
    <dd>{knownCountText(stats.automatic_total)}</dd>
    {#if engine}
      <dt>engine actions</dt>
      <dd data-testid="interventions-engine">{engine} (not interventions)</dd>
    {/if}
    {#if unknownActor}
      <dt>actor unknown</dt>
      <dd>{unknownActor} (outside the totals)</dd>
    {/if}
    {#if other}
      <dt>actions this apb does not know</dt>
      <dd>{other} (outside the totals)</dd>
    {/if}
  </dl>
  {#if interventionsPartlyKnown(stats)}
    <p class="note" data-testid="interventions-note">
      Some figures are not recorded in every run (written by an older apb, or an action that names no actor):
      those runs are left out of them, never counted as zero.
    </p>
  {/if}
  <p class="note">
    Corrections are counted through the supervisor channel, which does not tell a supervising agent from a person.
  </p>
</section>

<style>
  .interventions {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    margin-top: 0.25rem;
  }
  .head {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
  }
  .label,
  dt,
  .note {
    color: var(--muted-foreground);
  }
  dl {
    display: grid;
    /* max-content keeps labels on one line; the value column keeps a floor,
       so a wrapping value never collapses to one character per line. */
    grid-template-columns: minmax(0, max-content) minmax(10rem, 1fr);
    gap: 0.125rem 0.75rem;
  }
  dd {
    overflow-wrap: anywhere;
  }
</style>
