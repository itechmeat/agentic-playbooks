<script lang="ts">
  import { SvelteFlow, Background, Controls } from '@xyflow/svelte'
  import '@xyflow/svelte/dist/style.css'
  import { fetchPlaybook, fetchVersions, promoteVersion } from '../lib/api'
  import { toFlow, type FlowEdge, type FlowNode } from '../lib/graph'
  import { subscribeChanges } from '../lib/ws'
  import { provenanceLabel } from '../lib/versioninfo'
  import type { VersionInfo } from '../lib/types'
  import PlaybookNode from '../lib/PlaybookNode.svelte'

  let { id }: { id: string } = $props()

  let nodes = $state.raw<FlowNode[]>([])
  let edges = $state.raw<FlowEdge[]>([])
  let name = $state<string>(id)
  let version = $state<string>('')
  let validation = $state<{ code: string; severity: string; message: string; node?: string | null }[]>([])
  let error = $state<string | null>(null)
  let versions = $state<VersionInfo[]>([])
  let promoteError = $state<string | null>(null)
  let promoting = $state<string | null>(null)

  const nodeTypes = { playbookNode: PlaybookNode }

  const errors = $derived(validation.filter((v) => v.severity === 'error'))
  const issuesTitle = $derived(
    validation.map((v) => `${v.severity} ${v.code}: ${v.message}${v.node ? ` (${v.node})` : ''}`).join('\n'),
  )

  async function loadVersions() {
    try {
      versions = await fetchVersions(id)
    } catch (e) {
      promoteError = String(e)
    }
  }

  async function load() {
    try {
      const detail = await fetchPlaybook(id)
      const flow = toFlow(detail.playbook, detail.layout)
      nodes = flow.nodes
      edges = flow.edges
      name = detail.playbook.name || detail.id
      version = detail.version
      validation = detail.validation
      error = null
    } catch (e) {
      error = String(e)
    }
    await loadVersions()
  }

  async function promote(v: string) {
    promoteError = null
    promoting = v
    try {
      await promoteVersion(id, v)
      await load()
    } catch (e) {
      promoteError = String(e)
    } finally {
      promoting = null
    }
  }

  $effect(() => {
    load()
    return subscribeChanges(load)
  })
</script>

<header class="topbar">
  <a href="#/">← playbooks</a>
  <h1>{name}</h1>
  <span class="muted">{version}</span>
  <span class="spacer"></span>
  <a href={`#/edit/${id}`}>edit</a>
  {#if error}
    <span class="badge err" title={error}>error</span>
  {:else if errors.length > 0}
    <span class="badge err" title={issuesTitle}>{errors.length} errors</span>
  {:else if validation.length > 0}
    <span class="badge warn" title={issuesTitle}>{validation.length} warnings</span>
  {:else}
    <span class="badge ok">valid</span>
  {/if}
</header>

<div class="page-body">
  <div class="pane-graph">
    <SvelteFlow bind:nodes bind:edges {nodeTypes} fitView
      nodesDraggable={false} nodesConnectable={false} elementsSelectable={false}>
      <Background />
      <Controls />
    </SvelteFlow>
  </div>

  <aside class="versions">
    <h2>Version history</h2>
    {#if promoteError}
      <p class="error">{promoteError}</p>
    {/if}
    {#if versions.length === 0}
      <p class="muted">no versions</p>
    {:else}
      <ul>
        {#each versions as v (v.version)}
          <li class:current={v.is_current}>
            <div class="row">
              <span class="ver">{v.version}</span>
              {#if v.is_current}<span class="badge ok">current</span>{/if}
            </div>
            <div class="prov muted">{provenanceLabel(v)}</div>
            {#if v.provenance && !v.provenance.promoted}
              <button onclick={() => promote(v.version)} disabled={promoting === v.version}>
                {promoting === v.version ? 'promoting...' : 'Promote'}
              </button>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  </aside>
</div>

<style>
  .page-body {
    flex: 1 1 auto;
    min-height: 0;
    display: flex;
  }
  .pane-graph {
    flex: 1 1 auto;
    min-width: 0;
    min-height: 0;
    position: relative;
  }
  .versions {
    flex: 0 0 260px;
    min-height: 0;
    overflow: auto;
    border-left: 1px solid var(--border);
    padding: 12px;
    font-size: 13px;
  }
  .versions h2 { margin: 0 0 8px; font-size: 13px; font-weight: 600; }
  .versions ul { list-style: none; margin: 0; padding: 0; }
  .versions li { padding: 8px 0; border-bottom: 1px solid var(--border); }
  .versions li.current { font-weight: 600; }
  .versions .row { display: flex; align-items: center; gap: 8px; }
  .versions .ver { font-family: ui-monospace, monospace; }
  .versions .prov { font-size: 12px; margin: 2px 0 6px; }
  .versions button {
    font-size: 12px;
    padding: 2px 10px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: transparent;
    color: var(--fg);
    cursor: pointer;
  }
  .versions button:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
  .versions button:disabled { opacity: 0.6; cursor: default; }
  .muted { color: var(--muted); }
</style>
