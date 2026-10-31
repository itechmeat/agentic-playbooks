<script lang="ts">
  import {
    dismissDiscovered,
    fetchDiscoverRoots,
    fetchDiscoveries,
    fetchDiscovery,
    fetchProjects,
    runDiscovery,
    saveDiscovered,
  } from '../lib/api'
  import type { ResultSummary, ResultView, RootInfo, Suggestion } from '../lib/api.gen'
  import type { Project } from '../lib/types'
  import {
    canAct,
    defaultRoots,
    projectsText,
    readLines,
    refusalText,
    rejectedText,
    savedText,
    sourceText,
    statusText,
    stepsText,
    supportText,
  } from '../lib/discover'
  import Topbar from '$lib/components/Topbar.svelte'
  import PageScroll from '$lib/components/PageScroll.svelte'
  import { Button } from '$lib/components/ui/button'
  import { Spinner } from '$lib/components/ui/spinner'
  import { toast } from 'svelte-sonner'

  // History discovery: the step sequences past agent sessions repeat, read
  // from this machine's agent history (roots named in the config, never a
  // path typed here) for the projects picked below, offered as drafts.
  let projects = $state<Project[]>([])
  let roots = $state<RootInfo[]>([])
  let pickedProjects = $state<string[]>([])
  let pickedRoots = $state<string[]>([])
  let days = $state(90)
  let past = $state<ResultSummary[]>([])
  let view = $state<ResultView | null>(null)
  let running = $state(false)
  // The suggestions with a save or decline in flight, each on its own, as
  // `<result id>/<slug>` (one slug can stand in several results).
  let busy = $state<string[]>([])
  const busyKey = (id: string, slug: string) => `${id}/${slug}`
  // The newest request for `view`: an older answer that lands later is
  // dropped, so the page shows (and acts on) the result picked last.
  let viewRequest = 0
  let loadError = $state<string | null>(null)
  let scopes = $state<Record<string, 'project' | 'global'>>({})

  async function load() {
    try {
      const [p, r, d] = await Promise.all([fetchProjects(), fetchDiscoverRoots(), fetchDiscoveries()])
      projects = p
      roots = r
      past = d
      if (!pickedRoots.length) pickedRoots = defaultRoots(r)
      if (!pickedProjects.length && p.length === 1) pickedProjects = [p[0].workspace_id]
      loadError = null
    } catch (e) {
      loadError = refusalText(e)
    }
  }

  $effect(() => {
    load()
  })

  function toggle(list: string[], value: string): string[] {
    return list.includes(value) ? list.filter((v) => v !== value) : [...list, value]
  }

  /** Loads `view` from `request`, unless a newer request was made since. */
  async function showView(request: () => Promise<ResultView>): Promise<boolean> {
    const mine = ++viewRequest
    const v = await request()
    if (mine !== viewRequest) return false
    view = v
    return true
  }

  /** Refreshes the list of earlier discoveries; a failure leaves it as it was. */
  async function refreshPast() {
    try {
      past = await fetchDiscoveries()
    } catch {
      // The list is an extra: what was just done stands.
    }
  }

  async function run() {
    running = true
    try {
      await showView(() =>
        runDiscovery({
          workspaces: pickedProjects,
          roots: pickedRoots,
          since_days: days,
        }),
      )
    } catch (e) {
      toast.error('Discovery failed', { description: refusalText(e) })
      return
    } finally {
      running = false
    }
    await refreshPast()
  }

  async function open(id: string) {
    try {
      await showView(() => fetchDiscovery(id))
    } catch (e) {
      toast.error('Could not open the discovery', { description: refusalText(e) })
    }
  }

  /** After a save or decline went through: the result again, while it is
   * still the one shown (a result picked since stays), then the list. */
  async function refresh(id: string) {
    if (view?.id === id) {
      try {
        await showView(() => fetchDiscovery(id))
      } catch {
        // The action went through; the card shows it on the next open.
      }
    }
    await refreshPast()
  }

  /** What one action on a suggestion says. */
  interface Action {
    call: (id: string) => Promise<{ id?: string; rejected?: string }>
    refused: string
    failed: string
    done: (out: { id?: string }) => string
    detail?: string
  }

  /** Runs `a` on `s`, keeping its buttons off until it is done. A save or
   * decline lands where the server says: the project the suggestion was
   * seen in first, by its workspace id. */
  async function act(s: Suggestion, a: Action) {
    if (!view) return
    const id = view.id
    const key = busyKey(id, s.slug)
    busy = [...busy, key]
    try {
      const out = await a.call(id)
      if (out.rejected) {
        toast.error(a.refused, { description: rejectedText(out.rejected) })
        return
      }
      toast.success(a.done(out), { description: a.detail })
    } catch (e) {
      toast.error(a.failed, { description: refusalText(e) })
      return
    } finally {
      busy = busy.filter((x) => x !== key)
    }
    await refresh(id)
  }

  const save = (s: Suggestion) =>
    act(s, {
      call: (id) => saveDiscovered(id, s.slug, scopes[s.slug] ?? 'project'),
      refused: 'Not saved',
      failed: 'Save failed',
      done: savedText,
      detail: 'It runs only after a trial or an approval.',
    })

  const decline = (s: Suggestion, kind: 'soft' | 'hard') =>
    act(s, {
      call: (id) => dismissDiscovered(id, s.slug, kind),
      refused: 'Not recorded',
      failed: 'Decline failed',
      done: () => (kind === 'hard' ? 'It will not be offered again' : 'Not offered for a while'),
    })

  const fmtTime = (ms: number) => new Date(ms).toLocaleString()
  const canRun = $derived(pickedProjects.length > 0 && pickedRoots.length > 0 && !running)
