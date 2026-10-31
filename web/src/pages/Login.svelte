<script lang="ts">
  // The whole app when the server says a credential is required and this
  // browser does not have one. One field, one button: the operator pastes the
  // key from `apb server key issue`, the server swaps it for an HttpOnly
  // cookie, and the key is never stored on this side.
  import { login } from '$lib/auth.svelte'
  import { Button } from '$lib/components/ui/button'
  import { Input } from '$lib/components/ui/input'
  import { Spinner } from '$lib/components/ui/spinner'
  import ApbLogo from '$lib/components/ApbLogo.svelte'

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
    key = ''
    if (result.ok) return
    error = result.message ?? 'Sign in failed.'
  }
</script>

<main class="flex min-h-screen flex-col items-center justify-center gap-6 p-4">
  <div class="flex items-center gap-2.5 text-foreground">
    <ApbLogo class="size-8" />
    <span class="text-xl font-bold tracking-[0.14em]">apb</span>
  </div>
  <section class="box w-full max-w-sm px-5 pt-6 pb-5">
    <h1 class="box-cap">Sign in to apb</h1>
    <p class="prose-text mb-5 text-xs leading-relaxed text-muted-foreground">
      This dashboard requires an authorization key. Create one on the server with
      <code class="bg-muted px-1 py-0.5 font-mono text-xs">apb server key issue</code>.
    </p>
    <form onsubmit={submit} class="flex flex-col gap-3">
      <label class="block text-xs text-fg-2" for="apb-key">Authorization key</label>
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
        <p class="text-xs text-destructive" role="alert">{error}</p>
      {/if}
      <Button type="submit" class="mt-2 w-full" disabled={busy || key.trim() === ''}>
        {#if busy}<Spinner class="size-4" />{/if}
        Sign in
      </Button>
    </form>
  </section>
</main>
