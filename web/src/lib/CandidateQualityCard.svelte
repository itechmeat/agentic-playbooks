<script lang="ts">
  import type { CandidateQualityView } from './api.gen'
  import OutcomeSection from '$lib/components/OutcomeSection.svelte'
  import OutcomeTag from '$lib/components/OutcomeTag.svelte'
  import { Button } from '$lib/components/ui/button'
  import {
    comparisonLabel,
    resultTone,
    tallyLeftOut,
    tallyText,
    verdictText,
    verdictTone,
  } from './quality'

  // A playbook's candidate version (a forward patch on trial) against
  // current: under a quality policy the comparison per criterion and the
  // verdict as it stands, the decision recorded last, and a person's
  // decision: Promote makes the candidate current, Keep current drops it
  // and changes nothing.
  let {
    candidate,
    frozen = false,
    busy = false,
    onPromote,
    onKeepCurrent,
  }: {
    candidate: CandidateQualityView
    frozen?: boolean
    busy?: boolean
    onPromote: () => void
    onKeepCurrent: (reason: string) => void
  } = $props()

  let reason = $state('')
  const decision = $derived(candidate.decision ?? null)
  const recorded = $derived(candidate.recorded ?? null)
</script>

<OutcomeSection title="Candidate" testid="candidate-card">
  <p data-testid="candidate-line">
    <span class="mono">{candidate.version}</span> on trial against
    <span class="mono">{candidate.current}</span>, {candidate.trials} {candidate.trials === 1 ? 'trial run' : 'trial runs'}
  </p>

  {#if candidate.gate}
    <p class="muted" data-testid="candidate-gate">
      Promoted by quality: {candidate.gate.compare === 'better' ? 'better' : 'not worse'} than current,
      {candidate.gate.min_runs} graded runs per side, at most {candidate.gate.max_trials} trials, a poor rate at
      most {Math.round(candidate.gate.margin * 100)} points above current's
    </p>
  {:else}
    <p class="muted" data-testid="candidate-gate">Promoted by successful trials, not by quality</p>
  {/if}

  {#if decision}
    <div class="row" data-testid="candidate-verdict">
      <OutcomeTag tone={verdictTone(decision.verdict)}>{verdictText(decision.verdict)}</OutcomeTag>
      <span>{decision.reason}</span>
    </div>
    {#if decision.per_criterion.length}
      <ul class="comparison" data-testid="candidate-table">
        {#each decision.per_criterion as c, i (i)}
          <li data-testid="candidate-row" data-result={c.result} title={comparisonLabel(c)}>
            <div class="row">
              <OutcomeTag tone={resultTone(c.result)}>{c.result.replace(/_/g, ' ')}</OutcomeTag>
              <span class="mono">{c.key.criterion}</span>
              <span class="muted">{c.key.grader}</span>
            </div>
            <div class="tallies">
              <span title={tallyLeftOut(c.candidate) ?? undefined}
                >candidate {tallyText(c.candidate, 'no graded trials yet')}</span
              >
              <span title={tallyLeftOut(c.current) ?? undefined}>current {tallyText(c.current)}</span>
            </div>
          </li>
        {/each}
      </ul>
      <p class="muted">Counts are good/partial/poor graded runs; hover a count for what was left out.</p>
    {/if}
  {/if}

  {#if recorded}
    <p class="muted" data-testid="candidate-recorded">
      Recorded: {verdictText(recorded.verdict)} by {recorded.decided_by}, {recorded.reason}
    </p>
  {/if}

  <div class="decide">
    <label class="reason">
      <span class="muted">Reason to keep current (optional)</span>
      <input
        type="text"
        maxlength="300"
        bind:value={reason}
        disabled={busy}
        data-testid="candidate-reason"
      />
    </label>
    <div class="buttons">
      <Button
        variant="outline"
        size="sm"
        disabled={busy}
        onclick={() => onKeepCurrent(reason)}
        data-testid="candidate-keep-current"
        title="Keep the current version and drop the candidate: nothing changes"
      >
        Keep current
      </Button>
      <Button
        size="sm"
        disabled={busy || frozen}
        onclick={onPromote}
        data-testid="candidate-promote"
        title={frozen
          ? 'Frozen playbook: unfreeze it to change the current version'
          : 'Make the candidate the current version'}
      >
        Promote
      </Button>
    </div>
  </div>
</OutcomeSection>

<style>
  p {
    margin: 0;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.375rem;
  }
  .comparison {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .comparison li {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    padding-bottom: 0.375rem;
    border-bottom: 1px solid var(--border);
  }
  .tallies {
    display: flex;
    flex-wrap: wrap;
    gap: 0.75rem;
    font-variant-numeric: tabular-nums;
  }
  .decide {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding-top: 0.5rem;
    border-top: 1px solid var(--border);
  }
  .reason {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .reason input {
    height: 2rem;
    padding: 0 0.5rem;
    border: 1px solid var(--input, var(--border));
    border-radius: var(--radius-md, 0.375rem);
    background: transparent;
    color: var(--foreground);
    font: inherit;
  }
  .reason input:focus-visible {
    outline: 2px solid var(--ring);
    outline-offset: 1px;
  }
  .buttons {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: 0.5rem;
  }
  .muted {
    color: var(--muted-foreground);
  }
  .mono {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
</style>