</script>

<Topbar active="discover">
  {#snippet title()}
    <span class="page-title">Discover</span>
  {/snippet}
</Topbar>

<PageScroll>
  <div class="page" data-testid="discover-page">
    <p class="muted intro">
      Finds step sequences your past agent sessions repeat and offers each as a playbook draft.
      Only the history of the projects you pick is read, on this machine, with no model and no
      network call. Tool arguments are never kept; what you asked is kept scrubbed and short.
    </p>

    {#if loadError}
      <p class="error" data-testid="discover-load-error">{loadError}</p>
    {/if}

    <form
      class="panel"
      onsubmit={(e) => {
        e.preventDefault()
        if (canRun) run()
      }}
    >
      <fieldset>
        <legend>Projects</legend>
        {#if !projects.length}
          <p class="muted">No registered project.</p>
        {/if}
        {#each projects as p (p.workspace_id)}
          <label class="choice">
            <input
              type="checkbox"
              checked={pickedProjects.includes(p.workspace_id)}
              onchange={() => (pickedProjects = toggle(pickedProjects, p.workspace_id))}
            />
            {p.name}
          </label>
        {/each}
      </fieldset>
      <fieldset>
        <legend>History</legend>
        {#if !roots.length}
          <p class="muted">No history root is configured.</p>
        {/if}
        {#each roots as r (r.name)}
          <label class="choice" class:off={!r.available}>
            <input
              type="checkbox"
              disabled={!r.available}
              checked={pickedRoots.includes(r.name)}
              onchange={() => (pickedRoots = toggle(pickedRoots, r.name))}
            />
            {r.name}
            <span class="muted">{r.agent}{r.available ? '' : ', not found'}</span>
          </label>
        {/each}
      </fieldset>
      <div class="run-row">
        <label class="days">
          Days back
          <input type="number" min="1" max="365" bind:value={days} />
        </label>
        <Button type="submit" disabled={!canRun} data-testid="discover-run">
          {#if running}<Spinner />{/if}
          Find repeats
        </Button>
      </div>
    </form>

    {#if view}
      <section class="result" data-testid="discover-result">
        <h2>
          {view.read.sessions}
          {view.read.sessions === 1 ? 'session' : 'sessions'} read in {projectsText(view.request.projects)}<span
            class="muted">, {view.request.since_days} days back</span
          >
        </h2>
        <ul class="muted read">
          {#each readLines(view.read) as line, i (i)}<li>{line}</li>{/each}
        </ul>
        {#if !view.signed}
          <p class="error">This result was not made by this installation: nothing is saved from it.</p>
        {/if}
        {#if view.dropped_drafts}
          <p class="muted">
            {view.dropped_drafts} drafts were left out: their text still looked like it held a secret.
          </p>
        {/if}
        {#if !view.suggestions.length}
          <p class="muted" data-testid="discover-empty">
            No repeated step sequence yet: a sequence counts after three sessions on two days.
          </p>
        {/if}
        {#each view.suggestions as s (s.slug)}
          <article class="card" data-testid="discover-card">
            <header class="card-head">
              <h3>{s.title}</h3>
              <span class="badge" class:done={s.status.state !== 'new'}>{statusText(s.status)}</span>
            </header>
            <ol class="steps">
              {#each s.steps as step, i (i)}<li>{step}</li>{/each}
            </ol>
            <p class="muted">{supportText(s)} · <span class="mono">{s.slug}</span></p>
            {#if s.effects.length}
              <p><span class="muted">effects:</span> {s.effects.join(', ')}</p>
            {/if}
            {#if s.notes?.includes('needs_profile')}
              <p class="muted">The project has no profile yet: the draft names one called main.</p>
            {/if}
            {#if s.examples.length}
              <div class="examples">
                <span class="muted">asked before it:</span>
                {#each s.examples as ex, i (i)}<blockquote>{ex.text}</blockquote>{/each}
              </div>
            {/if}
            <details>
              <summary>Seen in {s.sources.length} {s.sources.length === 1 ? 'place' : 'places'}</summary>
              <ul class="sources mono">
                {#each s.sources as src, i (i)}<li>{sourceText(src)}</li>{/each}
              </ul>
            </details>
            {#if canAct(view, s)}
              <div class="actions">
                <select
                  aria-label="Save in"
                  value={scopes[s.slug] ?? 'project'}
                  onchange={(e) =>
                    (scopes = { ...scopes, [s.slug]: (e.currentTarget as HTMLSelectElement).value as 'project' | 'global' })}
                >
                  <option value="project">this project</option>
                  <option value="global">every project</option>
                </select>
                <Button size="sm" disabled={busy.includes(busyKey(view.id, s.slug))} onclick={() => save(s)}>Save</Button>
                <Button size="sm" variant="outline" disabled={busy.includes(busyKey(view.id, s.slug))} onclick={() => decline(s, 'soft')}>
                  Not now
                </Button>
                <Button size="sm" variant="ghost" disabled={busy.includes(busyKey(view.id, s.slug))} onclick={() => decline(s, 'hard')}>
                  Never
                </Button>
              </div>
            {/if}
          </article>
        {/each}
        {#if view.suppressed.length}
          <div class="muted suppressed" data-testid="discover-suppressed">
            Left out, declined or saved before:
            {#each view.suppressed as x (x.slug)}<span class="mono">{x.slug} ({x.kind}, {x.scope})</span>{/each}
          </div>
        {/if}
      </section>
    {/if}

    {#if past.length}
      <section class="past">
        <h2>Earlier discoveries</h2>
        <ul>
          {#each past as d (d.id)}
            <li>
              <button type="button" class="link" onclick={() => open(d.id)}>{fmtTime(d.created_at)}</button>
              <span class="muted">
                {projectsText(d.request.projects)}: {d.suggestions} suggestions, {d.saved} saved, {d.dismissed} declined{d.signed
                  ? ''
                  : ', not signed here'}
              </span>
            </li>
          {/each}
        </ul>
      </section>
    {/if}
  </div>
</PageScroll>

<style>
  .page {
    display: flex;
    flex-direction: column;
    gap: 1.25rem;
    width: 100%;
    max-width: 56rem;
    margin: 0 auto;
    padding: 1.5rem 1rem;
    font-size: 0.875rem;
  }
  .page-title {
    font-size: 0.875rem;
    font-weight: 600;
  }
  .muted {
    color: var(--muted-foreground);
  }
  .error {
    color: var(--destructive);
  }
  .mono {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 0.75rem;
    overflow-wrap: anywhere;
  }
  .panel {
    display: flex;
    flex-wrap: wrap;
    gap: 1rem 2rem;
    padding: 1rem;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg, 8px);
  }
  fieldset {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    min-width: 12rem;
    border: 0;
    padding: 0;
    margin: 0;
  }
  legend {
    margin-bottom: 0.375rem;
    font-size: 0.75rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--muted-foreground);
  }
  .choice {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .choice.off {
    opacity: 0.6;
  }
  .run-row {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: 1rem;
    width: 100%;
  }
  .days {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    font-size: 0.75rem;
    color: var(--muted-foreground);
  }
  input[type='number'],
  select {
    padding: 0.375rem 0.5rem;
    border: 1px solid var(--input, var(--border));
    border-radius: var(--radius-md, 6px);
    background: var(--background);
    color: var(--foreground);
    font: inherit;
  }
  input[type='number'] {
    width: 6rem;
  }
  .result,
  .past {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  h2 {
    font-size: 0.9375rem;
    font-weight: 600;
  }
  .read {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    font-size: 0.75rem;
    overflow-wrap: anywhere;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding: 1rem;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg, 8px);
    background: var(--card, var(--background));
  }
  .card-head {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    justify-content: space-between;
    gap: 0.5rem;
  }
  h3 {
    min-width: 0;
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .badge {
    padding: 0 0.5rem;
    border: 1px solid var(--border);
    border-radius: 999px;
    font-size: 0.75rem;
    line-height: 1.25rem;
  }
  .badge.done {
    color: var(--muted-foreground);
  }
  .steps {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem;
    list-style: none;
    padding: 0;
  }
  .steps li {
    padding: 0.125rem 0.5rem;
    border-radius: var(--radius-sm, 4px);
    background: var(--muted);
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 0.75rem;
  }
  .examples {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  blockquote {
    margin: 0;
    padding-left: 0.75rem;
    border-left: 2px solid var(--border);
    overflow-wrap: anywhere;
  }
  summary {
    cursor: pointer;
    color: var(--muted-foreground);
  }
  .sources {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    margin-top: 0.25rem;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem;
  }
  .suppressed {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    font-size: 0.75rem;
  }
  .past ul {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .link {
    padding: 0;
    border: 0;
    background: none;
    color: var(--primary);
    text-decoration: underline;
    cursor: pointer;
    font: inherit;
  }
</style>
