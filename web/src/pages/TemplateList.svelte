<script lang="ts">
  // The template gallery: the example playbooks apb ships. An install makes
  // a draft (apb approves nothing for it) in the chosen scope and says what
  // the new draft still needs.
  import { fetchProjects, fetchTemplates, installTemplate } from '../lib/api'
  import { effectLabels, installTarget } from '../lib/templates'
  import type { Project } from '../lib/types'
  import type { TemplateSummary } from '../lib/api.gen'
  import type { TemplateInstallAnswer } from '../lib/api'
  import FitCard from '../lib/FitCard.svelte'
  import Topbar from '$lib/components/Topbar.svelte'
  import PageScroll from '$lib/components/PageScroll.svelte'
  import { toast } from 'svelte-sonner'

  let templates = $state<TemplateSummary[]>([])
  let projects = $state<Project[]>([])
  let workspace = $state('')
  let scope = $state<'project' | 'global'>('project')
  let asId = $state('')
  // The templates with an install in flight: each button waits for its own.
  let pending = $state<string[]>([])
  // Whether the project list has answered (or failed): a project install
  // needs its workspace, so it waits for that.
  let projectsLoaded = $state(false)
  // Each answer keeps the project it was installed with: its Open link is
  // built from that, whatever the Project select says later.
  type Installed = TemplateInstallAnswer & { workspace: string }
  let answers = $state<Record<string, Installed | { error: string }>>({})

  const answerOf = (id: string) => answers[id]

  $effect(() => {
    let alive = true
    fetchTemplates()
      .then((t) => {
        if (alive) templates = t
      })
      .catch((e) => toast.error('The templates could not be loaded', { description: String(e) }))
    fetchProjects()
      .then((ps) => {
        if (!alive) return
        projects = ps
        workspace = ps[0]?.workspace_id ?? ''
        projectsLoaded = true
        // Without a reachable project only a global install can work.
        if (ps.length === 0) scope = 'global'
      })
      .catch(() => {
        // A project list that does not load leaves no reachable project.
        if (!alive) return
        projectsLoaded = true
        scope = 'global'
      })
    return () => {
      alive = false
    }
  })

  const noProject = $derived(projects.length === 0)

  function onScope(e: Event) {
    scope = (e.currentTarget as HTMLSelectElement).value as 'project' | 'global'
  }
  function onWorkspace(e: Event) {
    workspace = (e.currentTarget as HTMLSelectElement).value
  }

  const busy = (id: string) => pending.includes(id)
  const blocked = (id: string) => busy(id) || (scope === 'project' && !projectsLoaded)

  async function install(t: TemplateSummary) {
    if (blocked(t.id)) return
    pending = [...pending, t.id]
    try {
      // The selected project goes with both scopes: a project install is
      // written there, and a global one gets its fit report made for it.
      const used = workspace
      const body: { scope: string; as?: string; workspace?: string } = { scope }
      if (asId) body.as = asId
      if (used) body.workspace = used
      const a = await installTemplate(t.id, body)
      answers = { ...answers, [t.id]: { ...a, workspace: used } }
    } catch (e) {
      const err = e as { status?: number; message?: string }
      if (err.status === 409) {
        answers = { ...answers, [t.id]: { error: 'conflict' } }
      } else {
        toast.error('Install failed', { description: String(e) })
      }
    } finally {
      pending = pending.filter((p) => p !== t.id)
    }
  }
</script>

