<script lang="ts">
  import { deletePlaybook, fetchPlaybook, fetchPlaybooks } from '../lib/api'
  import { storeDraftYaml, suggestDuplicateId } from '../lib/playbookdupe'
  import { subscribeChanges } from '../lib/ws'
  import type { PlaybookSummary } from '../lib/types'

  let items = $state<PlaybookSummary[]>([])
  let error = $state<string | null>(null)
  let deleting = $state<string | null>(null)

  async function load() {
    try {
      items = await fetchPlaybooks()
      error = null
    } catch (e) {
      error = String(e)
    }
  }

  $effect(() => {
    load()
    return subscribeChanges(load)
  })

  async function duplicate(sourceId: string) {
    try {
      const detail = await fetchPlaybook(sourceId)
      storeDraftYaml(detail.yaml, suggestDuplicateId(sourceId))
      location.hash = '#/new'
    } catch (e) {
      error = String(e)
    }
  }

  async function remove(id: string) {
    if (!confirm(`Delete playbook "${id}"? It will be moved to trash.`)) return
    deleting = id
    try {
      await deletePlaybook(id)
      await load()
      error = null
    } catch (e) {
      error = String(e)
    } finally {
      deleting = null
    }
  }
</script>

<header class="topbar">
  <h1>Playbooks</h1>
  <span class="spacer"></span>
  <a href="#/new" class="btn-link">Create</a>
  <a href="#/runs">runs</a>
  {#if error}<span class="badge err" title={error}>error</span>{/if}
</header>

<div class="page-scroll">
  {#if items.length === 0 && !error}<p>No playbooks in .apb/playbooks yet.</p>{/if}
  <ul>
    {#each items as w (w.id)}
      <li>
        <a href={`#/playbook/${w.id}`}><strong>{w.name}</strong></a>
        <span class="meta">{w.id} · current {w.current} · versions: {w.versions.join(', ')}</span>
        <span class="actions">
          <button class="btn-sm" onclick={() => duplicate(w.id)} title={`Duplicate as ${suggestDuplicateId(w.id)}`}>
            Duplicate
          </button>
          <button class="btn-sm btn-danger" onclick={() => remove(w.id)} disabled={deleting === w.id}>
            {deleting === w.id ? 'Deleting...' : 'Delete'}
          </button>
        </span>
        {#if w.description}<p>{w.description}</p>{/if}
      </li>
    {/each}
  </ul>
</div>

<style>
  .btn-link {
    color: var(--accent);
    text-decoration: none;
    font-size: 13px;
  }
  .btn-link:hover { text-decoration: underline; }
  .actions {
    display: inline-flex;
    gap: 6px;
    margin-left: 8px;
  }
  .btn-sm {
    font: inherit;
    font-size: 11px;
    padding: 2px 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    cursor: pointer;
  }
  .btn-sm:hover:not(:disabled) { border-color: var(--accent); }
  .btn-sm:disabled { opacity: 0.5; cursor: default; }
  .btn-danger:hover:not(:disabled) { border-color: var(--err); color: var(--err); }
</style>
