<script lang="ts">
  // How the run's drive stands, under the run header: the line of the
  // run's `run.previous_success` record, the line of a released run (its
  // driver let go while a deferred retry waits, and a waker resumes it;
  // not a dead driver, so amber, no alarm) and the apb binaries that
  // drove it, from the versions its journal recorded. A run that processes
  // external data says so: it keeps nothing for learning. A run a run step
  // started names its starter, and a run whose steps started runs lists
  // them (links within this workspace only).
  import type {
    ConcurrencyView,
    Released,
    PreviousSuccessView,
    RunDeadlineView,
    StartedBy,
    StartedRunView,
    UntrustedNode,
  } from './api.gen'
  import { releasedLine } from './retrywait'
  import { previousSuccessLine, untrustedLine } from './previoussuccess'
  import { deadlineLine } from './rundeadline'
  import { TRUNCATED_NOTE, runHref, startedByLine, startedRunLabel, starterHref } from './startedruns'

  // The one wording of the key line, shared with the report.
  function concurrencyLine(v: ConcurrencyView): string {
    return v.waiting_for
      ? `Key: ${v.display} (${v.on_busy}), waiting for run ${v.waiting_for}`
      : `Key: ${v.display} (${v.on_busy})`
  }

  let {
    released,
    drivenBy,
    concurrency,
    previousSuccess,
    untrusted,
    externalData = false,
    deadline,
    startedBy,
    startedRuns,
    startedRunsTruncated = false,
    startKey,
    workspace = '',
    fmt,
  }: {
    released?: Released | null
    drivenBy?: string | null
    concurrency?: ConcurrencyView | null
    previousSuccess?: PreviousSuccessView | null
    untrusted?: UntrustedNode[]
    externalData?: boolean
    deadline?: RunDeadlineView | null
    startedBy?: StartedBy | null
    startedRuns?: StartedRunView[]
    startedRunsTruncated?: boolean
    startKey?: string | null
    workspace?: string
    fmt?: (ms: number) => string
  } = $props()
</script>

{#if externalData}
  <p class="previous-success" data-testid="run-external-data">
    External data: this run keeps nothing for learning.
  </p>
{/if}
{#if deadline}
  <p class="previous-success" data-testid="run-deadline">{deadlineLine(deadline)}</p>
{/if}
{#if concurrency}
  <p class="previous-success" data-testid="run-concurrency">
    {concurrencyLine(concurrency)}
  </p>
{/if}
{#if startKey}
  <p class="previous-success" data-testid="run-start-key">Started under a start key ({startKey}).</p>
{/if}
{#if startedBy}
  {@const href = starterHref(startedBy, workspace)}
  <p class="previous-success" data-testid="run-started-by">
    {#if href}
      <a {href}>{startedByLine(startedBy, workspace)}</a>
    {:else}
      {startedByLine(startedBy, workspace)}
    {/if}
  </p>
{/if}
{#if (startedRuns && startedRuns.length > 0) || startedRunsTruncated}
  <p class="previous-success" data-testid="run-started-runs">
    Started runs in this project:
    {#each startedRuns ?? [] as r, i}
      {#if i > 0},{/if}
      <a href={runHref(workspace, r.run_id)}>{startedRunLabel(r)}</a>
    {/each}
    {#if startedRunsTruncated}<span data-testid="run-started-runs-truncated">({TRUNCATED_NOTE})</span>{/if}
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
