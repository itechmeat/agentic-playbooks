<script lang="ts">
  import type { NodeCommits, RunGoal } from './api.gen'
  import * as Card from '$lib/components/ui/card'
  import { Badge } from '$lib/components/ui/badge'
  import { commitLines, goalBadge, goalSummary, omittedCommits } from './runoutcome'

  // What the run produced beyond its outputs (0.23.0): the playbook's goal
  // with each criterion's result, manual ones as a checklist for a person,
  // and the commits its nodes made on a git tree. Each block renders only
  // when there is one, so the page of a run without them is unchanged.
  let { goal, commits }: { goal?: RunGoal | null; commits?: NodeCommits[] | null } = $props()

  const lines = $derived(commitLines(commits ?? undefined))
  const omitted = $derived(omittedCommits(commits ?? undefined))
  const checked = $derived(goal ? goal.criteria.filter((c) => c.check !== 'manual') : [])
  const manual = $derived(goal ? goal.criteria.filter((c) => c.check === 'manual') : [])
</script>

{#if goal}
  <Card.Root data-testid="run-goal">
    <Card.Header><Card.Title class="text-sm">Goal</Card.Title></Card.Header>
    <Card.Content class="flex flex-col gap-2 text-xs">
      <p class="break-words">{goal.statement}</p>
      <p class="font-mono text-muted-foreground" data-testid="run-goal-summary">{goalSummary(goal)}</p>
      {#if checked.length}
        <ul class="flex flex-col gap-1">
          {#each checked as c (c.index)}
            <li class="flex flex-col" data-testid="run-goal-criterion">
              <span class="flex items-start gap-1.5">
                <Badge variant={goalBadge(c.status)} class="shrink-0">{c.status}</Badge>
                <span class="min-w-0 break-words">{c.description} <span class="text-muted-foreground">· {c.check}</span></span>
              </span>
              {#if c.detail}<span class="break-words text-muted-foreground">{c.detail}</span>{/if}
            </li>
          {/each}
        </ul>
      {/if}
      {#if manual.length}
        <div class="flex flex-col gap-1 border-t border-border pt-2" data-testid="run-goal-checklist">
          <span class="text-muted-foreground">To confirm by hand</span>
          <ul class="flex flex-col gap-1">
            {#each manual as c (c.index)}
              <li class="flex items-start gap-1.5">
                <span class="mt-0.5 inline-block size-3 shrink-0 rounded-sm border border-border" aria-hidden="true"></span>
                <span class="break-words">{c.description}</span>
              </li>
            {/each}
          </ul>
        </div>
      {/if}
    </Card.Content>
  </Card.Root>
{/if}

{#if lines.length}
  <Card.Root data-testid="run-commits">
    <Card.Header><Card.Title class="text-sm">Commits</Card.Title></Card.Header>
    <Card.Content class="flex flex-col gap-1 text-xs">
      {#each lines as c, i (i)}
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
