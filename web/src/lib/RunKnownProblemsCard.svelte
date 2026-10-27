<script lang="ts">
  import type { RunKnownProblems } from './api.gen'
  import OutcomeSection from '$lib/components/OutcomeSection.svelte'
  import { hiddenReasonText, shownBasisText, shownNodesText, unsignedText } from './knownproblems'

  // What a run showed its agents of its playbook's known problems, what it
  // hid and why, and how many problems of a file this installation did not
  // write it left out (only counted, never named).
  let {
    problems,
    workspace,
    playbook,
  }: {
    problems: RunKnownProblems
    workspace: string
    playbook: string
  } = $props()
</script>

<OutcomeSection title="Known problems" testid="run-known-problems">
  {#if problems.shown.length}
    <p class="kp-lead">Shown to this run's agents, as facts to check:</p>
    <ul class="kp-list">
      {#each problems.shown as p (p.id)}
        <li data-testid="run-known-problem-shown">
          <span class="kp-scope">{p.scope}</span>
          <span>{p.symptom}</span>
          {#if p.workaround}
            <span class="kp-muted">Workaround reported: {p.workaround}</span>
          {/if}
          <span class="kp-muted">{shownNodesText(p.nodes)}, {shownBasisText(p.basis)}</span>
        </li>
      {/each}
    </ul>
  {/if}
  {#if problems.hidden.length}
    <p class="kp-lead">Not shown:</p>
    <ul class="kp-list">
      {#each problems.hidden as h (h.id)}
        <li data-testid="run-known-problem-hidden">
          <span class="kp-scope">{h.id}</span>
          <span class="kp-muted">{hiddenReasonText(h.reason)}</span>
        </li>
      {/each}
    </ul>
  {/if}
  {#if problems.unsigned > 0}
    <p class="kp-muted" data-testid="run-known-problems-unsigned">{unsignedText(problems.unsigned)}</p>
  {/if}
  <a class="kp-link" href={`#/playbook/${encodeURIComponent(workspace)}/${encodeURIComponent(playbook)}`}
    >Open the playbook's known problems</a
  >
</OutcomeSection>

<style>
  /* Plain CSS over the theme variables. */
  .kp-lead,
  p.kp-muted {
    margin: 0;
  }
  .kp-list {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .kp-list li {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    padding-left: 0.5rem;
    border-left: 2px solid var(--border);
  }
  .kp-scope {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .kp-muted {
    color: var(--muted-foreground);
  }
  .kp-link {
    align-self: flex-start;
    color: var(--primary);
    text-decoration: underline;
    text-underline-offset: 2px;
  }
</style>
