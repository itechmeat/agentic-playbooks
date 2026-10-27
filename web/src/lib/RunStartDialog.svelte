<script lang="ts">
  // The Run dialog: what a run of this playbook is expected to take and spend
  // (from its comparable past runs on this machine), and its budget. The
  // token and dollar inputs start from the playbook's `budget:` block; the
  // pause or continue choice appears when the policy is `ask`, the start will
  // have a limit (typed or the block's) and a gated node has no estimate in a
  // limited dimension, or the estimate could not be read, so the start is
  // never refused for a decision the person was not shown. `retry` reopens
  // the dialog after such a refusal: the typed values stay and the choice is
  // shown. The params and the instruction start from what this project
  // remembers for the playbook (a secret param is never prefilled, and a
  // remembered value that no longer fits is shown with the reason and not
  // used); "Remember for this project" keeps what this start uses. What the
  // person types before the memory answer arrives is kept, and Run waits for
  // that answer, so a start never uses remembered input the dialog did not
  // show. Run hands the budget and the input to `onrun`; the page starts the
  // run (and its irreversible-consent dialog, when needed).
  import { untrack } from 'svelte'
  import { fetchEstimate, fetchMemory, type RunBudget, type RunInput } from './api'
  import type { RunEstimate } from './api.gen'
  import type { PlaybookParam } from './types'
  import {
    instructionFrom,
    memoryNoteFrom,
    mergeRecalled,
    missingValues,
    paramFields,
    runInputFrom,
    type ParamField,
  } from './runinput'
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
  import { Textarea } from '$lib/components/ui/textarea'

  let {
    open = $bindable(false),
    playbookId,
    workspace = '',
    block = null,
    params = [],
    busy = false,
    retry = false,
    onrun,
  }: {
    open?: boolean
    playbookId: string
    workspace?: string
    block?: RunBudget | null
    params?: PlaybookParam[]
    busy?: boolean
    retry?: boolean
    onrun: (budget: RunBudget | undefined, input: RunInput) => void
  } = $props()

  let fields = $state<ParamField[]>([])
  let instruction = $state('')
  let initialInstruction = ''
  let remember = $state(false)
  let memoryNote = $state<string | null>(null)
  let memoryLoading = $state(false)

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
    const fresh = untrack(() => !retry)
    untrack(() => {
      if (fresh) {
        form = formFromBlock(block)
        fields = paramFields(params, null)
        instruction = ''
        initialInstruction = ''
        remember = false
        memoryNote = null
      }
      formError = null
      estimate = null
      loadError = null
    })
    let live = true
    if (fresh) {
      memoryLoading = true
      // What the fields held before anyone typed: a field still equal to
      // it takes the remembered value.
      const base = paramFields(
        untrack(() => params),
        null,
      )
      fetchMemory(id, ws)
        .then((recall) => {
          if (!live) return
          const recalled = paramFields(
            untrack(() => params),
            recall,
          )
          fields = mergeRecalled(
            untrack(() => fields),
            base,
            recalled,
          )
          initialInstruction = instructionFrom(recall)
          if (untrack(() => instruction) === '') instruction = initialInstruction
          memoryNote = memoryNoteFrom(recall)
        })
        .catch(() => {
          if (live) memoryNote = 'The remembered input could not be read.'
        })
        .finally(() => {
          if (live) memoryLoading = false
        })
    }
    fetchEstimate(id, ws)
      .then((e) => {
        if (live) estimate = e
      })
      .catch((e: unknown) => {
        if (live) loadError = e instanceof Error ? e.message : String(e)
      })
    return () => {
      live = false
      // A load cut short by closing must not leave Run disabled for a
      // reopening that does not load again (a retry).
      memoryLoading = false
    }
  })

  function submit() {
    // Until memory answered, a start could pick up remembered input the
    // dialog never showed.
    if (memoryLoading) return
    // An unanswered `ask` is not sent: the server would refuse the start.
    if (showPolicy && form.policy === 'ask') {
      formError = 'choose whether the run pauses or continues before the nodes with no estimate'
      return
    }
    const missing = missingValues(fields)
    if (missing.length) {
      formError = `enter a value for: ${missing.join(', ')}`
      return
    }
    const { budget, error } = budgetFromForm(form, block)
    if (error) {
      formError = error
      return
    }
    formError = null
    open = false
    onrun(budget, runInputFrom(fields, instruction, remember, initialInstruction))
  }
</script>

<Dialog.Root bind:open>
  <Dialog.Content class="run-dialog" data-testid="run-start-dialog">
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
      <fieldset class="input" data-testid="run-start-input">
        <legend>Input</legend>
        {#if memoryNote}<p class="muted hint" data-testid="run-start-memory-note">{memoryNote}</p>{/if}
        {#each fields as f, i (f.name)}
          <label class="param">
            <span>{f.label}{f.required ? ' *' : ''}</span>
            {#if f.secret}
              <Input
                type="password"
                autocomplete="off"
                data-testid="run-start-param-{f.name}"
                bind:value={fields[i].value}
              />
            {:else if f.type === 'enum' && f.options.length}
              <select class="select" data-testid="run-start-param-{f.name}" bind:value={fields[i].value}>
                <option value="">(default)</option>
                {#each f.options as o (o)}<option value={o}>{o}</option>{/each}
              </select>
            {:else if f.type === 'bool'}
              <select class="select" data-testid="run-start-param-{f.name}" bind:value={fields[i].value}>
                <option value="">(default)</option>
                <option value="true">true</option>
                <option value="false">false</option>
              </select>
            {:else}
              <Input
                inputmode={f.type === 'int' ? 'numeric' : undefined}
                data-testid="run-start-param-{f.name}"
                bind:value={fields[i].value}
              />
            {/if}
            {#if f.note}<span class="muted hint" data-testid="run-start-param-note-{f.name}">{f.note}</span>{/if}
          </label>
        {/each}
        <label class="param">
          <span>Instruction</span>
          <Textarea rows={3} data-testid="run-start-instruction" bind:value={instruction} />
        </label>
        <label class="check">
          <input type="checkbox" data-testid="run-start-remember" bind:checked={remember} />
          Remember for this project
        </label>
        <p class="muted hint">
          Secret params and values that look like credentials are never remembered.
        </p>
      </fieldset>
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
        <Button type="submit" data-testid="run-start-submit" disabled={busy || memoryLoading}>Run</Button>
      </Dialog.Footer>
    </form>
  </Dialog.Content>
</Dialog.Root>

<style>
  /* The input section makes the dialog taller than a short window: it
     scrolls inside the viewport instead of running off it. */
  :global(.run-dialog) {
    max-height: calc(100dvh - 2rem);
    overflow-y: auto;
  }
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
  .param {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .select {
    height: 32px;
    border: 1px solid var(--input, var(--border));
    border-radius: var(--radius-md, 6px);
    background: transparent;
    color: inherit;
    padding: 0 8px;
    font: inherit;
  }
  .policy label {
    display: flex;
    align-items: center;
    gap: 6px;
  }
</style>
