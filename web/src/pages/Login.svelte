<script lang="ts">
  // The whole app when the server says a credential is required and this
  // browser does not have one. One field, one button: the operator pastes the
  // key from `apb server key issue`, the server swaps it for an HttpOnly
  // cookie, and the key is never stored on this side.
  import { login } from '$lib/auth'
  import { Button } from '$lib/components/ui/button'
  import { Input } from '$lib/components/ui/input'
  import * as Card from '$lib/components/ui/card'
  import { Spinner } from '$lib/components/ui/spinner'
  import ShieldCheck from '@lucide/svelte/icons/shield-check'

  let key = $state('')
  let busy = $state(false)
  let error = $state('')

  async function submit(event: SubmitEvent) {
    event.preventDefault()
    if (busy || key.trim() === '') return
    busy = true
    error = ''
    const result = await login(key.trim())
    busy = false
    if (result.ok) {
      key = ''
      return
    }
    error = result.message ?? 'Sign in failed.'
  }
</script>

<main class="flex min-h-screen items-center justify-center bg-background p-4">
  <Card.Root class="w-full max-w-sm">
    <Card.Header>
      <div class="flex items-center gap-2">
        <ShieldCheck class="size-5 text-muted-foreground" />
        <Card.Title>Sign in to apb</Card.Title>
      </div>
      <Card.Description>
        This dashboard requires an authorization key. Create one on the server with
        <code class="rounded bg-muted px-1 py-0.5 text-xs">apb server key issue</code>.
      </Card.Description>
    </Card.Header>
    <form onsubmit={submit}>
      <Card.Content class="space-y-3">
        <label class="block text-sm font-medium" for="apb-key">Authorization key</label>
        <Input
          id="apb-key"
          type="password"
          autocomplete="off"
          spellcheck={false}
          placeholder="apb_..."
          bind:value={key}
          disabled={busy}
        />
        {#if error}
          <p class="text-sm text-destructive" role="alert">{error}</p>
        {/if}
      </Card.Content>
      <Card.Footer>
        <Button type="submit" class="w-full" disabled={busy || key.trim() === ''}>
          {#if busy}<Spinner class="size-4" />{/if}
          Sign in
        </Button>
      </Card.Footer>
    </form>
  </Card.Root>
</main>
