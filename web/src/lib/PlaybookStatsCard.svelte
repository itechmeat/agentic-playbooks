<script lang="ts">
  import type { StatsReport } from './api.gen'
  import { Spinner } from '$lib/components/ui/spinner'
  import {
    costKnownText,
    executorGroups,
    modelsText,
    perRunText,
    qualityDeltaText,
    qualityDeltas,
    qualityKey,
    qualityLabels,
    qualityText,
    rateText,
    spendText,
    versionsNewestFirst,
    waitsText,
  } from './playbookstats'

  // Cross-run metrics of one playbook (C3), per version, newest first, from
  // `GET /api/stats`. The page loads the report; this card only shows it and
  // its loading, error and empty states.
  let {
    report = null,
    loading = false,
    error = null,
  }: { report?: StatsReport | null; loading?: boolean; error?: string | null } = $props()

  const versions = $derived(versionsNewestFirst(report))
  const deltas = $derived(qualityDeltas(report?.compare))
  const deltaLabels = $derived(qualityLabels(deltas))
</script>

<div class="stats" data-testid="playbook-stats">
  {#if error}
    <p class="error" data-testid="playbook-stats-error">Could not load the stats: {error}</p>
  {:else if loading && !report}
    <p class="muted loading" data-testid="playbook-stats-loading">
      <Spinner /> loading
    </p>
  {:else if !report || report.runs === 0}
    <p class="muted" data-testid="playbook-stats-empty">No runs of this playbook yet.</p>
  {:else}
    {#if deltas.length && report.compare?.against}
      <section class="version" data-testid="playbook-stats-quality-deltas">
        <div class="version-head">
          <span class="muted">quality, {report.compare.against} against {report.compare.base}</span>
        </div>
        <dl>
          {#each deltas as d, i (qualityKey(d))}
            <dt>{deltaLabels[i]}</dt>
            <dd>{qualityDeltaText(d)}</dd>
          {/each}
        </dl>
      </section>
    {/if}
    {#each versions as v (v.version)}
      {@const executors = executorGroups(v)}
      <section class="version" data-testid="playbook-stats-version">
        <div class="version-head">
          <span class="version-name">{v.version}</span>
          <span class="runs">{v.runs} {v.runs === 1 ? 'run' : 'runs'}</span>
        </div>
        {#if v.note}<p class="muted">{v.note}</p>{/if}
        <dl>
          <dt>succeeded</dt>
          <dd>{rateText(v.success)}</dd>
          <dt>first pass</dt>
          <dd>{rateText(v.first_pass)}</dd>
          <dt>retries</dt>
          <dd>{perRunText(v.retries)}</dd>
          <dt>fallbacks</dt>
          <dd>{perRunText(v.fallbacks)}</dd>
          <dt>loops</dt>
          <dd>{perRunText(v.loop_traversals)}</dd>
          <dt>gate wait</dt>
          <dd>{waitsText(v.gate_wait)}</dd>
          <dt>run time</dt>
          <dd>{waitsText(v.duration)}</dd>
          {#if spendText(v)}
            <dt>spend</dt>
            <dd>{spendText(v)}</dd>
          {/if}
          {#if costKnownText(v)}
            <dt>cost known</dt>
            <dd data-testid="playbook-stats-cost-known">{costKnownText(v)}</dd>
          {/if}
          {#if modelsText(v)}
            <dt>models</dt>
            <dd data-testid="playbook-stats-models">{modelsText(v)}</dd>
          {/if}
        </dl>
        {#if executors.length}
          <ul class="executors" data-testid="playbook-stats-executors">
            {#each executors as n (n.node)}
              <li>
                <span class="muted">{n.node}, per executor:</span>
                <!-- Two executors may print alike (shortened digests): key by position. -->
                {#each n.groups as g, i (i)}<div class="group">{g}</div>{/each}
              </li>
            {/each}
          </ul>
        {/if}
        {#if v.quality?.length}
          {@const labels = qualityLabels(v.quality)}
          <ul class="quality" data-testid="playbook-stats-quality">
            {#each v.quality as q, i (qualityKey(q))}
              <li>
                <span class="muted">quality {labels[i]}:</span>
                {qualityText(q)}
              </li>
            {/each}
          </ul>
        {/if}
        {#if v.goal.length}
          <ul class="goal" data-testid="playbook-stats-goal">
            {#each v.goal as g (g.index)}
              <li>
                <span class="muted">goal {g.index + 1}:</span>
                {g.description}
                <span class="muted">
                  · {g.check === 'manual' ? `manual in ${g.manual}` : `passed ${rateText(g.passed)}`}
                </span>
              </li>
            {/each}
          </ul>
        {/if}
      </section>
    {/each}
  {/if}
</div>

<style>
  .stats {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
    font-size: 0.75rem;
  }
  .version {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding-bottom: 0.75rem;
    border-bottom: 1px solid var(--border);
  }
  .version-head,
  .loading {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .version-name {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 0.875rem;
    font-weight: 600;
  }
  .runs {
    padding: 0 0.5rem;
    border: 1px solid var(--border);
    border-radius: 999px;
    line-height: 1.25rem;
  }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 0.125rem 0.75rem;
  }
  dt,
  .muted {
    color: var(--muted-foreground);
  }
  .error {
    color: var(--destructive);
  }
  .executors {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    margin-top: 0.25rem;
    overflow-wrap: anywhere;
  }
  .group {
    padding-left: 0.75rem;
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .goal,
  .quality {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    margin-top: 0.25rem;
    overflow-wrap: anywhere;
  }
</style>
