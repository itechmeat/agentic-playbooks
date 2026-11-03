<script lang="ts">
  // What each isolated attempt's isolation held and did not: a badge with
  // the level, whether a sandbox ran and the network, then every limit in
  // the words of the shared code table, and why a best_effort attempt ran
  // without a sandbox. A read-only node's attempt also shows its check:
  // the tier, what it covered and the paths that failed it (names only).
  // Absent for a run with neither.
  import type { AttemptIsolationRow, ReadOnlyRow } from './api.gen'
  import * as Card from '$lib/components/ui/card'
  import {
    attemptEntries,
    isSandboxed,
    isolationBadge,
    isolationLimits,
    isolationReasonText,
    readOnlyBadge,
    readOnlyText,
    readOnlyWarns,
  } from './attemptisolation'

  let { rows = [], readOnly = [] }: { rows?: AttemptIsolationRow[] | null; readOnly?: ReadOnlyRow[] | null } = $props()

  const list = $derived(attemptEntries(rows ?? [], readOnly ?? []))
</script>

{#if list.length}
  <Card.Root data-testid="run-isolation">
    <Card.Header>
      <Card.Title>Isolation</Card.Title>
    </Card.Header>
    <Card.Content>
      <div class="attempts">
        {#each list as e, i (i)}
          <div class="attempt" data-testid="run-isolation-row">
            <div class="head">
              <span class="node">{e.node}</span>
              {#if e.item}<span class="muted">{e.item}</span>{/if}
              <span class="muted">#{e.attempt}</span>
              {#if e.isolation}
                <span class="badge" class:bare={!isSandboxed(e.isolation)} data-testid="run-isolation-badge">
                  {isolationBadge(e.isolation)}
                </span>
              {/if}
              {#if e.readOnly}
                <span class="badge" class:bare={readOnlyWarns(e.readOnly)} data-testid="run-read-only-badge">
                  {readOnlyBadge(e.readOnly)}
                </span>
              {/if}
            </div>
            {#if e.readOnly}
              <p class="muted ro-text">{readOnlyText(e.readOnly)}</p>
              {#if e.readOnly.violated}
                {@const v = e.readOnly.violated}
                <div class="violated" data-testid="run-read-only-violated">
                  The node changed the tree <span class="code">read_only_dirty</span>
                  <ul>
                    {#each v.changes as c, j (j)}
                      <li data-testid="run-read-only-change"><span class="code path">{c.path}</span> {c.change}</li>
                    {/each}
                    {#if v.truncated}<li>{v.truncated} more</li>{/if}
                  </ul>
                  {#if v.evidence}<div class="muted">Evidence <span class="code">{v.evidence}</span></div>{/if}
                </div>
              {/if}
            {/if}
            {#if e.isolation}
              {@const iso = e.isolation}
              {#if iso.isolation.fallbacks?.length}
                <ul class="fallbacks">
                  {#each iso.isolation.fallbacks as f (f)}
                    {@const reason = isolationReasonText(f)}
                    <li title={reason.code}>No sandbox: {reason.text} <span class="code">{reason.code}</span></li>
                  {/each}
                </ul>
              {/if}
              <details>
                <summary>{iso.isolation.limits.length} limits</summary>
                <ul class="limits">
                  {#each isolationLimits(iso) as l (l.code)}
                    <li data-testid="run-isolation-limit">{l.text} <span class="code">{l.code}</span></li>
                  {/each}
                </ul>
              </details>
            {/if}
          </div>
        {/each}
      </div>
    </Card.Content>
  </Card.Root>
{/if}

<style>
  /* Plain CSS over the app's variables. */
  .attempts {
    display: flex;
    flex-direction: column;
    gap: 10px;
    font-size: 12px;
  }
  .attempt {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 8px;
  }
  .node,
  .code {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .code {
    color: var(--muted-foreground);
    font-size: 11px;
  }
  .badge {
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 1px 8px;
    overflow-wrap: anywhere;
  }
  .badge.bare {
    border-color: var(--destructive);
    color: var(--destructive);
  }
  .muted {
    color: var(--muted-foreground);
  }
  summary {
    cursor: pointer;
    color: var(--muted-foreground);
  }
  ul {
    margin: 4px 0 0;
    padding-left: 18px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    overflow-wrap: anywhere;
  }
  .fallbacks {
    color: var(--destructive);
    margin: 0;
  }
  .ro-text {
    margin: 0;
  }
  .violated {
    color: var(--destructive);
    overflow-wrap: anywhere;
  }
  .violated ul {
    margin: 2px 0;
  }
  .path {
    color: inherit;
  }
</style>
