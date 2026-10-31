<script lang="ts">
  import type { NodeCommits, RunGoal } from './api.gen'
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
  <section class="box flex flex-col gap-2 px-4 pt-4 pb-3 text-xs" data-testid="run-goal">
    <span class="box-cap">Goal</span>
      <p class="prose-text m-0 break-words">{goal.statement}</p>
      <p class="m-0 text-muted-foreground" data-testid="run-goal-summary">{goalSummary(goal)}</p>
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
        <div class="flex flex-col gap-1 border-t border-line pt-2" data-testid="run-goal-checklist">
          <span class="text-muted-foreground">To confirm by hand</span>
          <ul class="flex flex-col gap-1">
            {#each manual as c (c.index)}
              <li class="flex items-start gap-1.5">
                <span class="mt-0.5 inline-block size-3 shrink-0 border border-line-2" aria-hidden="true"></span>
                <span class="break-words">{c.description}</span>
              </li>
            {/each}
          </ul>
        </div>
      {/if}
  </section>
{/if}

{#if lines.length}
  <section class="box flex flex-col gap-1 px-4 pt-4 pb-3 text-xs" data-testid="run-commits">
    <span class="box-cap">Commits</span>
      {#each lines as c, i (i)}
        <div data-testid="run-commit-row" class="break-words">
          <span class="text-violet" title={c.sha}>{c.short}</span>
          <span> {c.subject}</span>
          <span class="text-muted-foreground"> · {c.node}</span>
        </div>
      {/each}
      {#if omitted > 0}
        <p class="text-muted-foreground">and {omitted} more</p>
      {/if}
  </section>
{/if}
