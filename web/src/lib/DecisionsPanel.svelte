<script lang="ts">
  import type { RunDecisions } from './api.gen'
  import type { WfEvent } from './types'
  import { decisionsSummary, decisionUseLines, exampleDraftOf, type ExampleDraft } from './rundecisions'
  import { decisionBars, decisionSource } from './runview'
  import { refusalText, saveJudgeExample } from './api'
  import { authState } from './auth.svelte'
  import type { JudgeExample } from './api.gen'
  import { Button } from '$lib/components/ui/button'

  // The run's decision layer (issue #165 Part 4): totals, one line per use,
  // and one bar per decision with its node and outcome (applied or not), how
  // decisive the answer was (green when sharp or applied, orange when split,
  // red on a provider error), the value, the verdict and every answer. Renders nothing when the run journaled no
  // decision, so an unconfigured run's page is unchanged.
  // A judge node's decision can be saved as a labelled example of that node
  // (`evals/judge/<node>/`): the person names the expected answer per
  // question, the server stores the execution's inputs redacted and shows
  // what it stored.
  let {
    decisions,
    events,
    runId,
    workspace = '',
  }: { decisions?: RunDecisions | null; events: WfEvent[]; runId?: string; workspace?: string } = $props()

  let draft = $state<(ExampleDraft & { seq: number; expected: Record<string, string>; note: string }) | null>(null)
  let saving = $state(false)
  let saved = $state<JudgeExample | null>(null)
  let saveError = $state<string | null>(null)
  // A dashboard without keys refuses saving an example (it writes into the
  // project, and anything on this machine reaches it): once the status
  // read answered, the button is off and the command line is named.
  const authSnapshot = authState()
  const keyless = $derived(authSnapshot.checked && !authSnapshot.required)
  // The run page keeps this panel when it moves to another run: a draft,
  // a result or a save still on its way belongs to the run it was made on.
  // `page` counts the moves; a save answers only on the page it started on.
  let page = 0
  $effect.pre(() => {
    void runId
    void workspace
    page += 1
    draft = null
    saved = null
    saveError = null
    saving = false
  })

  function openDraft(seq: number) {
    const d = exampleDraftOf(events, seq)
    if (!d) return
    saved = null
    saveError = null
    draft = { ...d, seq, expected: Object.fromEntries(d.questions.map((q) => [q.name, q.observed])), note: '' }
  }

  async function saveDraft() {
    if (!draft || !runId) return
    const d = draft
    const expected: Record<string, string | boolean> = {}
    for (const q of d.questions) {
      const v = d.expected[q.name] ?? ''
      if (v === '') continue
      expected[q.name] = q.yesNo ? v === 'true' : v
    }
    saving = true
    saveError = null
    const on = page
    try {
      const stored = await saveJudgeExample(
        runId,
        d.node,
        {
          ...(d.execution ? { execution: d.execution } : {}),
          expected,
          ...(d.note.trim() ? { note: d.note.trim() } : {}),
        },
        workspace,
      )
      if (on !== page) return
      saved = stored
      draft = null
    } catch (e) {
      if (on === page) saveError = refusalText(e)
    } finally {
      if (on === page) saving = false
    }
  }

  function storedLines(x: JudgeExample): string[] {
    const i = x.inputs
    const lines: string[] = []
    if (i.instruction) lines.push(`instruction: ${i.instruction}`)
    for (const [k, v] of Object.entries(i.params)) lines.push(`params.${k}: ${v}`)
    for (const [k, v] of Object.entries(i.outputs)) lines.push(`outputs.${k}: ${v}`)
    for (const [k, v] of Object.entries(i.reviews)) lines.push(`reviews.${k}: ${v.decision}${v.note ? `, ${v.note}` : ''}`)
    return lines
  }

  const bars = $derived(decisions ? decisionBars(events) : [])
  const tone = { sharp: 'green', split: 'orange', error: 'red' } as const
  const source = $derived(decisions ? decisionSource(events) : null)
</script>

