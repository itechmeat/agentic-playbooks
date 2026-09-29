<script lang="ts">
  // 0.24.0 irreversible consent: the Run dialog for a playbook the server
  // refused with `irreversible_requires_confirmation`. It lists the refusal's
  // structured `sources` (never the free-text detail), and only its confirm
  // button calls `onconfirm`, which sends the refusal's `consent_nonce`.
  import * as AlertDialog from '$lib/components/ui/alert-dialog'

  let {
    open = $bindable(false),
    playbookId,
    sources = [],
    onconfirm,
  }: {
    open?: boolean
    playbookId: string
    sources?: string[]
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
      <AlertDialog.Title>Run a playbook with irreversible effects?</AlertDialog.Title>
      <AlertDialog.Description>
        {#if sources.length}
          The playbook <code>{playbookId}</code> has irreversible effects:
        {:else}
          The playbook <code>{playbookId}</code> declares irreversible effects, such as a push, a merge or a deploy.
        {/if}
      </AlertDialog.Description>
    </AlertDialog.Header>
    {#if sources.length}
      <ul class="list-disc pl-6 text-sm" data-testid="irreversible-sources">
        {#each sources as source, i (i)}
          <li>{source}</li>
        {/each}
      </ul>
    {/if}
    <p class="text-sm text-muted-foreground">
      Starting it records your consent in the run manifest, and its sub-playbooks inherit it.
    </p>
    <AlertDialog.Footer>
      <AlertDialog.Cancel data-testid="irreversible-cancel">Cancel</AlertDialog.Cancel>
      <AlertDialog.Action data-testid="irreversible-confirm" onclick={confirm}>Run it</AlertDialog.Action>
    </AlertDialog.Footer>
  </AlertDialog.Content>
</AlertDialog.Root>
