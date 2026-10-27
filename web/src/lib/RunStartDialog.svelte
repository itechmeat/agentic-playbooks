<script lang="ts">
  // The Run dialog: what a run of this playbook is expected to take and spend
  // (from its comparable past runs on this machine), and its budget. The
  // token and dollar inputs start from the playbook's `budget:` block; the
  // pause or continue choice appears when the policy is `ask`, the start will
  // have a limit (typed or the block's) and a gated node has no estimate in a
  // limited dimension, or the estimate could not be read, so the start is
  // never refused for a decision the person was not shown. `retry` reopens
  // the dialog after such a refusal: the typed values stay and the choice is
  // shown. Run hands the budget to `onrun`; the page starts the run (and its
  // irreversible-consent dialog, when needed).
  import { untrack } from 'svelte'
  import { fetchEstimate, type RunBudget } from './api'
  import type { RunEstimate } from './api.gen'
  import {
    budgetFromForm,
    comparableText,
    effectiveLimit,
    estimateRows,
    formFromBlock,
    needsPolicyChoice,
    unestimatedText,
    type BudgetForm,
  } from './runestimate'
  import * as Dialog from '$lib/components/ui/dialog'
  import { Button } from '$lib/components/ui/button'
  import { Input } from '$lib/components/ui/input'

  let {
    open = $bindable(false),
    playbookId,
    workspace = '',
    block = null,
    busy = false,
    retry = false,
    onrun,
  }: {
    open?: boolean
    playbookId: string
    workspace?: string
    block?: RunBudget | null
    busy?: boolean
    retry?: boolean
    onrun: (budget: RunBudget | undefined) => void
  } = $props()

  let estimate = $state<(RunEstimate & { summary: string }) | null>(null)
  let loadError = $state<string | null>(null)
  let form = $state<BudgetForm>(formFromBlock(null))
  let formError = $state<string | null>(null)

  const rows = $derived(estimate ? estimateRows(estimate) : [])
  const limit = $derived(effectiveLimit(form, block))
  const unestimated = $derived(estimate ? unestimatedText(estimate, limit) : [])
  const limited = $derived(limit.tokens !== undefined || limit.usd !== undefined)
  const showPolicy = $derived(
    limited &&
      (block?.on_no_estimate ?? 'ask') === 'ask' &&
      (retry || needsPolicyChoice(estimate, 'ask', limit, loadError !== null)),
  )
  const blockLimited = $derived(!!(block?.tokens || block?.usd))

  // Each opening reads the estimate afresh and resets the inputs to the
  // playbook's block (a retry keeps what the person typed). Only `open` and
  // the route reset the dialog: a reload of the page that hands a new
  // `block` object leaves typed values alone, and a response that lands
  // after a newer opening (or after closing) is dropped.
  $effect(() => {
    if (!open) return
    const id = playbookId
    const ws = workspace
    untrack(() => {
      if (!retry) form = formFromBlock(block)
      formError = null
      estimate = null
      loadError = null
    })
    let live = true
    fetchEstimate(id, ws)
      .then((e) => {
        if (live) estimate = e
      })
      .catch((e: unknown) => {
        if (live) loadError = e instanceof Error ? e.message : String(e)
      })
    return () => {
      live = false
    }
  })

  function submit() {
    // An unanswered `ask` is not sent: the server would refuse the start.
    if (showPolicy && form.policy === 'ask') {
      formError = 'choose whether the run pauses or continues before the nodes with no estimate'
      return
    }
    const { budget, error } = budgetFromForm(form, block)
    if (error) {
      formError = error
      return
    }
    formError = null
    open = false
    onrun(budget)
  }
</script>

<Dialog.Root bind:open>
  <Dialog.Content data-testid="run-start-dialog">
    <Dialog.Header>
      <Dialog.Title>Run {playbookId}</Dialog.Title>
      <Dialog.Description>
        The estimate comes from comparable past runs of this version on this machine.
      </Dialog.Description>
    </Dialog.Header>

    <section class="estimate" data-testid="run-start-estimate">
      {#if loadError}
        <p class="muted">No estimate: {loadError}</p>
      {:else if !estimate}
        <p class="muted">Reading past runs...</p>
      {:else}
        <dl>
          {#each rows as r (r.label)}
            <dt>{r.label}</dt>
            <dd class:muted={!r.known}>{r.value}</dd>
          {/each}
        </dl>
        <p class="muted" data-testid="run-start-comparable">{comparableText(estimate)}</p>
        {#if estimate.note}<p class="muted">{estimate.note}</p>{/if}
        {#if unestimated.length}
          <p data-testid="run-start-unestimated">Not estimated: {unestimated.join(', ')}</p>
        {/if}
      {/if}
    </section>

    <form
      class="budget"
      onsubmit={(e) => {
        e.preventDefault()
        submit()
      }}
    >
      <fieldset>
        <legend>Budget (optional)</legend>
        <p class="muted hint">
          The run pauses before a node whose median spend no longer fits. A predictive stop, not a
          hard ceiling.
        </p>
        {#if blockLimited}
          <p class="muted hint" data-testid="run-start-block-hint">
            The playbook sets a budget: an empty field keeps its limit.
          </p>
        {/if}
        <div class="fields">
          <label>
            Tokens
            <Input data-testid="run-start-tokens" bind:value={form.tokens} placeholder="such as 300k" />
          </label>
          <label>
            Dollars
            <Input data-testid="run-start-usd" bind:value={form.usd} placeholder="such as 3.50" />
          </label>
        </div>
      </fieldset>
      {#if showPolicy}
        <fieldset class="policy" data-testid="run-start-policy">
          <legend>Before a node with no estimate</legend>
          <label>
            <input type="radio" name="run-start-policy" value="pause" bind:group={form.policy} />
            Pause the run
          </label>
          <label>
            <input type="radio" name="run-start-policy" value="continue" bind:group={form.policy} />
            Run it anyway
          </label>
        </fieldset>
      {/if}
      {#if formError}<p class="error" data-testid="run-start-error">{formError}</p>{/if}
      <Dialog.Footer>
        <Button type="button" variant="ghost" onclick={() => (open = false)}>Cancel</Button>
        <Button type="submit" data-testid="run-start-submit" disabled={busy}>Run</Button>
      </Dialog.Footer>
    </form>
  </Dialog.Content>
</Dialog.Root>

<style>
  /* Plain CSS over the app's variables. */
  .estimate,
  .budget {
    display: flex;
    flex-direction: column;
    gap: 8px;
    font-size: 13px;
  }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 4px 12px;
    margin: 0;
  }
  dt {
    color: var(--muted-foreground);
  }
  dd {
    margin: 0;
    font-family: var(--font-mono, ui-monospace, monospace);
    overflow-wrap: anywhere;
  }
  .muted {
    color: var(--muted-foreground);
  }
  .hint {
    font-size: 12px;
  }
  .error {
    color: var(--destructive);
  }
  fieldset {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin: 0;
    padding: 0;
    border: 0;
    min-width: 0;
  }
  legend {
    padding: 0;
    margin-bottom: 4px;
    font-weight: 600;
  }
  .fields {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
    gap: 8px;
  }
  .fields label {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .policy label {
    display: flex;
    align-items: center;
    gap: 6px;
  }
</style>
