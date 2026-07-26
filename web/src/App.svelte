<script lang="ts">
  // The landing page is imported eagerly: it is what the dashboard opens on,
  // and it carries none of the heavy dependencies. Every other page is a
  // dynamic import, so the graph canvas (@xyflow + dagre + d3), the YAML
  // document model, and the connector screens only reach the browser when a
  // route actually needs them instead of riding along on first paint.
  import PlaybookList from './pages/PlaybookList.svelte'
  import ChunkError from '$lib/components/ChunkError.svelte'
  import ChunkPending from '$lib/components/ChunkPending.svelte'
  import { Toaster } from '$lib/components/ui/sonner'
  import { connectorRouteName, decodeSegment } from '$lib/route'
  import { ModeWatcher } from 'mode-watcher'

  let hash = $state(location.hash)
  $effect(() => {
    const onHash = () => (hash = location.hash)
    window.addEventListener('hashchange', onHash)
    return () => window.removeEventListener('hashchange', onHash)
  })

  // One dynamic import per page. Grouped by component, so a route that shares
  // a page with another (new/edit, profile-new/profile-edit) shares its chunk.
  const loadPlaybookView = () => import('./pages/PlaybookView.svelte')
  const loadPlaybookEdit = () => import('./pages/PlaybookEdit.svelte')
  const loadRunView = () => import('./pages/RunView.svelte')
  const loadRunList = () => import('./pages/RunList.svelte')
  const loadProfileList = () => import('./pages/ProfileList.svelte')
  const loadProfileEdit = () => import('./pages/ProfileEdit.svelte')
  const loadConnectorList = () => import('./pages/ConnectorList.svelte')
  const loadConnectorView = () => import('./pages/ConnectorView.svelte')

  const dec = decodeSegment

  // Routes carry the owning project so the global dashboard can address a
  // playbook/run in any project: #/playbook/<workspace>/<id>, #/edit/<ws>/<id>,
  // #/run/<ws>/<id>. #/new opens the editor with a project picker.
  function wsId(rest: string): { workspace: string; id: string } {
    const slash = rest.indexOf('/')
    if (slash < 0) return { workspace: dec(rest), id: '' }
    return { workspace: dec(rest.slice(0, slash)), id: dec(rest.slice(slash + 1)) }
  }

  // #/profile-edit/<workspace>/<scope>/<name> - workspace is empty for global scope.
  function profileRef(rest: string): { workspace: string; scope: string; name: string } {
    const parts = rest.split('/')
    return {
      workspace: dec(parts[0] ?? ''),
      scope: dec(parts[1] ?? 'project'),
      name: dec(parts.slice(2).join('/')),
    }
  }

  const route = $derived.by(() => {
    const h = hash
    const base = { page: 'playbooks', workspace: '', id: '', scope: 'project', name: '' }
    if (h === '#/new') return { ...base, page: 'new' }
    if (h.startsWith('#/edit/')) return { ...base, page: 'edit', ...wsId(h.slice(7)) }
    if (h.startsWith('#/playbook/')) return { ...base, page: 'playbook', ...wsId(h.slice(11)) }
    if (h.startsWith('#/run/')) return { ...base, page: 'run', ...wsId(h.slice(6)) }
    if (h.startsWith('#/runs')) return { ...base, page: 'runs' }
    if (h === '#/profiles') return { ...base, page: 'profiles' }
    if (h === '#/profile-new') return { ...base, page: 'profile-new' }
    if (h.startsWith('#/profile-edit/'))
      return { ...base, page: 'profile-edit', ...profileRef(h.slice(15)) }
    if (h === '#/connectors') return { ...base, page: 'connectors' }
    // Connectors are machine-wide, so this route carries no workspace and
    // cannot reuse `wsId` (which would read the name as the workspace).
    if (h.startsWith('#/connector/'))
      return { ...base, page: 'connector', name: connectorRouteName(h.slice(12)) }
    return base
  })
</script>

<!-- Each arm awaits its own page chunk. The props stay written out per route,
     so the compiler still checks them; a spread through one dynamic component
     would have to be cast to `any` and would check nothing. Every arm carries
     all three branches: a chunk in flight, and a chunk that never arrives,
     would otherwise both render nothing and leave the route silently blank. -->
{#if route.page === 'new'}
  {#await loadPlaybookEdit()}
    <ChunkPending />
  {:then { default: Page }}
    <Page id="" workspace="" />
  {:catch error}<ChunkError {error} />{/await}
{:else if route.page === 'edit'}
  {#await loadPlaybookEdit()}
    <ChunkPending />
  {:then { default: Page }}
    <Page id={route.id} workspace={route.workspace} />
  {:catch error}<ChunkError {error} />{/await}
{:else if route.page === 'playbook'}
  {#await loadPlaybookView()}
    <ChunkPending />
  {:then { default: Page }}
    <Page id={route.id} workspace={route.workspace} />
  {:catch error}<ChunkError {error} />{/await}
{:else if route.page === 'run'}
  {#await loadRunView()}
    <ChunkPending />
  {:then { default: Page }}
    <Page id={route.id} workspace={route.workspace} />
  {:catch error}<ChunkError {error} />{/await}
{:else if route.page === 'runs'}
  {#await loadRunList()}
    <ChunkPending />
  {:then { default: Page }}
    <Page />
  {:catch error}<ChunkError {error} />{/await}
{:else if route.page === 'profiles'}
  {#await loadProfileList()}
    <ChunkPending />
  {:then { default: Page }}
    <Page />
  {:catch error}<ChunkError {error} />{/await}
{:else if route.page === 'profile-new'}
  {#await loadProfileEdit()}
    <ChunkPending />
  {:then { default: Page }}
    <Page name="" scope="project" workspace="" />
  {:catch error}<ChunkError {error} />{/await}
{:else if route.page === 'profile-edit'}
  {#await loadProfileEdit()}
    <ChunkPending />
  {:then { default: Page }}
    <Page name={route.name} scope={route.scope} workspace={route.workspace} />
  {:catch error}<ChunkError {error} />{/await}
{:else if route.page === 'connectors'}
  {#await loadConnectorList()}
    <ChunkPending />
  {:then { default: Page }}
    <Page />
  {:catch error}<ChunkError {error} />{/await}
{:else if route.page === 'connector'}
  {#await loadConnectorView()}
    <ChunkPending />
  {:then { default: Page }}
    <Page name={route.name} />
  {:catch error}<ChunkError {error} />{/await}
{:else}
  <PlaybookList />
{/if}

<ModeWatcher />
<Toaster richColors closeButton position="bottom-right" />
