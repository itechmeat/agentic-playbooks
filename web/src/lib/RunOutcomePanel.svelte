<script lang="ts">
  import type { NodeCommits } from './api.gen'
  import * as Card from '$lib/components/ui/card'
  import { commitLines, omittedCommits } from './runoutcome'

  // What the run produced beyond its outputs (0.23.0): the commits its nodes
  // made on a git tree. Renders nothing for a run without them, so such a
  // run's page is unchanged.
  let { commits }: { commits?: NodeCommits[] | null } = $props()

  const lines = $derived(commitLines(commits ?? undefined))
  const omitted = $derived(omittedCommits(commits ?? undefined))
</script>

{#if lines.length}
  <Card.Root data-testid="run-commits">
    <Card.Header><Card.Title class="text-sm">Commits</Card.Title></Card.Header>
    <Card.Content class="flex flex-col gap-1 text-xs">
      {#each lines as c (c.node + c.sha)}
        <div data-testid="run-commit-row" class="break-words">
          <span class="font-mono" title={c.sha}>{c.short}</span>
          <span> {c.subject}</span>
          <span class="text-muted-foreground"> · {c.node}</span>
        </div>
      {/each}
      {#if omitted > 0}
        <p class="text-muted-foreground">and {omitted} more</p>
      {/if}
    </Card.Content>
  </Card.Root>
{/if}
