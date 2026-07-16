<script lang="ts">
  import PlaybookList from './pages/PlaybookList.svelte'
  import PlaybookView from './pages/PlaybookView.svelte'
  import PlaybookEdit from './pages/PlaybookEdit.svelte'
  import RunList from './pages/RunList.svelte'
  import RunView from './pages/RunView.svelte'
  import ProfileList from './pages/ProfileList.svelte'
  import ProfileEdit from './pages/ProfileEdit.svelte'
  import { Toaster } from '$lib/components/ui/sonner'
  import { ModeWatcher } from 'mode-watcher'

  let hash = $state(location.hash)
  $effect(() => {
    const onHash = () => (hash = location.hash)
    window.addEventListener('hashchange', onHash)
    return () => window.removeEventListener('hashchange', onHash)
  })

  // decodeURIComponent throws on malformed percent-encoding (e.g. a lone `%`).
  // A bad hash segment must not blow up route parsing, so fall back to the raw
  // segment when decoding fails.
  const dec = (s: string) => {
    try {
      return decodeURIComponent(s)
    } catch {
      return s
    }
  }

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
    return base
  })
</script>

{#if route.page === 'new'}
  <PlaybookEdit id="" workspace="" />
{:else if route.page === 'edit'}
  <PlaybookEdit id={route.id} workspace={route.workspace} />
{:else if route.page === 'playbook'}
  <PlaybookView id={route.id} workspace={route.workspace} />
{:else if route.page === 'run'}
  <RunView id={route.id} workspace={route.workspace} />
{:else if route.page === 'runs'}
  <RunList />
{:else if route.page === 'profiles'}
  <ProfileList />
{:else if route.page === 'profile-new'}
  <ProfileEdit name="" scope="project" workspace="" />
{:else if route.page === 'profile-edit'}
  <ProfileEdit name={route.name} scope={route.scope} workspace={route.workspace} />
{:else}
  <PlaybookList />
{/if}

<ModeWatcher />
<Toaster richColors closeButton position="bottom-right" />
