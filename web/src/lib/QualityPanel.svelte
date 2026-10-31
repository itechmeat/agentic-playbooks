<script lang="ts">
  import type { Grade } from './api.gen'
  import type { GraderChoice } from './api'
  import type { WfEvent } from './types'
  import OutcomeSection from '$lib/components/OutcomeSection.svelte'
  import OutcomeTag from '$lib/components/OutcomeTag.svelte'
  import { Button } from '$lib/components/ui/button'
  import {
    citeTarget,
    graderLabel,
    graderName,
    hasHostGrade,
    latestGrades,
    levelText,
    levelTone,
    readingLine,
  } from './quality'

  // The run's quality: the latest grade of each grader, criterion by
  // criterion, with the records each level cites. A cite that stands for a
  // journal event opens it in the events list (onCite); a transcript or a
  // status file also names the file under the run directory. Once the run
  // ended it is graded on request: by rules (free), or by the project's
  // decision model on the playbook's own criteria (only when the project
  // turns that use on; otherwise nothing is stored and the reason is said).
  // A run takes one host grade: Grade again lets a host grade it once more
  // (onReopen).
  let {
    grades,
    events,
    ended,
    grading = false,
    onGrade,
    onReopen,
    onCite,
  }: {
    grades: Grade[]
    events: WfEvent[]
    ended: boolean
    grading?: boolean
    onGrade: (grader: GraderChoice) => void
    onReopen: () => void
    onCite: (seq: number) => void
  } = $props()

  const shown = $derived(latestGrades(grades))

  function when(ms: number): string {
    return new Date(ms).toISOString().slice(0, 16).replace('T', ' ')
  }
</script>

<OutcomeSection title="Quality" testid="run-quality">
  {#if shown.length === 0}
    <p class="muted" data-testid="run-quality-empty">
      {ended ? 'Not graded yet.' : 'Graded once the run ends.'}
    </p>
  {/if}

  {#each shown as g (g.grade_id)}
    <div class="grade" data-testid="run-quality-grade" data-grader={graderName(g)}>
      <div class="grade-head">
        <span class="mono" title={graderName(g)}>{graderLabel(g)}</span>
        <span class="muted">{when(g.created_at)} UTC</span>
      </div>
      <ul class="criteria">
        {#each g.criteria as c (c.id)}
          <li class="criterion" data-testid="run-quality-criterion" data-level={c.level}>
            <div class="row">
              <OutcomeTag tone={levelTone(c.level)}>{levelText(c.level)}</OutcomeTag>
              <span class="mono">{c.id}</span>
              {#if !c.counts_for_promotion}
                <span class="muted" title="This level does not count when a candidate is compared">not compared</span>
              {/if}
            </div>
            <p class="why">
              {c.why}
              <span class="muted">({c.reason_code})</span>
            </p>
            {#if c.cites?.length}
              <div class="cites">
                {#each c.cites as cite, i (i)}
                  {@const t = citeTarget(cite, events)}
                  {#if t.seq !== null}
                    <button
                      type="button"
                      class="cite"
                      title={t.path ? `runs/${g.run_id}/${t.path}` : `journal event ${t.seq}`}
                      data-testid="run-quality-cite"
                      onclick={() => onCite(t.seq as number)}>{t.label}</button
                    >
                  {:else}
                    <span class="cite static" title={t.path ? `runs/${g.run_id}/${t.path}` : undefined}>{t.label}</span>
                  {/if}
                {/each}
              </div>
            {/if}
          </li>
        {/each}
      </ul>
      <p class="muted" data-testid="run-quality-reading">{readingLine(g.reading)}</p>
    </div>
  {/each}

  {#if ended}
    <div class="actions">
      {#if hasHostGrade(grades)}
        <Button
          variant="outline"
          size="sm"
          onclick={onReopen}
          disabled={grading}
          data-testid="run-quality-reopen"
          title="A run takes one host grade: let a host grade this run once more"
        >
          Grade again
        </Button>
      {/if}
      <Button
        variant="outline"
        size="sm"
        onclick={() => onGrade('model')}
        disabled={grading}
        data-testid="run-quality-grade-model"
        title="Grade the playbook's own criteria by the project's decision model; only when decisions.yaml turns its grade use on and lets run outputs be sent"
      >
        Grade by model
      </Button>
      <Button
        variant="outline"
        size="sm"
        onclick={() => onGrade('rules')}
        disabled={grading}
        data-testid="run-quality-grade-rules"
        title="Grade the built-in criteria (outcome, loops, scope, consent, skill use) from the journal and transcripts; no model is called"
      >
        {grading ? 'Grading...' : 'Grade by rules'}
      </Button>
    </div>
  {/if}
</OutcomeSection>

<style>
  .grade {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    padding-top: 0.5rem;
    border-top: 1px solid var(--border);
  }
  .grade:first-of-type {
    padding-top: 0;
    border-top: 0;
  }
  .grade-head {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    justify-content: space-between;
    gap: 0.5rem;
    font-weight: 600;
  }
  .criteria {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .criterion {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.375rem;
  }
  .why {
    margin: 0;
  }
  .cites {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem;
  }
  .cite {
    padding: 0 0.375rem;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm, 0.25rem);
    background: transparent;
    color: var(--primary);
    font: inherit;
    font-size: 0.6875rem;
    cursor: pointer;
  }
  button.cite:hover {
    background: var(--accent);
  }
  .cite:focus-visible {
    outline: 2px solid var(--ring);
    outline-offset: 1px;
  }
  .cite.static {
    color: var(--muted-foreground);
    cursor: default;
  }
  .actions {
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
