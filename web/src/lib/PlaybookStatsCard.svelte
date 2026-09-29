<script lang="ts">
  import type { StatsReport } from './api.gen'
  import { Badge } from '$lib/components/ui/badge'
  import { Spinner } from '$lib/components/ui/spinner'
  import { perRunText, rateText, spendText, versionsNewestFirst, waitsText } from './playbookstats'

  // Cross-run metrics of one playbook (C3), per version, newest first, from
  // `GET /api/stats`. The page loads the report; this card only shows it and
  // its loading, error and empty states.
  let {
    report = null,
    loading = false,
    error = null,
  }: { report?: StatsReport | null; loading?: boolean; error?: string | null } = $props()

  const versions = $derived(versionsNewestFirst(report))
</script>

<div class="flex flex-col gap-3 text-xs" data-testid="playbook-stats">
  {#if error}
    <p class="text-destructive" data-testid="playbook-stats-error">Could not load the stats: {error}</p>
  {:else if loading && !report}
    <p class="flex items-center gap-2 text-muted-foreground" data-testid="playbook-stats-loading">
      <Spinner /> loading
    </p>
  {:else if !report || report.runs === 0}
    <p class="text-muted-foreground" data-testid="playbook-stats-empty">No runs of this playbook yet.</p>
  {:else}
    {#each versions as v (v.version)}
      <section class="flex flex-col gap-1 border-b border-border pb-3" data-testid="playbook-stats-version">
        <div class="flex items-center gap-2">
          <span class="font-mono text-sm font-semibold">{v.version}</span>
          <Badge variant="outline">{v.runs} {v.runs === 1 ? 'run' : 'runs'}</Badge>
        </div>
        {#if v.note}<p class="text-muted-foreground">{v.note}</p>{/if}
        <dl class="grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5">
          <dt class="text-muted-foreground">succeeded</dt>
          <dd>{rateText(v.success)}</dd>
          <dt class="text-muted-foreground">first pass</dt>
          <dd>{rateText(v.first_pass)}</dd>
          <dt class="text-muted-foreground">retries</dt>
          <dd>{perRunText(v.retries)}</dd>
          <dt class="text-muted-foreground">fallbacks</dt>
          <dd>{perRunText(v.fallbacks)}</dd>
          <dt class="text-muted-foreground">loops</dt>
          <dd>{perRunText(v.loop_traversals)}</dd>
          <dt class="text-muted-foreground">gate wait</dt>
          <dd>{waitsText(v.gate_wait)}</dd>
          <dt class="text-muted-foreground">run time</dt>
          <dd>{waitsText(v.duration)}</dd>
          {#if spendText(v)}
            <dt class="text-muted-foreground">spend</dt>
            <dd>{spendText(v)}</dd>
          {/if}
        </dl>
        {#if v.goal.length}
          <ul class="mt-1 flex flex-col gap-0.5" data-testid="playbook-stats-goal">
            {#each v.goal as g (g.index)}
              <li class="break-words">
                <span class="text-muted-foreground">goal {g.index + 1}:</span>
                {g.description}
                <span class="text-muted-foreground">
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
