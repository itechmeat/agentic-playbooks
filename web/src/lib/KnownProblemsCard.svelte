<script lang="ts">
  import type { ChangedPassage, ProblemView, ProblemsView } from './api.gen'
  import type { ProblemAction, ProblemDecision } from './api'
  import OutcomeSection from '$lib/components/OutcomeSection.svelte'
  import OutcomeTag from '$lib/components/OutcomeTag.svelte'
  import { Button } from '$lib/components/ui/button'
  import {
    actionsFor,
    checkText,
    closeHint,
    groupProblems,
    originText,
    problemStateText,
    problemTone,
    quoteProblem,
    runsText,
    scopeText,
    otherWordings,
    sourceNowText,
    unsignedText,
  } from './knownproblems'

  // A playbook's known problems: trouble with its tools that earlier runs
  // reported, grouped as shown to runs, needing a look, not shown yet, and
  // closed or dismissed. A person confirms (shown before it recurs),
  // dismisses (never shown), reopens, says it still holds after its source
  // changed, asks the decision model to recheck, or closes it with a quote
  // copied from the source's new text (the passages new since it was seen
  // load when the close form opens). A file this installation did not
  // write is only counted.
  let {
    view,
    loading = false,
    error = null,
    busy = new Set<string>(),
    onAct,
    loadPassages,
  }: {
    view: ProblemsView | null
    loading?: boolean
    error?: string | null
    /** The problems whose action is in flight. */
    busy?: ReadonlySet<string>
    /** Acts on a problem; resolves to null when it was done, else why not. */
    onAct: (problem: string, action: ProblemAction, body: ProblemDecision) => Promise<string | null>
    loadPassages: (problem: string) => Promise<ChangedPassage[]>
  } = $props()

  const groups = $derived(groupProblems(view?.problems ?? []))
  const sections = $derived(
    [
      { key: 'look', title: 'Needs a look', items: groups.look },
      { key: 'shown', title: 'Shown to runs', items: groups.shown },
      { key: 'waiting', title: 'Not shown yet', items: groups.waiting },
      { key: 'done', title: 'Closed and dismissed', items: groups.done },
    ].filter((s) => s.items.length > 0),
  )

  // The one close form open at a time, with its passages and fields.
  let closing = $state<string | null>(null)
  let passages = $state<ChangedPassage[] | null>(null)
  let passagesError = $state<string | null>(null)
  let quote = $state('')
  let reason = $state('')
  let closeError = $state<string | null>(null)
  const quoteError = $derived(quote.trim() ? quoteProblem(quote) : null)
  // Each opening of the form is its own load: a response for an earlier
  // opening (cancelled, or reopened since) is dropped.
  let closeToken = 0

  async function openClose(p: ProblemView) {
    const token = ++closeToken
    closing = p.id
    passages = null
    passagesError = null
    closeError = null
    quote = ''
    reason = ''
    try {
      const got = await loadPassages(p.id)
      if (token === closeToken) passages = got
    } catch (e) {
      if (token === closeToken) passagesError = String(e)
    }
  }

  function cancelClose() {
    closeToken++
    closing = null
    closeError = null
  }

  // The form stays open until the server closes the problem: a refused
  // quote keeps what the person typed or picked, with the reason shown.
  async function submitClose(id: string) {
    const token = closeToken
    closeError = null
    const failed = await onAct(id, 'close', { quote, reason: reason.trim() || undefined })
    if (token !== closeToken) return
    if (failed) closeError = failed
    else closing = null
  }
</script>

