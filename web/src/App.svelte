<script lang="ts">
  import PlaybookList from './pages/PlaybookList.svelte'
  import PlaybookView from './pages/PlaybookView.svelte'
  import PlaybookEdit from './pages/PlaybookEdit.svelte'
  import RunList from './pages/RunList.svelte'
  import RunView from './pages/RunView.svelte'

  let hash = $state(location.hash)
  $effect(() => {
    const onHash = () => (hash = location.hash)
    window.addEventListener('hashchange', onHash)
    return () => window.removeEventListener('hashchange', onHash)
  })

  const route = $derived.by(() => {
    if (hash === '#/new') return { page: 'new', id: '' }
    if (hash.startsWith('#/edit/')) return { page: 'edit', id: decodeURIComponent(hash.slice(7)) }
    if (hash.startsWith('#/playbook/')) return { page: 'playbook', id: decodeURIComponent(hash.slice(11)) }
    if (hash.startsWith('#/run/')) return { page: 'run', id: decodeURIComponent(hash.slice(6)) }
    if (hash.startsWith('#/runs')) return { page: 'runs', id: '' }
    return { page: 'playbooks', id: '' }
  })
</script>

{#if route.page === 'new' || route.page === 'edit'}
  <PlaybookEdit id={route.id} />
{:else if route.page === 'playbook'}
  <PlaybookView id={route.id} />
{:else if route.page === 'run'}
  <RunView id={route.id} />
{:else if route.page === 'runs'}
  <RunList />
{:else}
  <PlaybookList />
{/if}
