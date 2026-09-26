<script lang="ts">
  // Shown once the server answers from a different build than the one this
  // tab loaded (see `$lib/buildcheck`): apb was reinstalled under the open
  // tab, and the old bundle may no longer match the API. Reloading loads the
  // new shell (served no-cache) and its new hashed assets.
  import { Button } from '$lib/components/ui/button'
  import RefreshCw from '@lucide/svelte/icons/refresh-cw'
  import { staleBuild, watchServerBuild } from '$lib/buildcheck'

  $effect(() => watchServerBuild())
</script>

{#if $staleBuild}
  <div
    role="alert"
    class="fixed inset-x-0 bottom-0 z-50 flex items-center justify-center gap-3 border-t bg-background/95 px-4 py-2 text-sm shadow-lg backdrop-blur"
  >
    <RefreshCw class="size-4" />
    <span>apb was updated on this machine. Reload to use the new version.</span>
    <Button size="sm" onclick={() => location.reload()}>Reload</Button>
  </div>
{/if}
