<script lang="ts" module>
  import type { MissingReason, ProjectedItem, ProjectedOutput, ResultRef } from './api.gen'

  /** The words for every reason a run output is missing. */
  export const MISSING_REASON_TEXTS: Record<MissingReason, string> = {
    not_run: 'its node did not run',
    not_succeeded: 'its node ran but never succeeded',
    field_absent: 'the output has no such field',
    not_json_object: 'the output is not a JSON object',
    truncated: 'the item output was too large to keep',
    item_not_passed: 'the item did not pass',
    definition_unknown: 'the definition in force is not established',
  }

  /** A value as one line: a string as it is, anything else as JSON. */
  export function shownValue(v: unknown): string {
    return typeof v === 'string' ? v : JSON.stringify(v)
  }

  /** `node #n`, and the item for a list item. */
  export function sourceLabel(s: ResultRef): string {
    const item = s.item ? ` · ${s.item.id}` : ''
    return `${s.execution.node} #${s.execution.ordinal}${item}`
  }

  export function stateText(o: Pick<ProjectedOutput | ProjectedItem, 'state' | 'reason'>): string {
    if (o.state === 'missing') return o.reason ? `missing: ${MISSING_REASON_TEXTS[o.reason]}` : 'missing'
    return o.state
  }
</script>

<script lang="ts">
  // The run outputs the playbook declares, projected from the journal: each
  // with its state (a value, null, or missing and why), its value and the
  // node execution it came from, which opens the journal event that
  // settled it. Absent for a playbook that declares none.
  import type { RunOutputs } from './api.gen'
  import * as Card from '$lib/components/ui/card'

  let {
    outputs = null,
    onCite,
  }: { outputs?: RunOutputs | null; onCite?: (seq: number) => void } = $props()

  const list = $derived(outputs?.outputs ?? [])
</script>

{#snippet source(s: ResultRef | undefined)}
  {#if s}
    <button
      type="button"
      class="source"
      data-testid="run-output-source"
      title="Open the journal event that settled it"
      onclick={() => onCite?.(s.settlement_seq)}
    >
      {sourceLabel(s)}
    </button>
  {/if}
{/snippet}

{#if outputs && list.length}
  <Card.Root data-testid="run-outputs">
    <Card.Header>
      <Card.Title>Outputs</Card.Title>
      <p class="note" data-testid="run-outputs-note">
        {outputs.final ? 'Final' : 'Not final: the run may still change them'}{outputs.definition === 'current'
          ? ' · read against the current definition'
          : ''}
      </p>
    </Card.Header>
    <Card.Content>
      <dl class="outputs">
        {#each list as o (o.name)}
          <div class="row" data-testid="run-output" data-state={o.state}>
            <dt>
              <span class="name">{o.name}</span>
              {#if o.pick !== 'last'}<span class="pick">{o.pick}</span>{/if}
              <span class="state" class:missing={o.state === 'missing'} data-testid="run-output-state">
                {stateText(o)}
              </span>
              {@render source(o.source)}
            </dt>
            <dd>
              {#if o.state === 'value' && o.values}
                <ol class="values">
                  {#each o.values as v, i (i)}
                    <li>
                      <span class="value" class:muted={v.state !== 'value'}>
                        {v.state === 'value' ? shownValue(v.value) : stateText(v)}
                      </span>
                      {@render source(v.source)}
                    </li>
                  {/each}
                </ol>
              {:else if o.state === 'value' && o.items}
                <ul class="values">
                  {#each o.items as it (it.id)}
                    <li>
                      <span class="item">{it.id}</span>
                      <span class="value" class:muted={it.state !== 'value'}>
                        {it.state === 'value' ? shownValue(it.value) : stateText(it)}
                      </span>
                    </li>
                  {/each}
                </ul>
              {:else if o.state === 'value'}
                <span class="value" data-testid="run-output-value">{shownValue(o.value)}</span>
              {/if}
            </dd>
          </div>
        {/each}
      </dl>
    </Card.Content>
  </Card.Root>
{/if}

<style>
  /* Plain CSS over the app's variables. */
  .note {
    margin: 0;
    color: var(--muted-foreground);
    font-size: 12px;
  }
  .outputs {
    display: flex;
    flex-direction: column;
    gap: 10px;
    margin: 0;
    font-size: 12px;
  }
  .row {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  dt {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 8px;
  }
  dd {
    margin: 0;
    min-width: 0;
  }
  .name,
  .item,
  .value,
  .source {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .pick {
    color: var(--muted-foreground);
  }
  .state {
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 1px 8px;
    overflow-wrap: anywhere;
  }
  .state.missing {
    border-color: var(--destructive);
    color: var(--destructive);
  }
  .source {
    border: none;
    background: none;
    padding: 0;
    color: var(--muted-foreground);
    font-size: 11px;
    text-decoration: underline;
    cursor: pointer;
  }
  .source:hover {
    color: var(--foreground);
  }
  .value {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .muted {
    color: var(--muted-foreground);
  }
  .values {
    margin: 0;
    padding-left: 18px;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .values li {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 8px;
  }
</style>
