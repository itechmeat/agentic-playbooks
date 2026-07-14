<script lang="ts">
  import { fetchRuns } from '../lib/api'
  import { subscribeChanges } from '../lib/ws'
  import type { RunSummary } from '../lib/types'

  let items = $state<RunSummary[]>([])
  let error = $state<string | null>(null)

  async function load() {
    try { items = await fetchRuns(); error = null }
    catch (e) { error = String(e) }
  }

  $effect(() => {
    load()
    return subscribeChanges(load)
  })
</script>

<header class="topbar">
  <a href="#/">playbooks</a>
  <h1>Runs</h1>
  <span class="spacer"></span>
  {#if error}<span class="badge err" title={error}>error</span>{/if}
</header>

<div class="page-scroll">
  {#if items.length === 0 && !error}<p>No runs yet.</p>{/if}
  <ul>
    {#each items as r (r.run_id)}
      <li>
        <a href={`#/run/${encodeURIComponent(r.run_id)}`}><strong>{r.run_id}</strong></a>
        <span class="meta">{r.playbook} · {r.status}</span>
      </li>
    {/each}
  </ul>
</div>
