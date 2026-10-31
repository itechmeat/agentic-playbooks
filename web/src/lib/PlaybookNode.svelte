<script lang="ts">
  import { Handle, Position } from '@xyflow/svelte'
  import { cn } from '$lib/utils'
  import { shortModel } from './format'
  import OctagonX from '@lucide/svelte/icons/octagon-x'
  import CornerDownRight from '@lucide/svelte/icons/corner-down-right'
  import { accentVar, isTerminalNodeKind, nodeKindColor } from './profilecolor'
  import type { FlowNodeData } from './graph'

  // One card on the graph, shared by the playbook view, the editor and the run
  // view. The data contract (playbook fields from `toFlow`, optional run
  // fields from the run view) is `FlowNodeData` in graph.ts.
  let {
    id,
    data,
    selected = false,
  }: {
    id: string
    data: FlowNodeData
    selected?: boolean
  } = $props()

  // start - entry point, must have no incoming edge;
  // finish - terminal, must have no outgoing edge.
  const hasTarget = $derived(data.kind !== 'start')
  // One anonymous dot at the bottom, which is also what an editor drags a new
  // edge from. A node with named exits draws one dot per exit instead.
  const hasSource = $derived(data.kind !== 'finish' && !data.exits)

  // Start and finish are small muted pills, unless they carry named exits,
  // which need the card's caption row.
  const pill = $derived(isTerminalNodeKind(data.kind) && !data.exits)

  // The node color: the profile's for agent tasks, green for a judge, amber
  // for a human review, muted for start and finish.
  const k = $derived(nodeKindColor(data.kind, data.profile))

  const exitTone = { success: 'text-green', failure: 'text-red', default: 'text-muted-foreground' } as const

  const modeBadge = $derived.by(() => {
    if (!data.exits) return null
    const n = data.exits.list.length
    return data.exits.mode === 'all'
      ? { text: `all ${n}`, title: `All ${n} exits are taken: the branches run in parallel.` }
      : {
          text: `1 of ${n}`,
          title: `Exactly one of these ${n} exits is taken: the first whose condition matches, in the numbered order.`,
        }
  })

  // Second line: who executes the node, in the node color.
  const kindLine = $derived.by(() => {
    switch (data.kind) {
      case 'agent_task':
        if (!data.profile) return 'agent task'
        return data.model ? `${data.profile} · ${shortModel(data.model)}` : data.profile
      case 'judge':
        return 'judge · decides'
      case 'human_review':
        return 'human review'
      case 'playbook':
        return data.subPlaybook ? `↳ ${data.subPlaybook}` : '↳ sub-playbook'
      default:
        return data.kind.replaceAll('_', ' ')
    }
  })

  // Border color by run status, over the node color. Running keeps the node
  // color and glows instead; anything without a verdict keeps the node color.
  const statusBorder = $derived.by(() => {
    switch (data.status) {
      case 'succeeded':
        return accentVar('green')
      case 'failed':
      case 'timed_out':
        return accentVar('red')
      case 'interrupted':
      // A node whose attempt process is provably gone while the node never
      // reported a verdict (issue #85.4). It needs at least the attention an
      // interrupted node gets: without an arm here the most actionable state
      // on the canvas would render as the neutral default.
      case 'lost':
      case 'unknown':
        return accentVar('amber')
      default:
        return null
    }
  })

  const dimmed = $derived(
    data.status === 'pending' || data.status === 'ready' || data.status === 'skipped' || data.status === 'cancelled',
  )
  const skipped = $derived(data.status === 'skipped' || data.status === 'cancelled')
  const running = $derived(data.status === 'running')
  const progress = $derived(
    typeof data.progress === 'number' ? Math.max(0, Math.min(100, data.progress)) : null,
  )

  // Third line: the run verdict when there is one, else the title.
  const tail = (...parts: (string | number | undefined | null)[]) =>
    parts.filter((p) => p !== undefined && p !== null && p !== '').join(' · ')
  const statusLine = $derived.by(() => {
    switch (data.status) {
      case 'succeeded':
        return { tone: 'text-green', text: `✓ ${tail('done', data.duration, data.note)}` }
      case 'failed':
        return { tone: 'text-red', text: `✕ ${tail('failure', data.duration, data.note)}` }
      case 'timed_out':
        return { tone: 'text-red', text: `✕ ${tail('timed out', data.duration, data.note)}` }
      case 'interrupted':
      case 'lost':
      case 'unknown':
        return { tone: 'text-amber', text: `! ${tail(data.status, data.note)}` }
      case 'skipped':
      case 'cancelled':
        return { tone: 'text-muted-foreground', text: tail(data.status, data.note) }
      case 'pending':
      case 'ready':
        return { tone: 'text-muted-foreground', text: tail(data.status, data.note) }
      default:
        return null
    }
  })
  const subtitle = $derived(data.title && data.title !== id ? data.title : '')
</script>

