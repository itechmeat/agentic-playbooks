<script lang="ts">
  // How the run's drive stands, under the run header: the line of the
  // run's `run.previous_success` record, the line of a released run (its
  // driver let go while a deferred retry waits, and a waker resumes it;
  // not a dead driver, so amber, no alarm) and the apb binaries that
  // drove it, from the versions its journal recorded. A run that processes
  // external data says so: it keeps nothing for learning.
  import type { Released, PreviousSuccessView, UntrustedNode } from './api.gen'
  import { releasedLine } from './retrywait'
  import { previousSuccessLine, untrustedLine } from './previoussuccess'

  let {
    released,
    drivenBy,
    previousSuccess,
    untrusted,
    externalData = false,
    fmt,
  }: {
    released?: Released | null
    drivenBy?: string | null
    previousSuccess?: PreviousSuccessView | null
    untrusted?: UntrustedNode[]
    externalData?: boolean
    fmt?: (ms: number) => string
  } = $props()
</script>

{#if externalData}
  <p class="previous-success" data-testid="run-external-data">
    External data: this run keeps nothing for learning.
  </p>
{/if}
{#if previousSuccess}
  <p class="previous-success" data-testid="run-previous-success">
    {previousSuccessLine(previousSuccess)}
  </p>
{/if}
{#if untrusted && untrusted.length > 0}
  {#each untrusted as u}
    <p class="previous-success" data-testid="run-untrusted">{untrustedLine(u)}</p>
  {/each}
{/if}
{#if released}
  <p class="released-line" data-testid="run-released">{releasedLine(released, fmt)}</p>
{/if}
{#if drivenBy}
  <p class="driven-by" data-testid="run-driven-by">{drivenBy}</p>
{/if}

<style>
  .previous-success {
    margin: 4px 0 0;
    max-width: 100%;
    color: var(--c-muted);
    font-size: 11px;
    overflow-wrap: anywhere;
  }
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
