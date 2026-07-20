<script lang="ts">
  import { untrack } from 'svelte'
  import type { ProgressSummary } from './types'
  import { nextDisplay, showBar, type ProgressDisplayState } from './progress'
  import { Badge } from '$lib/components/ui/badge'

  let { progress, status, runKey }: {
    progress?: ProgressSummary | null
    status: string
    runKey: string
  } = $props()

  let state = $state<ProgressDisplayState | null>(null)

  $effect(() => {
    if (!progress) return
    const prev = untrack(() => state)
    // nextDisplay resets on run identity or plan_key change, and otherwise
    // clamps to a monotonic maximum; nothing downstream needs its own clamp.
    state = nextDisplay(prev, runKey, progress)
  })

  const shown = $derived(state?.shown ?? 0)
  const visible = $derived(showBar(status) && !!progress)
  const waiting = $derived(progress?.waiting_on ?? null)
  const waitingText = $derived(
    progress?.waiting_kind === 'human_review'
      ? 'waiting for decision'
      : progress?.waiting_kind === 'wait'
        ? 'waiting for event'
        : progress?.waiting_kind === 'question'
          ? 'waiting for answer'
          : 'waiting',
  )
</script>

{#if visible}
  <div class="flex items-center gap-2">
    <div
      class="h-1.5 w-full overflow-hidden rounded-full bg-muted"
      role="progressbar"
      aria-valuenow={shown}
      aria-valuemin="0"
      aria-valuemax="100"
      aria-label="run progress"
    >
      <div
        class="h-full rounded-full bg-chart-1 transition-[width] duration-300"
        style:width="{shown}%"
      ></div>
    </div>
    <span class="shrink-0 font-mono text-xs text-muted-foreground">{shown}%</span>
    {#if waiting}
      <Badge variant="secondary" class="shrink-0">{waitingText}</Badge>
    {:else if progress?.label}
      <span class="shrink-0 text-xs text-muted-foreground">{progress.label}</span>
    {/if}
  </div>
{/if}
