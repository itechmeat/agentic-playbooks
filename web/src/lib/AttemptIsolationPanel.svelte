<script lang="ts">
  // What each isolated attempt's isolation held and did not: a badge with
  // the level, whether a sandbox ran and the network, then every limit in
  // the words of the shared code table, and why a best_effort attempt ran
  // without a sandbox. Absent for a run without an isolated node.
  import type { AttemptIsolationRow } from './api.gen'
  import * as Card from '$lib/components/ui/card'
  import { isSandboxed, isolationBadge, isolationLimits, isolationReasonText } from './attemptisolation'

  let { rows = [] }: { rows?: AttemptIsolationRow[] | null } = $props()

  const list = $derived(rows ?? [])
</script>

{#if list.length}
  <Card.Root data-testid="run-isolation">
    <Card.Header>
      <Card.Title>Isolation</Card.Title>
    </Card.Header>
    <Card.Content>
      <div class="attempts">
        {#each list as r, i (i)}
          <div class="attempt" data-testid="run-isolation-row">
            <div class="head">
              <span class="node">{r.node}</span>
              {#if r.item}<span class="muted">{r.item}</span>{/if}
              <span class="muted">#{r.attempt}</span>
              <span class="badge" class:bare={!isSandboxed(r)} data-testid="run-isolation-badge">
                {isolationBadge(r)}
              </span>
            </div>
            {#if r.isolation.fallbacks?.length}
              <ul class="fallbacks">
                {#each r.isolation.fallbacks as f (f)}
                  {@const reason = isolationReasonText(f)}
                  <li title={reason.code}>No sandbox: {reason.text} <span class="code">{reason.code}</span></li>
                {/each}
              </ul>
            {/if}
            <details>
              <summary>{r.isolation.limits.length} limits</summary>
              <ul class="limits">
                {#each isolationLimits(r) as l (l.code)}
                  <li data-testid="run-isolation-limit">{l.text} <span class="code">{l.code}</span></li>
                {/each}
              </ul>
            </details>
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
</style>
