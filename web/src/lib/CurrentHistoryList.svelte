<script lang="ts">
  // The moves of a playbook's `current`, newest first: from -> to, who,
  // when and the reason. A note says when the record has a gap (a line lost
  // or edited, or `current` moved by an older apb or by hand) and when a
  // move was interrupted and waits to be finished. Reasons render as plain
  // text.
  import type { History } from './api.gen'
  import { moveSummary, movedAt, whoMoved } from './currentmove'

  let { history = null }: { history?: History | null } = $props()
</script>

<div class="moves" data-testid="current-history">
  <span class="cap">moves of current</span>
  {#if !history || history.entries.length === 0}
    <p class="muted">no recorded moves</p>
  {:else}
    <ul>
      <!-- Keyed by place: a hand-edited record may repeat a line. -->
      {#each history.entries as e, i (i)}
        <li class={[e.kind === 'aborted' && 'aborted']} data-testid="current-history-entry">
          <span class="what">{moveSummary(e)}</span>
          <span class="muted">{movedAt(e.at_ms)}</span>
          {#if e.kind !== 'aborted'}
            <span class="who">{whoMoved(e.by)}</span>
          {/if}
          {#if e.reason}
            <span class="why">{e.reason}</span>
          {/if}
          {#if e.superseded_candidate}
            <span class="muted">candidate {e.superseded_candidate} superseded</span>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
  {#if history?.incomplete}
    <p class="warn" data-testid="current-history-incomplete">
      incomplete: the record has a gap (a line lost or edited, or current moved by an older apb or by hand)
    </p>
  {/if}
  {#if history?.pending}
    <p class="warn" data-testid="current-history-pending">
      pending: a move was interrupted; the next change of current finishes it
    </p>
  {/if}
</div>

<style>
  .moves {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 12px;
  }
  .cap {
    color: var(--muted-foreground);
    text-transform: lowercase;
  }
  ul {
    display: flex;
    flex-direction: column;
    max-height: 12rem;
    margin: 0;
    padding: 0;
    overflow: auto;
    list-style: none;
  }
  li {
    display: flex;
    flex-wrap: wrap;
    gap: 2px 8px;
    border-bottom: 1px solid var(--c-line);
    padding: 4px 0;
  }
  li:last-child {
    border-bottom: 0;
  }
  li.aborted .what {
    color: var(--muted-foreground);
  }
  .what {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .who {
    color: var(--c-teal);
  }
  .why {
    flex-basis: 100%;
    overflow-wrap: anywhere;
  }
  p {
    margin: 0;
  }
  .muted {
    color: var(--muted-foreground);
  }
  .warn {
    color: var(--c-amber);
  }
</style>
