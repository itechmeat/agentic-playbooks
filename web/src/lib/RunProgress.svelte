<script lang="ts">
  import { untrack } from 'svelte'
  import type { ProgressSummary } from './types'
  import { nextDisplay, showBar, waitingKindText, type ProgressDisplayState } from './progress'

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
  const waitingText = $derived(waitingKindText(progress?.waiting_kind ?? null))
</script>

{#if visible}
  <div class="flex items-center gap-2">
    <div
      class="hatch-bar w-full"
      role="progressbar"
      aria-valuenow={shown}
      aria-valuemin="0"
      aria-valuemax="100"
      aria-label="run progress"
      style:--k={waiting ? 'var(--c-amber)' : 'var(--c-blue)'}
    >
      <i class="transition-[width] duration-300" style:width="{shown}%"></i>
    </div>
    <span class="shrink-0 text-xs text-fg-2 tabular-nums">{shown}%</span>
    {#if waiting}
      <span class="tag shrink-0 text-warning">{waitingText}</span>
    {:else if progress?.label}
      <span class="min-w-0 truncate text-xs text-muted-foreground" title={progress.label}>{progress.label}</span>
    {/if}
  </div>
{/if}
