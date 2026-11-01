<script lang="ts">
  // What a start needs first: each start action's reason and how it is met
  // (approve a digest, provide a param, install an agent, set a variable,
  // run the doctor, or a manual step). Text only; a remedy never carries a
  // value or a token. Shared by the fit report and a refused start.
  import type { StartAction } from './api.gen'
  import { remedyLine } from './nextactions'

  let { actions = [] }: { actions?: StartAction[] | null } = $props()
</script>

{#if actions?.length}
  <ul class="start-actions" data-testid="start-actions">
    {#each actions as a, i (i)}
      {@const how = remedyLine(a)}
      <li data-testid="start-action" data-code={a.code}>
        <span class="reason">{a.reason}</span>
        {#if how}<span class="how" data-testid="start-action-remedy">{how}</span>{/if}
      </li>
    {/each}
  </ul>
{/if}

<style>
  /* Plain CSS over the app's variables. */
  .start-actions {
    margin: 0;
    padding-left: 16px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    overflow-wrap: anywhere;
  }
  li {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .how {
    color: var(--muted-foreground);
    font-size: 12px;
  }
</style>
