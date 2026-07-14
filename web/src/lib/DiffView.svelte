<script lang="ts">
  import { fetchDiff } from './api'
  import { formatDiff } from './difffmt'
  import type { VersionDiff } from './types'

  let { id, versions }: { id: string; versions: string[] } = $props()

  let from = $state('')
  let to = $state('')
  let diff = $state<VersionDiff | null>(null)
  let loading = $state(false)
  let error = $state<string | null>(null)

  const lines = $derived(diff ? formatDiff(diff.yaml_diff) : [])

  $effect(() => {
    // default: the latest and second-to-latest versions
    if (versions.length >= 2) {
      from = versions[versions.length - 2]
      to = versions[versions.length - 1]
    } else if (versions.length === 1) {
      from = versions[0]
      to = versions[0]
    }
  })

  async function load() {
    if (!from || !to || from === to) {
      diff = null
      return
    }
    loading = true
    error = null
    try {
      diff = await fetchDiff(id, from, to)
    } catch (e) {
      error = String(e)
      diff = null
    } finally {
      loading = false
    }
  }
</script>

<div class="diff">
  <div class="diff-bar">
    <select bind:value={from}>
      {#each versions as v}<option value={v}>{v}</option>{/each}
    </select>
    <span class="arrow">-></span>
    <select bind:value={to}>
      {#each versions as v}<option value={v}>{v}</option>{/each}
    </select>
    <button class="btn" onclick={load} disabled={loading || !from || !to}>
      {loading ? '...' : 'diff'}
    </button>
    {#if error}<span class="err" title={error}>error</span>{/if}
  </div>

  {#if diff}
    <div class="sections">
      {#if diff.nodes_added.length}
        <section><h4>nodes added</h4><ul>{#each diff.nodes_added as n}<li class="add">+ {n}</li>{/each}</ul></section>
      {/if}
      {#if diff.nodes_removed.length}
        <section><h4>nodes removed</h4><ul>{#each diff.nodes_removed as n}<li class="del">- {n}</li>{/each}</ul></section>
      {/if}
      {#if diff.nodes_changed.length}
        <section><h4>nodes changed</h4><ul>{#each diff.nodes_changed as n}<li class="chg">~ {n}</li>{/each}</ul></section>
      {/if}
      {#if diff.edges_added.length}
        <section><h4>edges added</h4><ul>{#each diff.edges_added as e}<li class="add">+ {e}</li>{/each}</ul></section>
      {/if}
      {#if diff.edges_removed.length}
        <section><h4>edges removed</h4><ul>{#each diff.edges_removed as e}<li class="del">- {e}</li>{/each}</ul></section>
      {/if}
    </div>

    <pre class="yaml-diff">{#each lines as line}<span class="l-{line.kind}">{line.kind === 'add' ? '+' : line.kind === 'del' ? '-' : ' '}{line.text}{'\n'}</span>{/each}</pre>
  {/if}
</div>

<style>
  .diff { display: flex; flex-direction: column; gap: 8px; height: 100%; min-height: 0; }
  .diff-bar { display: flex; align-items: center; gap: 6px; font-size: 12px; flex: 0 0 auto; }
  .diff-bar select {
    font: inherit;
    font-size: 12px;
    color: var(--fg);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 2px 4px;
  }
  .arrow { color: var(--muted); }
  .btn {
    font: inherit;
    font-size: 12px;
    padding: 2px 10px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    cursor: pointer;
  }
  .btn:hover:not(:disabled) { border-color: var(--accent); }
  .btn:disabled { opacity: 0.5; cursor: default; }
  .err { color: var(--err); }
  .sections { display: flex; flex-direction: column; gap: 6px; font-size: 12px; flex: 0 0 auto; }
  .sections h4 { margin: 0; font-size: 11px; color: var(--muted); text-transform: uppercase; }
  .sections ul { margin: 2px 0 0; padding-left: 14px; }
  .add { color: #22a06b; }
  .del { color: #dc2626; }
  .chg { color: var(--warn); }
  .yaml-diff {
    flex: 1 1 auto;
    min-height: 0;
    overflow: auto;
    margin: 0;
    padding: 8px;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 4px;
    font-family: ui-monospace, 'Cascadia Code', 'Source Code Pro', Menlo, monospace;
    font-size: 12px;
    line-height: 1.45;
    white-space: pre;
  }
  .l-add { color: #22a06b; }
  .l-del { color: #dc2626; }
  .l-meta { color: var(--muted); }
  .l-ctx { color: var(--fg); opacity: 0.85; }
</style>
