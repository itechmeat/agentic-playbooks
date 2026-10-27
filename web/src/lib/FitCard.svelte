<script lang="ts">
  // "Will it fit here?": whether this playbook can run in this project, in
  // four sections that never hide each other. Compatibility is what the
  // project cannot change, setup what a person can provide, trust who
  // vouched for each part, consent what a start must be agreed to. Ready
  // and trusted are separate badges: an untrusted playbook can be ready.
  // While a fresh check runs, the last report stays visible but dimmed and
  // marked, so an old verdict is never read as the current one.
  import type { FitItem, FitReport } from './api.gen'
  import {
    bySeverity,
    compatibilitySummary,
    consentSummary,
    fitBadges,
    itemLine,
    setupSummary,
    trustLine,
    trustSummary,
  } from './fit'

  let {
    report = null,
    loading = false,
    error = null,
  }: { report?: FitReport | null; loading?: boolean; error?: string | null } = $props()

  const badges = $derived(report ? fitBadges(report) : [])
</script>

{#snippet items(g: { blocking: FitItem[]; notes: FitItem[] })}
  {#if g.blocking.length || g.notes.length}
    <ul>
      {#each g.blocking as i, n (n)}
        <li class="blocking" title={i.code}>{itemLine(i)}</li>
      {/each}
      {#each g.notes as i, n (n)}
        <li class="muted" title={i.code}>{itemLine(i)}</li>
      {/each}
    </ul>
  {/if}
{/snippet}

<div class="fit" data-testid="fit-card">
  {#if error}
    <p class="error" data-testid="fit-error">Could not check the fit: {error}</p>
  {:else if loading && !report}
    <p class="muted">Checking this project...</p>
  {:else if report}
    <div class="report" class:stale={loading}>
      <div class="badges">
        {#each badges as b (b.key)}
          <span class="badge {b.tone}" data-testid="fit-badge-{b.key}">{b.label}</span>
        {/each}
        {#if loading}<span class="muted" data-testid="fit-refreshing">checking again...</span>{/if}
        <span class="muted version">{report.scope} {report.version}</span>
      </div>

      <section data-testid="fit-compatibility">
        <h3>Compatibility</h3>
        <p class="summary">{compatibilitySummary(report)}</p>
        {@render items(bySeverity(report.compatibility))}
      </section>

      <section data-testid="fit-setup">
        <h3>Setup</h3>
        <p class="summary">{setupSummary(report)}</p>
        {@render items(bySeverity(report.setup))}
      </section>

      <section data-testid="fit-trust">
        <h3>Trust</h3>
        <p class="summary">{trustSummary(report)}</p>
        {#if report.trust.length}
          <ul>
            {#each report.trust as t, n (n)}
              <li class:muted={t.approved} title={t.digest ?? ''}>{trustLine(t)}</li>
            {/each}
          </ul>
        {/if}
      </section>

      <section data-testid="fit-consent">
        <h3>Consent</h3>
        <p class="summary">{consentSummary(report)}</p>
        {#if report.consent.effects.length}
          <p class="muted">Effects: {report.consent.effects.join(', ')}</p>
        {/if}
      </section>
    </div>
  {/if}
</div>

<style>
  /* Plain CSS over the app's variables. */
  .fit {
    display: flex;
    flex-direction: column;
    gap: 14px;
    font-size: 13px;
  }
  .badges {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .badge {
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 1px 8px;
    font-size: 12px;
    font-weight: 500;
  }
  .badge.ok {
    color: var(--success);
    border-color: color-mix(in oklab, var(--success) 40%, transparent);
    background: color-mix(in oklab, var(--success) 12%, transparent);
  }
  .badge.warn {
    color: var(--warning);
    border-color: color-mix(in oklab, var(--warning) 40%, transparent);
    background: color-mix(in oklab, var(--warning) 12%, transparent);
  }
  .badge.danger {
    color: var(--destructive);
    border-color: color-mix(in oklab, var(--destructive) 40%, transparent);
    background: color-mix(in oklab, var(--destructive) 12%, transparent);
  }
  .report {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .stale {
    opacity: 0.6;
  }
  .version {
    margin-left: auto;
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 12px;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  h3 {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
  }
  p {
    margin: 0;
  }
  ul {
    margin: 0;
    padding-left: 16px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    overflow-wrap: anywhere;
  }
  .blocking {
    color: var(--destructive);
  }
  .muted {
    color: var(--muted-foreground);
  }
  .error {
    color: var(--destructive);
  }
</style>
