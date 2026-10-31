<script lang="ts">
  // 0.24.0 irreversible consent: the Run dialog for a playbook the server
  // refused with `irreversible_requires_confirmation`. It lists the refusal's
  // structured `sources` (never the free-text detail), and only its confirm
  // button calls `onconfirm`, which sends the refusal's `consent_nonce`.
  // `action` says what the person is about to do: start a run (the default)
  // or resume a paused one (issue #200), which asks the same question.
  import * as AlertDialog from '$lib/components/ui/alert-dialog'

  let {
    open = $bindable(false),
    playbookId,
    sources = [],
    action = 'run',
    onconfirm,
  }: {
    open?: boolean
    playbookId: string
    sources?: string[]
    action?: 'run' | 'resume'
    onconfirm: () => void
  } = $props()

  function confirm() {
    open = false
    onconfirm()
  }
</script>

<AlertDialog.Root bind:open>
  <AlertDialog.Content data-testid="irreversible-dialog">
    <AlertDialog.Header>
      <AlertDialog.Title>{action === 'resume' ? 'Resume' : 'Run'} a playbook with irreversible effects?</AlertDialog.Title>
      <AlertDialog.Description>
        {#if sources.length}
          The playbook <code class="text-amber">{playbookId}</code> has irreversible effects:
        {:else}
          The playbook <code class="text-amber">{playbookId}</code> declares irreversible effects, such as a push, a merge or a deploy.
        {/if}
      </AlertDialog.Description>
    </AlertDialog.Header>
    {#if sources.length}
      <ul class="m-0 list-[square] border border-line bg-field py-2 pr-3 pl-7 text-sm marker:text-amber" data-testid="irreversible-sources">
        {#each sources as source, i (i)}
          <li>{source}</li>
        {/each}
      </ul>
    {/if}
    <p class="text-sm text-muted-foreground">
      {action === 'resume' ? 'Resuming' : 'Starting'} it records your consent in the run manifest, and its sub-playbooks inherit it.
    </p>
    <AlertDialog.Footer>
      <AlertDialog.Cancel data-testid="irreversible-cancel">Cancel</AlertDialog.Cancel>
      <AlertDialog.Action data-testid="irreversible-confirm" onclick={confirm}>{action === 'resume' ? 'Resume it' : 'Run it'}</AlertDialog.Action>
    </AlertDialog.Footer>
  </AlertDialog.Content>
</AlertDialog.Root>
