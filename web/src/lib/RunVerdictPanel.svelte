<script lang="ts">
  import type { OutcomeSummary } from './api.gen'
  import OutcomeSection from '$lib/components/OutcomeSection.svelte'
  import OutcomeTag from '$lib/components/OutcomeTag.svelte'
  import {
    evidenceSource,
    evidenceTone,
    gapIsWarning,
    gapLabel,
    goalReachedText,
    needsLookCounts,
    reportedBy,
    resultText,
  } from './runverdict'

  // The run's human outcome: what it did, what the engine checked, what
  // nobody checked, what the attempts reported for a person, and the
  // baseline as problem, change and goal. Verified and not verified never
  // mix: an agent's own word is listed under Not verified.
  let { outcome }: { outcome: OutcomeSummary } = $props()

  const look = $derived(needsLookCounts(outcome.needs_look))
</script>

<OutcomeSection title="Outcome" testid="run-verdict">
  <p class="headline" data-testid="run-verdict-headline">{outcome.headline}</p>
  {#if look}
    <p class="needs-look" role="status" data-testid="run-verdict-needs-look">Needs a look: {look}</p>
  {/if}

  {#if outcome.done.length}
    <div class="block" data-testid="run-verdict-done">
      <h4>Done</h4>
      <ul>
        {#each outcome.done as line, i (i)}<li>{line}</li>{/each}
      </ul>
    </div>
  {/if}

  {#if outcome.verified.length}
    <div class="block" data-testid="run-verdict-verified">
      <h4>Verified</h4>
      <ul>
        {#each outcome.verified as e, i (i)}
          <li class="row" data-testid="run-verdict-evidence">
            <OutcomeTag tone={evidenceTone(e)}>{resultText(e.result)}</OutcomeTag>
            <span>
              <span class="muted">{evidenceSource(e)}:</span>
              {e.subject}
              {#if e.detail}<span class="muted"> · {e.detail}</span>{/if}
            </span>
          </li>
        {/each}
      </ul>
    </div>
  {/if}

  {#if outcome.unverified.length}
    <div class="block" data-testid="run-verdict-unverified">
      <h4>Not verified</h4>
      <ul>
        {#each outcome.unverified as g, i (i)}
          <li class="row" class:warn={gapIsWarning(g)} data-testid="run-verdict-gap" data-kind={g.kind}>
            <OutcomeTag tone="gap">{gapLabel(g)}</OutcomeTag>
            <span>
              {g.subject}
              {#if g.detail}<span class="muted"> · {g.detail}</span>{/if}
            </span>
          </li>
        {/each}
      </ul>
    </div>
  {/if}

  {#if outcome.findings.length}
    <div class="block" data-testid="run-verdict-findings">
      <h4>Findings</h4>
      <ul>
        {#each outcome.findings as f, i (i)}
          <li class="note" data-testid="run-verdict-finding">
            <span class="row">
              <OutcomeTag tone="fail">{f.kind}</OutcomeTag>
              <code>{f.location}</code>
            </span>
            <span>{f.evidence}</span>
            <span class="muted">not fixed: {f.why_not_fixed} · {reportedBy(f)}</span>
          </li>
        {/each}
      </ul>
    </div>
  {/if}

  {#if outcome.warnings.length}
    <div class="block" data-testid="run-verdict-warnings">
      <h4>Warnings</h4>
      <ul>
        {#each outcome.warnings as w, i (i)}
          <li class="note" data-testid="run-verdict-warning">
            <span><strong>{w.subject}</strong>: {w.reason}</span>
            <span class="muted">{reportedBy(w)}</span>
          </li>
        {/each}
      </ul>
    </div>
  {/if}

  {#if outcome.baseline}
    <div class="block" data-testid="run-verdict-baseline">
      <h4>Baseline</h4>
      <ol class="story">
        <li>
          <span class="step">Problem</span>
          {#each outcome.baseline.problem as p, i (i)}<span>{p}</span>{:else}<span class="muted">no before snapshot</span>{/each}
        </li>
        <li>
          <span class="step">Change</span>
          {#each outcome.baseline.change as c, i (i)}<span>{c}</span>{:else}<span class="muted">no comparison yet</span>{/each}
        </li>
        <li data-testid="run-verdict-goal-reached">
          <span class="step">Goal reached</span>
          <span>{goalReachedText(outcome.baseline.goal_reached)}</span>
          <span class="muted">from the goal criteria, never from the baseline</span>
        </li>
      </ol>
    </div>
  {/if}

  {#if outcome.agent_summary}
    <details class="block" data-testid="run-verdict-agent-summary">
      <summary>Agent's own summary, not checked</summary>
      <pre>{outcome.agent_summary}</pre>
    </details>
  {/if}
</OutcomeSection>

<style>
  .headline {
    font-weight: 600;
  }
  .needs-look {
    padding: 0.25rem 0.5rem;
    border: 1px solid var(--warning);
    border-radius: var(--radius, 0.5rem);
    background: color-mix(in oklch, var(--warning) 12%, transparent);
  }
  .block {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  h4,
  summary {
    color: var(--muted-foreground);
    font-weight: 600;
  }
  summary {
    cursor: pointer;
  }
  ul,
  .story {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .row {
    display: flex;
    align-items: flex-start;
    gap: 0.375rem;
  }
  .row.warn {
    padding: 0.125rem 0.25rem;
    border-left: 2px solid var(--warning);
  }
  .note,
  .story li {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
  }
  .story li {
    padding-left: 0.5rem;
    border-left: 2px solid var(--border);
  }
  .step {
    font-weight: 600;
  }
  .muted {
    color: var(--muted-foreground);
  }
  code,
  pre {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  pre {
    margin-top: 0.25rem;
    white-space: pre-wrap;
    color: var(--muted-foreground);
  }
</style>