<OutcomeSection title="Reported by earlier runs" testid="known-problems-card">
  {#if loading && !view}
    <p class="muted">Loading...</p>
  {:else if error}
    <p class="error" data-testid="known-problems-error">{error}</p>
  {:else if view && !view.signed}
    <p class="error" data-testid="known-problems-unsigned">{unsignedText(view.unsigned_problems)}</p>
  {:else if !view || view.problems.length === 0}
    <p class="muted" data-testid="known-problems-empty">
      No known problems. Runs collect them from findings about tools and from connector calls that fail alike.
    </p>
  {:else}
    {#each sections as s (s.key)}
      <div class="group" data-testid="known-problems-group" data-group={s.key}>
        <h4>{s.title} <span class="muted">({s.items.length})</span></h4>
        <ul>
          {#each s.items as p (p.id)}
            {@const can = actionsFor(p)}
            {@const hint = sourceNowText(p.source_now)}
            {@const others = [
              ...otherWordings(p.symptoms, p.symptom).map((t) => ({ ...t, what: 'symptom' })),
              ...otherWordings(p.workarounds, p.workaround).map((t) => ({ ...t, what: 'workaround' })),
            ]}
            <li data-testid="known-problem" data-id={p.id} data-state={problemStateText(p)}>
              <div class="row">
                <OutcomeTag tone={problemTone(p)}>{problemStateText(p)}</OutcomeTag>
                <span class="mono scope">{scopeText(p.scope)}</span>
              </div>
              <p class="symptom">{p.symptom}</p>
              {#if p.workaround}
                <p class="muted">Workaround reported: {p.workaround}</p>
              {/if}
              {#if others.length > 0}
                <details class="wordings" data-testid="known-problem-wordings">
                  <summary class="muted">Other wordings runs reported ({others.length})</summary>
                  <ul>
                    {#each others as w, i (i)}
                      <li class="muted">{w.what}, {runsText(w.runs)}: {w.text}</li>
                    {/each}
                  </ul>
                </details>
              {/if}
              <p class="muted meta">
                {runsText(p.runs)} · {originText(p.origin)}{#if p.confirmed}
                  · confirmed by {p.confirmed.by}{/if}
              </p>
              {#if hint && p.status === 'open'}
                <p class="hint" data-testid="known-problem-source">{hint}</p>
              {/if}
              {#if p.check}
                <p class="muted" data-testid="known-problem-check">
                  Last check: {checkText(p.check)}
                </p>
                {#if p.check.quote}
                  <blockquote data-testid="known-problem-quote">
                    {p.check.quote.text}
                    {#if p.check.quote.lines[0] > 0}
                      <span class="muted">(lines {p.check.quote.lines[0]}-{p.check.quote.lines[1]})</span>
                    {/if}
                  </blockquote>
                {/if}
              {/if}

              {#if closing === p.id}
                <div class="close-form" data-testid="known-problem-close-form">
                  {#if passages === null && !passagesError && p.source && p.source_now !== 'unreadable'}
                    <p class="muted">Loading the new passages of its source...</p>
                  {:else if passagesError}
                    <p class="error">{passagesError}</p>
                  {:else if passages && passages.length > 0}
                    <p class="muted">Pick the passage that shows the fix, or paste text copied from the source:</p>
                    <div class="passages">
                      {#each passages as q (q.index)}
                        <button
                          type="button"
                          class="passage"
                          class:picked={quote === q.text}
                          data-testid="known-problem-passage"
                          onclick={() => (quote = q.text)}
                        >
                          <span class="muted">lines {q.lines[0]}-{q.lines[1]}</span>
                          <span class="mono">{q.text}</span>
                        </button>
                      {/each}
                    </div>
                  {:else}
                    <p class="muted" data-testid="known-problem-close-hint">{closeHint(p)}</p>
                  {/if}
                  <label class="field">
                    <span class="muted">Quote (copied exactly)</span>
                    <textarea rows="3" maxlength="400" bind:value={quote} data-testid="known-problem-quote-input"
                    ></textarea>
                  </label>
                  {#if quoteError}
                    <p class="error" data-testid="known-problem-quote-error">{quoteError}</p>
                  {/if}
                  {#if closeError}
                    <p class="error" data-testid="known-problem-close-error">Not closed: {closeError}</p>
                  {/if}
                  <label class="field">
                    <span class="muted">Reason (optional)</span>
                    <input type="text" maxlength="300" bind:value={reason} />
                  </label>
                  <div class="buttons">
                    <Button variant="outline" size="sm" onclick={cancelClose} data-testid="known-problem-close-cancel"
                      >Cancel</Button
                    >
                    <Button
                      size="sm"
                      disabled={!quote.trim() || !!quoteError || busy.has(p.id)}
                      onclick={() => submitClose(p.id)}
                      data-testid="known-problem-close-submit"
                    >
                      Close problem
                    </Button>
                  </div>
                </div>
              {:else}
                <div class="buttons">
                  {#if can.confirm}
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={busy.has(p.id)}
                      onclick={() => onAct(p.id, 'confirm', {})}
                      data-testid="known-problem-confirm"
                      title="Show it to runs even before it recurs"
                    >
                      Confirm
                    </Button>
                  {/if}
                  {#if can.holds}
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={busy.has(p.id)}
                      onclick={() => onAct(p.id, 'recheck', { verdict: 'holds' })}
                      data-testid="known-problem-holds"
                      title="The problem still holds after its source changed"
                    >
                      Still holds
                    </Button>
                  {/if}
                  {#if can.recheck}
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={busy.has(p.id)}
                      onclick={() => onAct(p.id, 'recheck', {})}
                      data-testid="known-problem-recheck"
                      title="Ask the project's decision model whether the change fixed it (only when decisions.yaml turns problem_recheck on)"
                    >
                      Recheck by model
                    </Button>
                  {/if}
                  {#if can.close}
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={busy.has(p.id)}
                      onclick={() => openClose(p)}
                      data-testid="known-problem-close"
                      title="Close it with a quote of the source's new text that shows the fix"
                    >
                      Close...
                    </Button>
                  {/if}
                  {#if can.dismiss}
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={busy.has(p.id)}
                      onclick={() => onAct(p.id, 'dismiss', {})}
                      data-testid="known-problem-dismiss"
                      title="Never show it to runs"
                    >
                      Dismiss
                    </Button>
                  {/if}
                  {#if can.reopen}
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={busy.has(p.id)}
                      onclick={() => onAct(p.id, 'reopen', {})}
                      data-testid="known-problem-reopen"
                    >
                      Reopen
                    </Button>
                  {/if}
                </div>
              {/if}
            </li>
          {/each}
        </ul>
      </div>
    {/each}
  {/if}
</OutcomeSection>

<style>
  p {
    margin: 0;
  }
  .group {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
  }
  h4 {
    margin: 0;
    font-size: 0.75rem;
    font-weight: 600;
  }
  ul {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  li {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding-bottom: 0.5rem;
    border-bottom: 1px solid var(--border);
  }
  li:last-child {
    border-bottom: 0;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.375rem;
  }
  .scope {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .hint {
    padding-left: 0.5rem;
    border-left: 2px solid var(--warning);
  }
  blockquote {
    margin: 0;
    padding: 0.25rem 0.5rem;
    border-left: 2px solid var(--success, var(--border));
    font-family: var(--font-mono, ui-monospace, monospace);
    white-space: pre-wrap;
  }
  .buttons {
    display: flex;
    flex-wrap: wrap;
    gap: 0.375rem;
  }
  .close-form {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    padding: 0.5rem;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm, 0.25rem);
  }
  .passages {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .passage {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    padding: 0.25rem 0.375rem;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm, 0.25rem);
    background: transparent;
    color: inherit;
    font: inherit;
    text-align: left;
    white-space: pre-wrap;
    cursor: pointer;
  }
  .passage:hover {
    background: var(--accent);
  }
  .passage.picked {
    border-color: var(--primary);
  }
  .passage:focus-visible {
    outline: 2px solid var(--ring);
    outline-offset: 1px;
  }
  .wordings ul {
    gap: 0.125rem;
    margin-top: 0.25rem;
  }
  .wordings li {
    padding-bottom: 0;
    border-bottom: 0;
  }
  summary {
    cursor: pointer;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
  }
  textarea,
  input {
    width: 100%;
    padding: 0.25rem 0.375rem;
    border: 1px solid var(--input, var(--border));
    border-radius: var(--radius-sm, 0.25rem);
    background: var(--background);
    color: var(--foreground);
    font: inherit;
  }
  textarea {
    font-family: var(--font-mono, ui-monospace, monospace);
    resize: vertical;
  }
  .muted {
    color: var(--muted-foreground);
  }
  .error {
    color: var(--destructive);
  }
  .mono {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
</style>