{#if decisions}
  <section class="box px-4 pt-4 pb-3" style:--k="var(--c-green)" data-testid="run-decisions">
    <span class="box-cap">Decisions</span>
    <div class="mb-2 flex items-baseline justify-between gap-2">
      <span class="hd text-green">Decision layer</span>
      <span class="text-xs text-muted-foreground">asks <b class="text-green">{decisions.decisions}</b></span>
    </div>
    {#if bars.length}
      <ol class="flex max-h-72 flex-col overflow-auto pr-1">
        {#each bars as b (b.seq)}
          <li
            class="grid grid-cols-[minmax(0,1fr)_64px_auto] items-center gap-x-2 py-1 text-xs"
            data-testid="run-decision-row"
            title={`${b.answer} · ${b.outcome} · ${b.source}${b.latency ? ` · ${b.latency}` : ''}`}
          >
            <span class="min-w-0">
              <span class="block truncate text-foreground">{b.use}</span>
              <span class="block truncate text-[11px] text-muted-foreground">{b.node ?? 'run'} · {b.outcome}</span>
              {#if b.answer}<span class="block truncate text-[11px] text-fg-2">{b.answer}</span>{/if}
            </span>
            <span
              class="hatch-bar"
              style:--k={`var(--c-${tone[b.verdict]})`}
              aria-hidden="true"><i style:width="{Math.round(b.strength * 100)}%"></i></span
            >
            <span class="min-w-0 text-right whitespace-nowrap">
              <span class="text-foreground">{b.value || '-'}</span>
              <span style:color={`var(--c-${tone[b.verdict]})`}>{b.verdict}</span>
            </span>
            {#if runId && exampleDraftOf(events, b.seq)}
              <span class="dp-save">
                <Button
                  variant="outline"
                  size="xs"
                  disabled={keyless}
                  onclick={() => openDraft(b.seq)}
                  data-testid="decision-save-example"
                  title="Save this judge decision as a labelled example of its node"
                >
                  Save as example
                </Button>
              </span>
            {/if}
          </li>
        {/each}
      </ol>
      {#if keyless && runId && bars.some((b) => exampleDraftOf(events, b.seq))}
        <p class="dp-muted" data-testid="decision-save-keyless">
          A dashboard without keys does not save examples: run
          <span class="dp-mono">apb judge save-example {runId} &lt;node&gt; --expect question=answer</span> in a terminal.
        </p>
      {/if}
    {/if}
    {#if draft}
      {@const d = draft}
      <div class="dp-draft" data-testid="example-draft">
        <p>
          Example of <span class="dp-mono">{d.node}</span>{d.execution ? `, execution ${d.execution}` : ''}: the expected
          answer per question (leave one empty to keep it unlabelled)
        </p>
        {#each d.questions as q (q.name)}
          <label class="dp-row">
            <span class="dp-mono">{q.name}</span>
            {#if q.yesNo}
              <select
                value={d.expected[q.name] ?? ''}
                onchange={(e) => (d.expected[q.name] = e.currentTarget.value)}
                data-testid={`example-expected-${q.name}`}
              >
                <option value="">unlabelled</option>
                <option value="true">yes</option>
                <option value="false">no</option>
              </select>
            {:else}
              <input
                type="text"
                value={d.expected[q.name] ?? ''}
                oninput={(e) => (d.expected[q.name] = e.currentTarget.value)}
                data-testid={`example-expected-${q.name}`}
              />
            {/if}
          </label>
        {/each}
        <label class="dp-row">
          <span>Note</span>
          <input type="text" maxlength="300" value={d.note} oninput={(e) => (d.note = e.currentTarget.value)} />
        </label>
        <p class="dp-muted">The inputs are stored redacted, with secret params as a placeholder, next to the playbook.</p>
        <div class="dp-buttons">
          <Button variant="outline" size="xs" disabled={saving} onclick={() => (draft = null)}>Cancel</Button>
          <Button size="xs" disabled={saving} onclick={saveDraft} data-testid="example-save">Save</Button>
        </div>
      </div>
    {/if}
    {#if saveError}<p class="dp-error" role="status" data-testid="example-save-error">{saveError}</p>{/if}
    {#if saved}
      <div class="dp-draft" data-testid="example-saved">
        <p>Saved as example <span class="dp-mono">{saved.id}</span> of {saved.node}. Stored inputs:</p>
        {#each storedLines(saved) as line, i (i)}<p class="dp-mono dp-clip" title={line}>{line}</p>{/each}
      </div>
    {/if}
    <div class="mt-2 flex flex-col gap-0.5 border-t border-line pt-2 text-[11px] text-muted-foreground">
      <p class="m-0">
        <span class="text-green">sharp: decisive or applied</span> · <span class="text-orange">split: undecided</span> ·
        <span style:color="var(--c-red)">error: provider failed</span> · the outcome under each use says whether it was applied
        {#if source}· mode {source.mode || 'unknown'} · {source.source}{/if}
      </p>
      <p class="m-0 text-fg-2" data-testid="run-decisions-summary">{decisionsSummary(decisions)}</p>
      {#each decisionUseLines(decisions) as u (u.use)}
        <p class="m-0"><span class="text-fg-2">{u.use}</span> · {u.text}</p>
      {/each}
    </div>
  </section>
{/if}

<style>
  .dp-save {
    grid-column: 1 / -1;
    justify-self: end;
  }
  .dp-draft {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    margin-top: 0.5rem;
    padding: 0.5rem;
    border: 1px solid var(--border);
    font-size: 0.75rem;
  }
  .dp-draft p {
    margin: 0;
  }
  .dp-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.375rem;
  }
  .dp-row input,
  .dp-row select {
    flex: 1 1 8rem;
    min-width: 0;
    height: 1.75rem;
    padding: 0 0.5rem;
    border: 1px solid var(--input, var(--border));
    border-radius: var(--radius-md, 0.375rem);
    background: transparent;
    color: var(--foreground);
    font: inherit;
  }
  .dp-row input:focus-visible,
  .dp-row select:focus-visible {
    outline: 2px solid var(--ring);
    outline-offset: 1px;
  }
  .dp-buttons {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
  }
  .dp-mono {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .dp-clip {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dp-muted {
    color: var(--muted-foreground);
  }
  .dp-error {
    margin: 0.25rem 0 0;
    color: var(--destructive);
    font-size: 0.75rem;
  }
</style>
