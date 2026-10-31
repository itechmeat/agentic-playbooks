<script lang="ts">
  import type { Snippet } from 'svelte'
  import { cn } from '$lib/utils'

  // The heading row of a list page in the terminal look: an uppercase title,
  // muted facts beside it (project, counts), and the page actions on the
  // right. `PLAYBOOKS   project · x   3 playbooks   [filter] [new]`.
  let {
    title,
    meta,
    actions,
    class: className = '',
  }: { title: string; meta?: Snippet; actions?: Snippet; class?: string } = $props()
</script>

<div class={cn('mb-5 flex flex-wrap items-center gap-x-4 gap-y-2', className)}>
  <h1 class="hd">{title}</h1>
  {#if meta}
    <div class="flex min-w-0 flex-wrap items-baseline gap-x-4 gap-y-1 text-muted-foreground">
      {@render meta()}
    </div>
  {/if}
  {#if actions}
    <div class="ml-auto flex flex-wrap items-center gap-2">{@render actions()}</div>
  {/if}
</div>
