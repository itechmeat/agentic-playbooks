<script lang="ts">
  // How the run's drive stands, under the run header: the line of a
  // released run (its driver let go while a deferred retry waits, and a
  // waker resumes it; not a dead driver, so amber, no alarm) and the apb
  // binaries that drove it, from the versions its journal recorded.
  import type { Released } from './api.gen'
  import { releasedLine } from './retrywait'

  let { released, drivenBy, fmt }: { released?: Released | null; drivenBy?: string | null; fmt?: (ms: number) => string } =
    $props()
</script>

{#if released}
  <p class="released-line" data-testid="run-released">{releasedLine(released, fmt)}</p>
{/if}
{#if drivenBy}
  <p class="driven-by" data-testid="run-driven-by">{drivenBy}</p>
{/if}

<style>
  .released-line {
    margin: 4px 0 0;
    max-width: 100%;
    color: var(--c-amber);
    font-size: 11px;
    overflow-wrap: anywhere;
  }
  .driven-by {
    margin: 0;
    color: var(--c-muted);
    font-size: 11px;
    overflow-wrap: anywhere;
  }
</style>
