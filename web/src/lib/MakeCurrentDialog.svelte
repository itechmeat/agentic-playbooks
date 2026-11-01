<script lang="ts">
  // "Make current": the person sees the move (from -> to), may give a
  // reason, and confirms. `submit` posts the move with the version the
  // dialog showed as current: a reload of the page underneath never
  // changes it unseen. When `current` changed meanwhile the dialog says so,
  // asks the page to reload (`onstale`) and stays open on the new value.
  import { untrack } from 'svelte'
  import * as AlertDialog from '$lib/components/ui/alert-dialog'
  import { Button } from '$lib/components/ui/button'
  import { Textarea } from '$lib/components/ui/textarea'
  import type { Moved } from './api.gen'
  import { REASON_MAX, type MoveOutcome } from './currentmove'

  let {
    open = $bindable(false),
    playbookId,
    from,
    to,
    submit,
    onstale = () => {},
    ondone = () => {},
  }: {
    open?: boolean
    playbookId: string
    /** The version the page shows as current now. */
    from: string | null
    to: string
    /** Posts the move, expecting `current` to hold `expected`. */
    submit: (reason: string, expected: string | null) => Promise<MoveOutcome>
    onstale?: () => void | Promise<void>
    ondone?: (moved: Moved) => void
  } = $props()

  let reason = $state('')
  let busy = $state(false)
  let note = $state<{ kind: 'stale' | 'refused'; text: string } | null>(null)
  // The `current` the person was shown: taken when the dialog opens, and
  // after a stale answer, the value the server says it holds now.
  let shown = $state<string | null>(null)

  $effect(() => {
    if (open) {
      reason = ''
      note = null
      shown = untrack(() => from)
    }
  })

  // Already where the move would take it (a move made elsewhere got
  // there first): nothing to confirm.
  const arrived = $derived(shown === to)

  async function confirm() {
    busy = true
    try {
      const outcome = await submit(reason.trim(), shown)
      if (outcome.kind === 'moved') {
        open = false
        ondone(outcome.moved)
        return
      }
      if (outcome.kind === 'stale') {
        // Busy until the reload is done, so a second click never posts the
        // old expectation again.
        await onstale()
        // The server's answer, not the page: a reload that did not land
        // must not bring the old expectation back.
        shown = outcome.actual
        note = {
          kind: 'stale',
          text: `The current version changed meanwhile: it is now ${outcome.actual ?? 'unset'}. The list was reloaded; confirm again to move from there.`,
        }
        return
      }
      note = { kind: 'refused', text: outcome.message }
    } finally {
      busy = false
    }
  }
</script>

<AlertDialog.Root bind:open>
  <AlertDialog.Content data-testid="make-current-dialog">
    <AlertDialog.Header>
      <AlertDialog.Title>Make {to} current?</AlertDialog.Title>
      <AlertDialog.Description>
        New runs of <code>{playbookId}</code> start on the current version. Moving it grants
        nothing: a version waiting for an approval still waits.
      </AlertDialog.Description>
    </AlertDialog.Header>
    <p class="move" data-testid="make-current-move">
      <span class="from">{shown ?? 'unset'}</span>
      <span class="arrow" aria-hidden="true">-></span>
      <span class="to">{to}</span>
    </p>
    <label class="reason">
      <span>Reason (optional, kept in the history)</span>
      <Textarea
        rows={2}
        maxlength={REASON_MAX}
        value={reason}
        oninput={(e) => (reason = e.currentTarget.value)}
        data-testid="make-current-reason"
      />
    </label>
    {#if arrived}
      <p class="note" data-testid="make-current-arrived">{to} is already the current version.</p>
    {:else if note}
      <p class={['note', note.kind]} data-testid="make-current-note">{note.text}</p>
    {/if}
    <AlertDialog.Footer>
      <AlertDialog.Cancel data-testid="make-current-cancel">Cancel</AlertDialog.Cancel>
      <Button data-testid="make-current-confirm" disabled={busy || arrived} onclick={confirm}
        >Make current</Button
      >
    </AlertDialog.Footer>
  </AlertDialog.Content>
</AlertDialog.Root>

<style>
  .move {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0;
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .from {
    color: var(--muted-foreground);
  }
  .arrow {
    color: var(--c-faint);
  }
  .to {
    color: var(--c-orange);
    font-weight: 600;
  }
  .reason {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
    color: var(--muted-foreground);
  }
  .note {
    margin: 0;
    font-size: 12px;
    overflow-wrap: anywhere;
  }
  .note.stale {
    color: var(--c-amber);
  }
  .note.refused {
    color: var(--destructive);
  }
</style>
