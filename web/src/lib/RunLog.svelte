<script lang="ts">
  import { tick } from 'svelte'
  import type { LogLine } from './runview'

  // The run log: one line per journal event (time, actor, message), newest at
  // the bottom. It follows new lines while the reader is at the bottom and
  // stays put once they scroll up to read.
  let { lines }: { lines: LogLine[] } = $props()

  let el = $state<HTMLElement | null>(null)
  let follow = true

  function onScroll() {
    if (!el) return
    follow = el.scrollHeight - el.scrollTop - el.clientHeight < 24
  }

  $effect(() => {
    void lines.length
    if (!el || !follow) return
    void tick().then(() => {
      if (el) el.scrollTop = el.scrollHeight
    })
  })
</script>

<div bind:this={el} onscroll={onScroll} class="h-full min-h-0 overflow-auto pr-1" data-testid="run-log">
  {#if lines.length}
    <ol class="m-0 list-none p-0">
      {#each lines as l (l.seq)}
        <li class="grid grid-cols-[64px_minmax(0,110px)_minmax(0,1fr)] gap-3 py-px text-xs sm:grid-cols-[72px_130px_minmax(0,1fr)]">
          <span class="text-muted-foreground tabular-nums">{l.time}</span>
          <span class="truncate" style:color={l.color}>{l.actor}</span>
          <span class="min-w-0 truncate text-fg-2" title={l.message}>{l.message}</span>
        </li>
      {/each}
    </ol>
  {:else}
    <p class="m-0 text-xs text-muted-foreground">no events yet</p>
  {/if}
</div>
