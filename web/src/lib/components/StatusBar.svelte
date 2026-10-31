<script lang="ts" module>
  /** Tone of a status bar value; maps onto the semantic palette. */
  export type StatusTone =
    | 'default'
    | 'muted'
    | 'info'
    | 'success'
    | 'warning'
    | 'danger'
    | 'violet'
    | 'orange'

  /** One `label [value]` pair of the status bar. */
  export interface StatusItem {
    label: string
    value: string | number
    tone?: StatusTone
    /** Optional hover text with the full value. */
    title?: string
  }
</script>

<script lang="ts">
  import type { Snippet } from 'svelte'
  import { cn } from '$lib/utils'

  // The bottom bar of a page, like a terminal status line:
  // `playbook [apb-task@1.0.0]   validate [ok]   ...   <right slot>`.
  let {
    items = [],
    right,
    class: className = '',
  }: { items?: StatusItem[]; right?: Snippet; class?: string } = $props()

  const toneClass: Record<StatusTone, string> = {
    default: 'text-foreground',
    muted: 'text-muted-foreground',
    info: 'text-info',
    success: 'text-success',
    warning: 'text-warning',
    danger: 'text-destructive',
    violet: 'text-violet',
    orange: 'text-orange',
  }
</script>

<footer
  data-slot="status-bar"
  class={cn(
    'flex shrink-0 items-center gap-x-5 gap-y-0.5 overflow-hidden border-t border-line bg-panel px-4 py-1.5 text-xs text-muted-foreground',
    className,
  )}
>
  <div class="flex min-w-0 flex-wrap items-center gap-x-5 gap-y-0.5">
    {#each items as it, i (i)}
      <span class="whitespace-nowrap" title={it.title}>
        {it.label}
        <b class={cn('font-medium', toneClass[it.tone ?? 'default'])}>[{it.value}]</b>
      </span>
    {/each}
  </div>
  {#if right}
    <div class="ml-auto flex shrink-0 items-center gap-3">{@render right()}</div>
  {/if}
</footer>
