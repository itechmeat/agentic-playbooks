<script lang="ts">
  // Shown while a route's page chunk is in flight.
  //
  // The chunk comes from localhost (in a release, from assets embedded in the
  // apb binary), so the usual wait is tens of milliseconds - short enough that a
  // spinner appearing and vanishing again is a flicker, not information. So the
  // indicator fades in only after 200ms: a normal navigation shows nothing at
  // all, and a load slow enough to notice says so. The delay is CSS, not a
  // timer, so there is no state to keep in sync.
  import { Spinner } from '$lib/components/ui/spinner'
</script>

<div class="flex min-h-0 flex-1 items-center justify-center p-8" role="status" aria-live="polite">
  <span class="sr-only">Loading page</span>
  <span class="delayed">
    <Spinner class="size-5 text-muted-foreground" />
  </span>
</div>

<style>
  .delayed {
    opacity: 0;
    animation: fade-in 120ms linear 200ms forwards;
  }
  @keyframes fade-in {
    to {
      opacity: 1;
    }
  }
</style>
