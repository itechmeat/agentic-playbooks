<script lang="ts">
  import type { PlaybookEdge, PlaybookNode } from './types'
  import { edgeLabel, edgeTone, expectedText, type FlowNodeData } from './graph'
  import { accentVar, nodeKindColor } from './profilecolor'
  import { shortModel } from './format'
  import { questionSummary, toYamlField } from './judgefields'
  import { Button } from '$lib/components/ui/button'
  import FileText from '@lucide/svelte/icons/file-text'
  import X from '@lucide/svelte/icons/x'

  // The read-only summary of the selected node on the playbook view: who runs
  // it, how long it should take, where it comes from and goes to, and its
  // prompt. The whole read-only form is one click away (`onopenform`).
  let {
    node,
    data,
    edges,
    agent = '',
    onopenform,
    onclose,
  }: {
    node: PlaybookNode
    /** The card data of the same node (profile, model, expected). */
    data: FlowNodeData | null
    edges: PlaybookEdge[]
    /** The profile's agent, when the profile listing knows it. */
    agent?: string
    onopenform?: () => void
    onclose?: () => void
  } = $props()

  const k = $derived(nodeKindColor(node.type, data?.profile))
  const str = (v: unknown) => (typeof v === 'string' ? v : v == null ? '' : String(v))

  const incoming = $derived(edges.filter((e) => e.to === node.id))
  const outgoing = $derived(edges.filter((e) => e.from === node.id))
  const edgeNote = (e: PlaybookEdge) => (e.fallback ? 'else' : edgeLabel(e))

  const expected = $derived(data?.expected ?? expectedText(node.expected_duration))

  // The long text of the node, by kind: the prompt of an agent task (and of a
  // finish with a summary prompt), the instruction of a sub-playbook.
  const longText = $derived.by(() => {
    if (node.type === 'playbook') return { cap: 'instruction', text: str(node.instruction) }
    if (node.type === 'script') return { cap: 'script', text: str(node.script) }
    return { cap: 'prompt', text: str(node.prompt) }
  })

  // `{{ ... }}` template references stand out in the prompt.
  const parts = $derived(
    longText.text.split(/(\{\{[^}]*\}\})/g).map((t) => ({ t, ref: /^\{\{[^}]*\}\}$/.test(t) })),
  )

  const options = $derived(Array.isArray(node.options) ? node.options.map(String) : [])
  const judgeAsks = $derived(node.type === 'judge' ? questionSummary(node.questions) : '')
  const judgeYaml = $derived(node.type === 'judge' ? toYamlField(node.questions) : '')
</script>

<div class="box px-4 pt-3.5 pb-3.5" style:--k={k} data-testid="node-summary">
  <span class="box-cap">node</span>
  <div class="flex items-start justify-between gap-2">
    <span class="min-w-0 truncate text-sm font-semibold">{node.id}</span>
    <div class="flex shrink-0 items-center gap-1">
      <span class="tag" style:color={k}>{node.type}</span>
      {#if onclose}
        <Button variant="ghost" size="icon-xs" title="Close" onclick={onclose}><X /></Button>
      {/if}
    </div>
  </div>
  {#if node.title}
    <p class="prose-text mt-1 text-xs text-muted-foreground">{node.title}</p>
  {/if}

  <dl class="kv mt-3">
    {#if data?.profile}
      <dt>profile</dt>
      <dd style:color={k}>{data.profile}</dd>
      {#if agent}
        <dt>agent</dt>
        <dd>{agent}</dd>
      {/if}
      <dt>model</dt>
      <dd class={data.model ? '' : 'text-muted-foreground'} title={data.model ?? undefined}>{data.model ? shortModel(data.model) : 'unknown'}</dd>
    {/if}
    {#if data?.subPlaybook}
      <dt>playbook</dt>
      <dd>{data.subPlaybook}</dd>
    {/if}
    {#if expected}
      <dt>expected</dt>
      <dd>~{expected}</dd>
    {/if}
    {#if ['agent_task', 'script', 'playbook'].includes(node.type)}
      <dt>retries</dt>
      <dd class={typeof node.max_retries === 'number' ? '' : 'text-muted-foreground'}>
        {typeof node.max_retries === 'number' ? node.max_retries : 'playbook default'}
      </dd>
    {/if}
    {#if typeof node.timeout_seconds === 'number'}
      <dt>timeout</dt>
      <dd>{node.timeout_seconds}s</dd>
    {/if}
    {#if node.isolation}
      <dt>isolation</dt>
      <dd>{str(node.isolation)}</dd>
    {/if}
    {#if node.continue_session}
      <dt>continues</dt>
      <dd>{str(node.continue_session)}</dd>
    {/if}
  </dl>

  <div class="mt-3.5 grid grid-cols-2 gap-3 text-xs">
    <div class="min-w-0">
      <div class="mb-1 text-muted-foreground">← from</div>
      {#each incoming as e, i (i)}
        <div class="break-words" title={edgeNote(e) ?? ''}>
          {e.from}
          {#if edgeNote(e)}<span class="text-muted-foreground">[{edgeNote(e)}]</span>{/if}
        </div>
      {:else}
        <div class="text-muted-foreground">none</div>
      {/each}
    </div>
    <div class="min-w-0">
      <div class="mb-1 text-muted-foreground">→ to</div>
      {#each outgoing as e, i (i)}
        <div class="break-words" title={edgeNote(e) ?? ''}>
          {e.to}
          {#if edgeNote(e)}
            <span class={edgeTone(e) === 'fail' ? 'text-red' : 'text-muted-foreground'}>[{edgeNote(e)}]</span>
          {/if}
        </div>
      {:else}
        <div class="text-muted-foreground">none</div>
      {/each}
    </div>
  </div>

  {#if options.length}
    <div class="mt-3 flex flex-wrap items-center gap-1.5 text-xs">
      <span class="text-muted-foreground">options</span>
      {#each options as o (o)}<span class="tag text-amber">{o}</span>{/each}
    </div>
  {/if}

  {#if onopenform}
    <Button variant="outline" size="xs" class="mt-3.5" onclick={onopenform} title="Show every field of this node, read-only">
      <FileText data-icon="inline-start" />
      all fields
    </Button>
  {/if}
</div>

{#if node.type === 'judge'}
  <div class="box px-4 pt-3.5 pb-3.5" style:--k={accentVar('green')}>
    <span class="box-cap">judge</span>
    {#if judgeAsks}
      <p class="mb-2 text-xs text-muted-foreground">asks <span class="text-green">{judgeAsks}</span></p>
    {/if}
    {#if judgeYaml}
      <pre class="max-h-60 overflow-auto border border-line bg-field px-3 py-2.5 text-xs whitespace-pre-wrap text-fg-2">{judgeYaml}</pre>
    {/if}
  </div>
{:else if longText.text}
  <div class="box px-4 pt-3.5 pb-3.5">
    <span class="box-cap">{longText.cap}</span>
    <pre
      class="max-h-72 overflow-auto border border-line bg-field px-3 py-2.5 text-xs leading-relaxed break-words whitespace-pre-wrap text-fg-2"
      data-testid="node-summary-prompt">{#each parts as p, i (i)}{#if p.ref}<span class="text-violet">{p.t}</span>{:else}{p.t}{/if}{/each}</pre>
  </div>
{/if}
