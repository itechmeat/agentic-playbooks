<script lang="ts">
  import type { AttemptModel } from './api.gen'
  import * as Card from '$lib/components/ui/card'
  import { Badge } from '$lib/components/ui/badge'

  // The model each attempt actually ran on (issue #193). In host mode the
  // profile's model is only a hint: the host picks its own, and a run whose
  // nodes all ran on another model than the profile names says so here.
  let { models }: { models?: AttemptModel[] | null } = $props()

  const rows = $derived(models ?? [])
  const mismatches = $derived(rows.filter((a) => a.mismatch).length)
</script>

{#if rows.length}
  <Card.Root data-testid="run-models">
    <Card.Header>
      <Card.Title class="text-sm">
        Models
        {#if mismatches}
          <Badge variant="outline" class="ml-1 border-warning/60" data-testid="run-models-mismatch">
            {mismatches} differ from the profile
          </Badge>
        {/if}
      </Card.Title>
    </Card.Header>
    <Card.Content class="flex flex-col gap-1 text-xs">
      {#each rows as a (`${a.node}#${a.attempt}`)}
        <div class="flex flex-wrap items-center gap-1.5" data-testid="run-model-row">
          <span class="font-mono">{a.node}</span>
          <span class="text-muted-foreground">#{a.attempt}</span>
          <span class="break-all">{a.model ?? 'not reported'}</span>
          <span class="text-muted-foreground">· {a.executed_by}</span>
          {#if a.mismatch}
            <Badge variant="outline" class="border-warning/60" title={`The profile names ${a.expected}`}>
              profile: {a.expected}
            </Badge>
          {/if}
        </div>
      {/each}
    </Card.Content>
  </Card.Root>
{/if}
