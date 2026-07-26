<script lang="ts">
  // A route's page chunk never arrived. The realistic cause is not a flaky
  // network but an apb binary replaced under an open tab: the dashboard is
  // served from assets embedded in the binary, so after an update the hashed
  // chunk the running page asks for is simply gone. Without this the route
  // renders nothing at all and the tab looks broken with no way out.
  import { Button } from '$lib/components/ui/button'
  import TriangleAlert from '@lucide/svelte/icons/triangle-alert'

  let { error }: { error: unknown } = $props()
</script>

<div class="flex min-h-0 flex-1 flex-col items-center justify-center gap-3 p-8 text-center">
  <TriangleAlert class="size-6 text-warning" />
  <p class="text-sm font-medium">This page could not be loaded.</p>
  <p class="max-w-md break-words text-xs text-muted-foreground">{String(error)}</p>
  <p class="max-w-md text-xs text-muted-foreground">
    If apb was updated while this tab was open, reloading picks up the new assets.
  </p>
  <Button size="sm" onclick={() => location.reload()}>Reload</Button>
</div>
