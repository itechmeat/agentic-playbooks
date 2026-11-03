<script lang="ts">
  // The confirmation of a start or a resume the server refused with a
  // nonce (`launch_requires_confirmation`, `billing_requires_confirmation`):
  // what the person is asked, in their words, with the agents and bases a
  // billing refusal holds and the settings behind it. Only the confirm
  // button calls `onconfirm`, which retries with the refusal's nonce; one
  // confirmation covers this one start.
  import * as AlertDialog from '$lib/components/ui/alert-dialog'
  import type { LaunchQuestion } from './launch'

  let {
    open = $bindable(false),
    question,
    action = 'run',
    onconfirm,
  }: {
    open?: boolean
    question: LaunchQuestion | null
    action?: 'run' | 'resume'
    onconfirm: () => void
  } = $props()

  function confirm() {
    open = false
    onconfirm()
  }
</script>

<AlertDialog.Root bind:open>
  <AlertDialog.Content data-testid="launch-confirm-dialog">
    {#if question}
      <AlertDialog.Header>
        <AlertDialog.Title>{question.title}</AlertDialog.Title>
        <AlertDialog.Description>{question.reason}</AlertDialog.Description>
      </AlertDialog.Header>
      {#if question.held.length}
        <ul class="held" data-testid="launch-confirm-held">
          {#each question.held as entry, i (i)}
            <li>{entry}</li>
          {/each}
        </ul>
      {/if}
      {#if question.sources.length}
        <p class="note" data-testid="launch-confirm-sources">
          Asked by: {question.sources.join(', ')}
        </p>
      {/if}
      <p class="note">
        Confirming covers this one {action === 'resume' ? 'resume' : 'run'}; the next one asks again.
      </p>
      <AlertDialog.Footer>
        <AlertDialog.Cancel data-testid="launch-confirm-cancel">Cancel</AlertDialog.Cancel>
        <AlertDialog.Action data-testid="launch-confirm-ok" onclick={confirm}
          >{action === 'resume' ? 'Resume it' : 'Start it'}</AlertDialog.Action
        >
      </AlertDialog.Footer>
    {/if}
  </AlertDialog.Content>
</AlertDialog.Root>

<style>
  .held {
    margin: 0;
    padding: 8px 12px 8px 28px;
    list-style: square;
    border: 1px solid var(--c-line);
    background: var(--c-field);
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 13px;
    overflow-wrap: anywhere;
  }
  .held li::marker {
    color: var(--c-amber);
  }
  .note {
    margin: 0;
    font-size: 12px;
    color: var(--muted-foreground);
    overflow-wrap: anywhere;
  }
</style>
