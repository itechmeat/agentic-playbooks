<script lang="ts">
  import { useSvelteFlow } from '@xyflow/svelte'

  // Place inside <SvelteFlow>: centres the canvas on `focus.nodes` each time
  // `focus.seq` moves (an issue picked, a deep link opened).
  let { focus = null }: { focus?: { nodes: string[]; seq: number } | null } = $props()

  const flow = useSvelteFlow()
  let done = -1

  $effect(() => {
    if (!focus || focus.seq === done || focus.nodes.length === 0) return
    done = focus.seq
    const reduce = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches
    void flow.fitView({
      nodes: focus.nodes.map((id) => ({ id })),
      maxZoom: 1,
      padding: 0.4,
      duration: reduce ? 0 : 250,
    })
  })
</script>
