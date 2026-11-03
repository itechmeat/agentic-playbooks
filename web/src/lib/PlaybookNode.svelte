<script lang="ts">
  import { Handle, Position } from '@xyflow/svelte'
  import { cn } from '$lib/utils'
  import { shortModel } from './format'
  import OctagonX from '@lucide/svelte/icons/octagon-x'
  import CornerDownRight from '@lucide/svelte/icons/corner-down-right'
  import { accentVar, isTerminalNodeKind, nodeKindColor } from './profilecolor'
  import { hasExitRow, PLAIN_HANDLE, type FlowNodeData } from './graph'

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
  // The plain dot at the bottom: unconditional edges leave from it, and it is
  // what an editor drags a new edge from, so every node but a finish has it.
  // A conditional edge or a fallback has its own dot under its caption.
  const hasSource = $derived(data.kind !== 'finish' || (data.exits?.parallel ?? 0) > 0)
  const hollow = $derived(data.hollow ?? [])
  const exitRow = $derived(hasExitRow(data.exits) || hollow.length > 0)
  // In the caption row the plain dot takes the first slot, the hollow judge
  // exits (values without an edge) the last ones.
  const named = $derived(data.exits?.list.length ?? 0)
  const slots = $derived(named + hollow.length + (hasSource ? 1 : 0))
  // Too many exits for a label each: the hollow ones are listed in one line
  // under the row instead, each dot keeping its own tooltip.
  const crowded = $derived(slots > 6 && hollow.length > 0)
  const plainLeft = $derived(exitRow && hasSource ? `left: ${((0.5 / slots) * 100).toFixed(2)}%` : undefined)

  // Start and finish are small muted pills, unless they carry named exits,
  // which need the card's caption row.
  const pill = $derived(isTerminalNodeKind(data.kind) && !exitRow)

  // The node color: the profile's for agent tasks, green for a judge, amber
  // for a human review, muted for start and finish.
  const k = $derived(nodeKindColor(data.kind, data.profile))

  const exitTone = { success: 'text-green', failure: 'text-red', default: 'text-muted-foreground' } as const

  const modeBadge = $derived.by(() => {
    if (!data.exits) return null
    const n = data.exits.list.length + data.exits.parallel
    if (n < 2) return null
    return data.exits.mode === 'all'
      ? { text: `all ${n}`, title: `All ${n} exits are taken: the branches run in parallel.` }
      : {
          text: `1 of ${n}`,
          title: `Exactly one of these ${n} exits is taken: the first whose condition matches, in the numbered order.`,
        }
  })

  // A model not reported yet while the attempt runs is still to come.
  const unreported = $derived(data.status === 'running' ? 'awaiting report' : 'not reported')

  // Second line: who executes the node, in the node color.
  const kindLine = $derived.by(() => {
    switch (data.kind) {
      case 'agent_task':
        if (!data.profile) return 'agent task'
        if (data.model) return `${data.profile} · ${shortModel(data.model)}`
        // The attempt reported no model: say so (a running one may still
        // report), the request only as a hint.
        if (data.modelHint) return `${data.profile} · ${unreported} (hint ${shortModel(data.modelHint)})`
        return data.profile
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

  // The second line's tooltip: the full model id, or what an unreported
  // attempt was asked for, marked as a hint.
  const kindTitle = $derived.by(() => {
    if (data.kind !== 'agent_task' || !data.profile) return kindLine
    if (data.model) return `${data.profile} · ${data.model}`
    if (data.modelHint)
      return `${data.profile} · model ${unreported}; asked for ${data.modelHint} (a hint, not the model it ran on)`
    return kindLine
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

  // Last lines: the title, then the run verdict when there is one.
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
  // The card's tooltip on a run canvas: why it did not run, or its counts.
  const cardTitle = $derived(data.notRun?.title ?? data.runCount?.title)
</script>

{#if pill}
  <div
    class={cn(
      'flex h-[30px] min-w-[110px] items-center justify-center gap-1.5 rounded-full border bg-panel px-4 text-[11px] tracking-[0.06em] text-muted-foreground transition-[opacity,box-shadow]',
      dimmed && 'opacity-40',
      (selected || running) && 'glow',
      data.diff && `diff-${data.diff}`,
      'pill-box',
    )}
    style:--k={k}
    style:border-color={statusBorder ?? k}
    data-diff={data.diff}
    title={[data.title !== id ? data.title : '', cardTitle, data.layer?.title, data.issue?.text].filter(Boolean).join('\n') ||
      undefined}
  >
    {#if hasTarget}<Handle type="target" position={Position.Top} />{/if}
    {#if running}<span class="spin text-blue"></span>{/if}
    {#if data.status === 'succeeded'}<span class="text-green" aria-hidden="true">✓</span>{/if}
    <span>{id}</span>
    {#if data.runCount?.badge}<span class="run-count">{data.runCount.badge}</span>{/if}
    {#if data.issue}<span class={['node-issue', data.issue.error && 'error']} aria-label="validation issue">!</span>{/if}
    {#if data.status && data.status !== 'succeeded' && data.status !== 'pending'}
      <span class="sr-only">{data.status}</span>
    {/if}
    {#if hasSource}<Handle type="source" id={PLAIN_HANDLE} position={Position.Bottom} />{/if}
    {#if data.layer || data.notRun}
      <!-- A pill has no room for a line: the note hangs below it. -->
      <span class="pill-note" data-testid={data.layer ? 'node-layer' : 'node-not-run'}
        >{data.layer ? data.layer.text : `not run · ${data.notRun?.word}`}</span
      >
    {/if}
  </div>
{:else}
  <div
    class={cn(
      'w-[210px] border bg-panel px-2.5 py-[7px] text-xs leading-[1.45] text-foreground transition-[opacity,box-shadow]',
      data.kind === 'condition' && 'border-dashed',
      skipped && 'border-dashed opacity-30',
      dimmed && !skipped && 'opacity-40',
      (selected || running) && 'glow',
      data.diff && `diff-${data.diff}`,
    )}
    style:--k={k}
    style:border-color={statusBorder ?? k}
    title={cardTitle}
    data-diff={data.diff}
  >
    {#if hasTarget}<Handle type="target" position={Position.Top} />{/if}

    <div class="flex items-baseline justify-between gap-2">
      <b class="min-w-0 truncate font-semibold" title={subtitle ? `${subtitle} (${id})` : id}>{id}</b>
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
        {#if data.orderChanged}
          <span class="order-badge" title="Its edges are checked in another order than before">order changed</span>
        {/if}
        {#if data.diff === 'changed'}<span class="diff-word">changed</span>{/if}
        {#if data.runCount?.badge}
          <span class="run-count" title={data.runCount.title} data-testid="node-run-count">{data.runCount.badge}</span>
        {/if}
        {#if data.issue}
          <span class={['node-issue', data.issue.error && 'error']} title={data.issue.text} data-testid="node-issue"
            >!</span
          >
        {/if}
      </span>
    </div>

    <div
      class="truncate text-[11px]"
      style:color={k}
      title={kindTitle}
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
    {:else}
      <!-- The title stays next to the run verdict: no taller than a running
           card, which shows its progress instead (the title is in the
           heading's tooltip there). -->
      {#if subtitle}
        <div class="mt-0.5 truncate text-[11px] text-muted-foreground" title={subtitle}>{subtitle}</div>
      {/if}
      {#if statusLine}
        <div class={cn('mt-0.5 truncate text-[11px]', statusLine.tone)} title={statusLine.text}>{statusLine.text}</div>
      {/if}
    {/if}

    {#if data.notRun}
      <div class="run-note" title={data.notRun.title} data-testid="node-not-run">not run · {data.notRun.word}</div>
    {/if}
    {#if data.approval}
      <div
        class="approval-refusal"
        title={data.approval.detail ? `${data.approval.text} (${data.approval.detail})` : data.approval.text}
        data-testid="node-approval-refusal"
      >
        approval refused · <span class="code">{data.approval.code}</span>
      </div>
    {/if}
    {#if data.retryWait}
      <div
        class="retry-wait"
        class:frozen={data.retryWait.frozen}
        title={data.retryWait.title}
        data-testid="node-retry-wait"
      >
        {data.retryWait.text}
      </div>
    {/if}
    {#if data.judge}
      <div class="judge-box" title={data.judge.title} data-testid="node-judge">
        {#each data.judge.lines as line, i (i)}<div class="judge-line">{line}</div>{/each}
        {#if data.judge.more}<div class="judge-more">+{data.judge.more}</div>{/if}
        {#each data.judge.routed as r (r)}<div class="judge-routed">routed {r}</div>{/each}
        {#if data.judge.shadow}<span class="judge-shadow">shadow</span>{/if}
      </div>
    {/if}
    {#if data.layer}
      <div class="layer-line" title={data.layer.title} data-testid="node-layer">{data.layer.text}</div>
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

    {#if exitRow}
      <!-- One dot per exit, each under its own caption, so no two lines share a
           point and every line says on the node itself why it is taken. The
           plain dot comes first: the unconditional edges, and new ones. -->
      <div class="-mx-1 mt-1.5 flex items-start border-t border-line pt-1">
        {#if hasSource}
          <span
            class="exit-plain"
            title={(data.exits?.parallel ?? 0) > 0
              ? `always taken: ${data.exits?.parallel} unconditional ${data.exits?.parallel === 1 ? 'edge' : 'edges'}`
              : 'unconditional edges leave from here'}
          >
            {(data.exits?.parallel ?? 0) > 0 ? 'always' : ''}
          </span>
        {/if}
        {#each data.exits?.list ?? [] as exit, i (exit.id)}
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
            style={`left: ${(((i + (hasSource ? 1 : 0) + 0.5) / slots) * 100).toFixed(2)}%`}
          />
        {/each}
        {#each hollow as h, i (h.id)}
          <span class="exit-hollow" title={h.title}>{crowded ? '' : h.label}</span>
          <Handle
            type="source"
            id={h.id}
            class="hollow-handle"
            position={Position.Bottom}
            style={`left: ${(((named + i + (hasSource ? 1 : 0) + 0.5) / slots) * 100).toFixed(2)}%`}
          />
        {/each}
      </div>
      {#if crowded}
        <div class="exit-hollow-list" title={hollow.map((h) => h.title).join('\n')}>
          not connected: {hollow.map((h) => h.label).join(', ')}
        </div>
      {/if}
    {/if}
    <!-- Outside the exit row: a judge whose only hint is "add bands" may
         have no exit to draw. -->
    {#if data.exitNote}<div class="exit-note">{data.exitNote}</div>{/if}
    {#if hasSource}<Handle type="source" id={PLAIN_HANDLE} position={Position.Bottom} style={plainLeft} />{/if}
  </div>
{/if}

<style>
  /* The diff canvas: removed nodes are ghosts, added ones outlined in the
     success tone, changed ones in the warning tone. */
  .diff-removed {
    border-style: dashed;
    opacity: 0.45;
  }
  .diff-added {
    outline: 2px solid var(--c-green);
    outline-offset: 2px;
  }
  .diff-changed {
    outline: 2px solid var(--c-amber);
    outline-offset: 2px;
  }
  .order-badge,
  .diff-word {
    padding: 0 4px;
    border: 1px solid var(--c-amber);
    color: var(--c-amber);
    font-size: 10px;
    line-height: 14px;
  }
  .run-count {
    padding: 0 4px;
    border: 1px solid var(--c-line-2);
    color: var(--c-fg-2);
    font-size: 10px;
    line-height: 14px;
  }
  .node-issue {
    padding: 0 4px;
    border: 1px solid var(--c-amber);
    color: var(--c-amber);
    font-size: 10px;
    font-weight: 700;
    line-height: 14px;
  }
  .node-issue.error {
    border-color: var(--c-red);
    color: var(--c-red);
  }
  /* A node its approval check refused before any attempt: the code on the
     card, the sentence and what it is about in its tooltip. */
  .approval-refusal {
    margin-top: 2px;
    overflow: hidden;
    color: var(--c-red);
    font-size: 10px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .approval-refusal .code {
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .run-note,
  .layer-line {
    margin-top: 2px;
    overflow: hidden;
    color: var(--muted-foreground);
    font-size: 10px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* A retry waiting out its delay: amber while it counts down, muted with a
     dashed edge while the run is paused and the delay is frozen. */
  .retry-wait {
    margin-top: 3px;
    padding: 1px 5px;
    border: 1px solid var(--c-amber);
    color: var(--c-amber);
    font-size: 10px;
    line-height: 1.4;
    overflow-wrap: anywhere;
  }
  .retry-wait.frozen {
    border-style: dashed;
    color: var(--muted-foreground);
  }
  .judge-box {
    position: relative;
    margin-top: 3px;
    padding-top: 2px;
    border-top: 1px dashed var(--c-line);
    font-size: 10px;
    line-height: 1.35;
  }
  .judge-line,
  .judge-routed {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .judge-routed,
  .judge-more {
    color: var(--muted-foreground);
  }
  .pill-box {
    position: relative;
  }
  .pill-note {
    position: absolute;
    top: calc(100% + 2px);
    left: 0;
    right: 0;
    color: var(--muted-foreground);
    font-size: 9px;
    letter-spacing: 0;
    line-height: 1.25;
    text-align: center;
  }
  .judge-shadow {
    display: inline-block;
    margin-top: 2px;
    padding: 0 3px;
    border: 1px dashed var(--c-line-2);
    color: var(--muted-foreground);
  }
  .exit-hollow {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    padding: 0 2px;
    color: var(--muted-foreground);
    font-size: 9px;
    font-style: italic;
    line-height: 1.25;
    text-align: center;
  }
  .exit-hollow-list {
    margin-top: 2px;
    overflow: hidden;
    color: var(--muted-foreground);
    font-size: 9px;
    font-style: italic;
    line-height: 1.25;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  :global(.svelte-flow__handle.hollow-handle) {
    background: var(--c-bg);
    border: 1px dashed var(--c-fg-2);
  }
  .exit-note {
    margin-top: 2px;
    color: var(--c-amber);
    font-size: 9px;
    line-height: 1.25;
  }
  .list-badge {
    padding: 0 4px;
    border: 1px solid var(--c-line-2);
    color: var(--c-fg-2);
    font-size: 10px;
    line-height: 14px;
  }
  /* The caption slot of the plain dot, the same width as an exit's. */
  .exit-plain {
    flex: 1;
    padding: 0 2px;
    text-align: center;
    font-size: 9px;
    line-height: 1.25;
    color: var(--muted-foreground);
  }
</style>