{#if pill}
  <div
    class={cn(
      'flex h-[30px] min-w-[110px] items-center justify-center gap-1.5 rounded-full border bg-panel px-4 text-[11px] tracking-[0.06em] text-muted-foreground transition-[opacity,box-shadow]',
      dimmed && 'opacity-40',
      (selected || running) && 'glow',
    )}
    style:--k={k}
    style:border-color={statusBorder ?? k}
    title={data.title !== id ? data.title : undefined}
  >
    {#if hasTarget}<Handle type="target" position={Position.Top} />{/if}
    {#if running}<span class="spin text-blue"></span>{/if}
    {#if data.status === 'succeeded'}<span class="text-green" aria-hidden="true">✓</span>{/if}
    <span>{id}</span>
    {#if data.status && data.status !== 'succeeded' && data.status !== 'pending'}
      <span class="sr-only">{data.status}</span>
    {/if}
    {#if hasSource}<Handle type="source" position={Position.Bottom} />{/if}
  </div>
{:else}
  <div
    class={cn(
      'w-[210px] border bg-panel px-2.5 py-[7px] text-xs leading-[1.45] text-foreground transition-[opacity,box-shadow]',
      data.kind === 'condition' && 'border-dashed',
      skipped && 'border-dashed opacity-30',
      dimmed && !skipped && 'opacity-40',
      (selected || running) && 'glow',
    )}
    style:--k={k}
    style:border-color={statusBorder ?? k}
  >
    {#if hasTarget}<Handle type="target" position={Position.Top} />{/if}

    <div class="flex items-baseline justify-between gap-2">
      <b class="min-w-0 truncate font-semibold">{id}</b>
      <span class="flex shrink-0 items-baseline gap-1.5 text-[11px] text-muted-foreground">
        {#if data.list}
          <span class="list-badge" title="Runs once per item: items processed of the list" data-testid="node-list-badge">
            {data.list}
          </span>
        {/if}
        {#if modeBadge}
          <!-- Several lines leaving one node mean two opposite things - all of
               them run, or exactly one does - and the graph alone cannot say
               which. -->
          <span class="tag px-1 text-[10px] leading-[14px]" title={modeBadge.title}>{modeBadge.text}</span>
        {/if}
        {#if data.expected}<span title="expected time of one execution">~{data.expected}</span>{/if}
      </span>
    </div>

    <div
      class="truncate text-[11px]"
      style:color={k}
      title={data.kind === 'agent_task' && data.profile && data.model ? `${data.profile} · ${data.model}` : kindLine}
    >
      {kindLine}
    </div>

    {#if running}
      {#if progress !== null}
        <div class="mt-1 grid grid-cols-[1fr_auto] items-center gap-2 text-[11px] text-fg-2">
          <div class="hatch-bar"><i style:width={`${progress}%`}></i></div>
          <span>{Math.round(progress)}%</span>
        </div>
      {/if}
      <div class="mt-0.5 truncate text-[11px]" style:color={k}>
        <span class="spin"></span>{tail('running', data.attempt ? `attempt ${data.attempt}` : undefined, data.note)}
      </div>
    {:else if statusLine}
      <div class={cn('mt-0.5 truncate text-[11px]', statusLine.tone)} title={statusLine.text}>{statusLine.text}</div>
    {:else if subtitle}
      <div class="mt-0.5 truncate text-[11px] text-muted-foreground" title={subtitle}>{subtitle}</div>
    {/if}

    {#if data.cached || data.failure}
      <div class="mt-0.5 flex items-center gap-1.5">
        {#if data.cached}
          <span class="tag px-1 text-[10px] leading-[14px] text-teal">cached</span>
        {/if}
        {#if data.failure}
          <!-- The failure branch that is not drawn: without this the graph reads
               as if nobody thought about what happens when this node fails. -->
          <span
            class="flex min-w-0 items-center gap-0.5 truncate text-[10px] leading-none text-red"
            title={data.failure.kind === 'stop' ? 'Failure ends the run' : `Failure goes to ${data.failure.node}`}
          >
            {#if data.failure.kind === 'stop'}
              <OctagonX class="size-3 shrink-0" />
              stop on failure
            {:else}
              <CornerDownRight class="size-3 shrink-0" />
              on failure: {data.failure.node}
            {/if}
          </span>
        {/if}
      </div>
    {/if}

    {#if data.exits}
      <!-- One dot per exit, each under its own caption, so no two lines share a
           point and every line says on the node itself why it is taken. -->
      <div class="-mx-1 mt-1.5 flex items-start border-t border-line pt-1">
        {#each data.exits.list as exit, i (exit.id)}
          <span
            class={cn('flex-1 px-0.5 text-center text-[9px] leading-tight', exitTone[exit.tone])}
            title={exit.title}
          >
            {exit.label}
          </span>
          <Handle
            type="source"
            id={exit.id}
            position={Position.Bottom}
            style={`left: ${(((i + 0.5) / data.exits.list.length) * 100).toFixed(2)}%`}
          />
        {/each}
      </div>
    {/if}
    {#if hasSource}<Handle type="source" position={Position.Bottom} />{/if}
  </div>
{/if}

<style>
  .list-badge {
    padding: 0 4px;
    border: 1px solid var(--c-line-2);
    color: var(--c-fg-2);
    font-size: 10px;
    line-height: 14px;
  }
</style>