<Topbar active="templates" />
<PageScroll>
  <div class="page">
    <header class="head">
      <h1 class="title">Templates</h1>
      <p class="note">
        A template installs as a draft apb approves nothing for: run a trial, or approve it
        before a normal start.
      </p>
      <div class="controls">
        <label class="field">
          Project
          <select
            value={workspace}
            onchange={onWorkspace}
            disabled={noProject}
          >
            {#each projects as p (p.workspace_id)}
              <option value={p.workspace_id}>{p.name}</option>
            {/each}
          </select>
        </label>
        <label class="field">
          Scope
          <select value={scope} onchange={onScope}>
            <option value="project" disabled={noProject}>project</option>
            <option value="global">global</option>
          </select>
        </label>
        <label class="field">
          Install as
          <input type="text" placeholder="the template's own id" bind:value={asId} />
        </label>
      </div>
      {#if noProject}
        <p class="note">Open a project for a project-scope install; a global one works here.</p>
      {/if}
    </header>

    <div class="cards">
      {#each templates as t (t.id)}
        {@const a = answerOf(t.id)}
        <section class="card">
          <h2 class="name">{t.name}</h2>
          <p class="mono id">{t.id}</p>
          <p class="desc">{t.description}</p>
          <p class="facts">
            <span class="mono">v{t.version}</span>
            {#each effectLabels(t.effects) as e (e)}
              <span class="tag" data-effect={e}>{e}</span>
            {/each}
            {#if t.scenarios > 0}
              <span class="tag scenarios">{t.scenarios} scenarios</span>
            {/if}
          </p>
          <button class="install" onclick={() => install(t)} disabled={blocked(t.id)}>
            {busy(t.id) ? 'Installing.' : `Install${scope === 'global' ? ' (global)' : ''}`}
          </button>

          {#if a && 'rejected' in a === false && 'error' in a === false}
            <div class="answer">
              <p class="okline">
                Installed as draft <span class="mono">{a.id}</span> v{a.version} ({a.scope}).
              </p>
              {#each a.needs as n (n)}
                <p class="need">{n}</p>
              {/each}
              {#each a.warnings as w (w)}
                <p class="warning">{w}</p>
              {/each}
              {#if a.fit}
                <FitCard report={a.fit} />
              {:else}
                <p class="commands">
                  <code>apb run {a.id} --scope {a.scope}</code>
                  <code>apb adopt {a.id} --scope {a.scope}</code>
                </p>
              {/if}
              <a class="link" href={installTarget(a.scope, a.workspace, a.id)}>
                Open {a.id}
              </a>
            </div>
          {:else if a && 'error' in a}
            <p class="conflict">
              A playbook with this id already exists in that scope; pick another id in Install as.
            </p>
          {/if}
        </section>
      {/each}
    </div>
  </div>
</PageScroll>

<style>
  .page {
    display: flex;
    flex-direction: column;
    gap: 1.25rem;
    max-width: 70rem;
    padding: 0 0.875rem 2rem;
  }
  .head {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .title {
    font-size: 1.15rem;
    font-weight: 700;
  }
  .note {
    color: var(--muted-foreground);
    font-size: 0.8rem;
  }
  .controls {
    display: flex;
    flex-wrap: wrap;
    gap: 0.9rem;
  }
  .field {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    font-size: 0.8rem;
  }
  .field select,
  .field input {
    background: var(--card);
    border: 1px solid var(--border);
    padding: 0.25rem 0.4rem;
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(20rem, 1fr));
    gap: 0.9rem;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    border: 1px solid var(--border);
    background: var(--card);
    padding: 0.9rem;
  }
  .name {
    font-size: 0.95rem;
    font-weight: 700;
  }
  .mono {
    font-family: var(--font-mono);
  }
  .id {
    color: var(--muted-foreground);
    font-size: 0.72rem;
  }
  .desc {
    font-size: 0.8rem;
  }
  .facts {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.72rem;
  }
  .tag {
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 0.05rem 0.5rem;
  }
  .tag[data-effect='irreversible'] {
    border-color: var(--destructive);
    color: var(--destructive);
  }
  .install {
    align-self: flex-start;
    background: var(--primary);
    color: var(--primary-foreground);
    border: 1px solid var(--border);
    cursor: pointer;
    font-size: 0.8rem;
    margin-top: 0.3rem;
    padding: 0.3rem 0.7rem;
  }
  .install:disabled {
    cursor: default;
    opacity: 0.6;
  }
  .answer {
    border-top: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    margin-top: 0.4rem;
    padding-top: 0.5rem;
  }
  .okline {
    font-size: 0.8rem;
  }
  .need {
    font-size: 0.78rem;
  }
  .need::before {
    content: 'needs: ';
    color: var(--muted-foreground);
  }
  .warning {
    color: var(--warning);
    font-size: 0.78rem;
  }
  .commands {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .commands code {
    align-self: flex-start;
    background: var(--card);
    border: 1px solid var(--border);
    font-family: var(--font-mono);
    font-size: 0.75rem;
    padding: 0.2rem 0.45rem;
  }
  .link {
    font-size: 0.8rem;
  }
  .conflict {
    border-top: 1px solid var(--border);
    color: var(--destructive);
    font-size: 0.78rem;
    margin-top: 0.4rem;
    padding-top: 0.5rem;
  }
</style>
