<script lang="ts">
  import { useNodesInitialized, useSvelteFlow } from '@xyflow/svelte'

  // Place inside <SvelteFlow> (without its `fitView`). A tall playbook fitted
  // whole shrinks every card to an unreadable size, so this fits the top of
  // the graph instead (the first ranks, at most 1:1) and leaves the rest to
  // panning; a short graph is fitted whole. It fits once per `key` (a
  // playbook or run id), so a reload of the same graph keeps the person's
  // pan and zoom.
  let {
    nodes,
    key = '',
    span = 640,
  }: {
    nodes: { id: string; position: { x: number; y: number } }[]
    key?: string
    /** Height of the graph slice, in layout pixels, fitted on a tall graph. */
    span?: number
  } = $props()

  const flow = useSvelteFlow()
  const initialized = useNodesInitialized()
  let fittedKey: string | null = null

  $effect(() => {
    if (!initialized.current || nodes.length === 0 || fittedKey === key) return
    fittedKey = key
    const top = Math.min(...nodes.map((n) => n.position.y))
    const bottom = Math.max(...nodes.map((n) => n.position.y))
    const slice =
      bottom - top > span * 1.5 ? nodes.filter((n) => n.position.y <= top + span) : undefined
    void flow.fitView({
      maxZoom: 1,
      padding: 0.08,
      ...(slice ? { nodes: slice.map((n) => ({ id: n.id })) } : {}),
    })
  })
</script>
