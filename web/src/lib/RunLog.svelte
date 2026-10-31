<script lang="ts">
  import { tick } from 'svelte'
  import type { LogLine } from './runview'

  // The run log: one line per journal event (time, actor, message), newest at
  // the bottom. It follows new lines while the reader is at the bottom and
  // stays put once they scroll up to read. `cited` marks the event a grade's
  // citation opened (each line carries its journal seq for that jump).
  let { lines, cited = null }: { lines: LogLine[]; cited?: number | null } = $props()

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
      if (el && follow) el.scrollTop = el.scrollHeight
    })
  })

  // A cited event is what the reader wants to see: stop following.
  $effect(() => {
    if (cited !== null) follow = false
  })

  // Lines that arrived while the tab was hidden could not scroll a box with
  // no size: catch up when it is shown again.
  $effect(() => {
    const box = el
    if (!box || typeof ResizeObserver === 'undefined') return
    const ro = new ResizeObserver(() => {
      if (follow && box.clientHeight > 0) box.scrollTop = box.scrollHeight
    })
    ro.observe(box)
    return () => ro.disconnect()
  })
</script>

<div bind:this={el} onscroll={onScroll} class="h-full min-h-0 overflow-auto pr-1" data-testid="run-log">
  {#if lines.length}
    <ol class="m-0 list-none p-0">
      {#each lines as l (l.seq)}
        <li
          class:cited={cited === l.seq}
          data-event-seq={l.seq}
          class="grid grid-cols-[64px_minmax(0,110px)_minmax(0,1fr)] gap-3 py-px text-xs sm:grid-cols-[72px_130px_minmax(0,1fr)]"
        >
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

<style>
  .cited {
    outline: 1px solid var(--c-violet);
    background: color-mix(in oklab, var(--c-violet) 15%, transparent);
  }
</style>
