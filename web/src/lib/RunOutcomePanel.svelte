<script lang="ts">
  import type { NodeCommits, RunGoal } from './api.gen'
  import OutcomeSection from '$lib/components/OutcomeSection.svelte'
  import OutcomeTag from '$lib/components/OutcomeTag.svelte'
  import { commitLines, goalSummary, goalTone, omittedCommits } from './runoutcome'

  // What the run produced beyond its outputs (0.23.0): the playbook's goal
  // with each criterion's result, manual ones as a checklist for a person,
  // and the commits its nodes made on a git tree. Each block renders only
  // when there is one, so the page of a run without them is unchanged. The
  // inbox gaps are part of the verdict panel's Not verified list.
  let { goal, commits }: { goal?: RunGoal | null; commits?: NodeCommits[] | null } = $props()

  const lines = $derived(commitLines(commits ?? undefined))
  const omitted = $derived(omittedCommits(commits ?? undefined))
  const checked = $derived(goal ? goal.criteria.filter((c) => c.check !== 'manual') : [])
  const manual = $derived(goal ? goal.criteria.filter((c) => c.check === 'manual') : [])
</script>

{#if goal}
  <OutcomeSection title="Goal" testid="run-goal">
    <p>{goal.statement}</p>
    <p class="mono muted" data-testid="run-goal-summary">{goalSummary(goal)}</p>
    {#if checked.length}
      <ul class="list">
        {#each checked as c (c.index)}
          <li class="criterion" data-testid="run-goal-criterion">
            <span class="row">
              <OutcomeTag tone={goalTone(c.status)}>{c.status}</OutcomeTag>
              <span>{c.description} <span class="muted">· {c.check}</span></span>
            </span>
            {#if c.detail}<span class="muted">{c.detail}</span>{/if}
          </li>
        {/each}
      </ul>
    {/if}
    {#if manual.length}
      <div class="checklist" data-testid="run-goal-checklist">
        <span class="muted">To confirm by hand</span>
        <ul class="list">
          {#each manual as c (c.index)}
            <li class="row">
              <span class="box" aria-hidden="true"></span>
              <span>{c.description}</span>
            </li>
          {/each}
        </ul>
      </div>
    {/if}
  </OutcomeSection>
{/if}

{#if lines.length}
  <OutcomeSection title="Commits" testid="run-commits">
    <div class="list">
      {#each lines as c, i (i)}
        <div data-testid="run-commit-row">
          <span class="mono" title={c.sha}>{c.short}</span>
          <span> {c.subject}</span>
          <span class="muted"> · {c.node}</span>
        </div>
      {/each}
      {#if omitted > 0}
        <p class="muted">and {omitted} more</p>
      {/if}
    </div>
  </OutcomeSection>
{/if}

<style>
  .list {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .criterion {
    display: flex;
    flex-direction: column;
  }
  .row {
    display: flex;
    align-items: flex-start;
    gap: 0.375rem;
  }
  .checklist {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding-top: 0.5rem;
    border-top: 1px solid var(--border);
  }
  .box {
    flex-shrink: 0;
    width: 0.75rem;
    height: 0.75rem;
    margin-top: 0.125rem;
    border: 1px solid var(--border);
    border-radius: 0.125rem;
  }
  .muted {
    color: var(--muted-foreground);
  }
  .mono {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
</style>
