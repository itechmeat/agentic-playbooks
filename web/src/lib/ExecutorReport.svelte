<script lang="ts">
  import type { ExecutorGroup } from './api.gen'
  import { CLOSE_REASON_NOT_RECORDED } from './consts.gen'
  import { groupHeadText, hintFollowText, stepLabelText, stepText, transitionText } from './playbookstats'

  // The executor report of one node inside the stats card: per executor
  // group its chain steps (attempts, success, cost, reported models, closes
  // by reason), the transitions between steps with the reason each was
  // recorded with (`not recorded` kept as such, never reclassified), and for
  // a host group how often the host followed the hint.
  let { node, groups }: { node: string; groups: ExecutorGroup[] } = $props()
</script>

{#if groups.length}
  <div class="report" data-testid="executor-report">
    <span class="node">{node}, per executor and step:</span>
    <!-- Two groups may print alike (shortened digests): key by position. -->
    {#each groups as g, i (i)}
      <section class="group" data-testid="executor-group">
        <div class="head" data-testid="executor-group-head">{groupHeadText(g)}</div>
        <ul class="steps">
          {#each g.steps as s, j (j)}
            <li data-testid="executor-step" class:idle={s.attempts === 0}>
              <span class="label">{stepLabelText(s)}</span>
              <span>{stepText(s)}</span>
            </li>
          {/each}
        </ul>
        {#if g.transitions.length}
          <ul class="moves">
            {#each g.transitions as t, j (j)}
              <li data-testid="executor-transition" class:unrecorded={t.reason === CLOSE_REASON_NOT_RECORDED}>
                {transitionText(t)}
              </li>
            {/each}
          </ul>
        {/if}
        {#if g.hint_follow}
          <div class="hints" data-testid="executor-hints">{hintFollowText(g.hint_follow)}</div>
        {/if}
      </section>
    {/each}
  </div>
{/if}

<style>
  .report {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    overflow-wrap: anywhere;
  }
  .node {
    color: var(--muted-foreground);
  }
  .group {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    padding-left: 0.75rem;
    border-left: 2px solid var(--c-line-2);
  }
  .head {
    color: var(--c-fg-2);
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .steps,
  .moves {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
  }
  .steps li {
    display: flex;
    flex-wrap: wrap;
    column-gap: 0.5rem;
  }
  .label {
    color: var(--c-fg-2);
    font-weight: 600;
  }
  .idle,
  .unrecorded,
  .hints {
    color: var(--muted-foreground);
  }
  .unrecorded {
    font-style: italic;
  }
</style>
