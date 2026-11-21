<script lang="ts">
  // The read-only page of a global playbook: what it is, whether it fits a
  // chosen project, and the commands that run it. Everything that acts on a
  // playbook of a project (run, edit, versions, memory, scenarios,
  // statistics) stays on the CLI, so none of it is reachable from here.
  import { fetchFit, fetchPlaybook, fetchProjects } from '../lib/api'
  import type { PlaybookDetail, Project } from '../lib/types'
  import type { FitReport } from '../lib/api.gen'
  import Topbar from '$lib/components/Topbar.svelte'
  import PageScroll from '$lib/components/PageScroll.svelte'
  import FitCard from '../lib/FitCard.svelte'
  import IssuesPanel from '../lib/IssuesPanel.svelte'

  let { id }: { id: string } = $props()

  // The embedded playbook of the detail carries its `description` (a schema
  // field the shared TS type does not name).
  type GlobalDetail = PlaybookDetail & {
    playbook: PlaybookDetail['playbook'] & { description?: string }
  }
  let detail = $state<GlobalDetail | null>(null)
  let failed = $state(false)
  let projects = $state<Project[]>([])
  let workspace = $state('')
  let fit = $state<FitReport | null>(null)
  let fitLoading = $state(false)
  let fitError = $state<string | null>(null)

  const effects = $derived(detail?.effective_effects ?? [])
  const runCmd = $derived(`apb run ${id} --scope global`)
  const adoptCmd = $derived(`apb adopt ${id} --scope global`)

  $effect(() => {
    let alive = true
    // The page is reused when the route names another global playbook:
    // each id starts from a clean state.
    const current = id
    detail = null
    failed = false
    fetchProjects()
      .then((ps) => {
        if (!alive) return
        projects = ps
        workspace = ps[0]?.workspace_id ?? ''
      })
      .catch(() => {})
    fetchPlaybook(current, '', undefined, 'global')
      .then((d) => {
        if (alive) detail = d
      })
      .catch(() => {
        if (alive) failed = true
      })
    return () => {
      alive = false
    }
  })

  async function loadFit() {
    if (!workspace) return
    const token = ++fitToken
    fitLoading = true
    fitError = null
    try {
      const r = await fetchFit(id, workspace, 'global')
      if (token === fitToken) fit = r
    } catch (e) {
      if (token === fitToken) fitError = String(e)
    } finally {
      if (token === fitToken) fitLoading = false
    }
  }
  let fitToken = 0
  $effect(() => {
    void workspace
    fit = null
    void loadFit()
  })

  function onPick(e: Event) {
    workspace = (e.currentTarget as HTMLSelectElement).value
  }
</script>

<Topbar active="playbooks" />
<PageScroll>
  <div class="page">
    {#if failed}
      <p class="note">The global playbook could not be loaded.</p>
    {:else if detail}
      <header class="head">
        <h1 class="title">{detail.playbook.name}</h1>
        <p class="sub">
          <span class="mono">{detail.id}</span>
          <span class="dot">·</span> v{detail.version}
          <span class="dot">·</span> global playbook
        </p>
        {#if detail.playbook.description}
          <p class="desc">{detail.playbook.description}</p>
        {/if}
        {#if effects.length > 0}
          <p class="effects">
            {#each effects as e (e)}<span class="tag">{e}</span>{/each}
          </p>
        {/if}
      </header>

      {#if detail.validation.length > 0}
        <section class="section">
          <h2 class="h2">Validation</h2>
          <IssuesPanel
            issues={detail.validation}
            drawable={{ nodes: new Set<string>(), edges: new Set<string>() }}
            fix={null}
          />
        </section>
      {/if}

      <section class="section">
        <h2 class="h2">Run it</h2>
        <p class="note">
          Start and edit a global playbook with the CLI; the dashboard shows it.
        </p>
        <code class="cmd">{runCmd}</code>
        <code class="cmd">{adoptCmd}</code>
      </section>

      <section class="section">
        <h2 class="h2">Does it fit a project?</h2>
        {#if projects.length === 0}
          <p class="note">Open a project to see whether this playbook fits it.</p>
        {:else}
          <label class="pick">
            Project
            <select value={workspace} onchange={onPick}>
              {#each projects as p (p.workspace_id)}
                <option value={p.workspace_id}>{p.name}</option>
              {/each}
            </select>
          </label>
          <FitCard report={fit} loading={fitLoading} error={fitError} />
        {/if}
      </section>

      <section class="section">
        <h2 class="h2">Definition</h2>
        <pre class="yaml">{detail.yaml}</pre>
      </section>
    {:else}
      <p class="note">Loading.</p>
    {/if}
  </div>
</PageScroll>

<style>
  .page {
    display: flex;
    flex-direction: column;
    gap: 1.25rem;
    max-width: 60rem;
    padding: 0 0.875rem 2rem;
  }
  .head {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .title {
    font-size: 1.15rem;
    font-weight: 700;
  }
  .sub {
    color: var(--muted-foreground);
    font-size: 0.8rem;
  }
  .mono {
    font-family: var(--font-mono);
  }
  .dot {
    margin: 0 0.35rem;
  }
  .desc {
    font-size: 0.875rem;
  }
  .effects {
    display: flex;
    gap: 0.35rem;
  }
  .tag {
    border: 1px solid var(--border);
    border-radius: 999px;
    font-size: 0.7rem;
    padding: 0.05rem 0.5rem;
  }
  .section {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    border-top: 1px solid var(--border);
    padding-top: 0.9rem;
  }
  .h2 {
    font-size: 0.7rem;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted-foreground);
  }
  .note {
    color: var(--muted-foreground);
    font-size: 0.8rem;
  }
  .cmd {
    align-self: flex-start;
    background: var(--card);
    border: 1px solid var(--border);
    display: block;
    font-family: var(--font-mono);
    font-size: 0.8rem;
    padding: 0.35rem 0.6rem;
  }
  .pick {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: 0.85rem;
  }
  .pick select {
    background: var(--card);
    border: 1px solid var(--border);
    padding: 0.25rem 0.4rem;
  }
  .yaml {
    background: var(--card);
    border: 1px solid var(--border);
    font-family: var(--font-mono);
    font-size: 0.78rem;
    overflow: auto;
    padding: 0.7rem;
    white-space: pre;
  }
</style>
