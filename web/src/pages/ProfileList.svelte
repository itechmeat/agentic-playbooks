<script lang="ts">
  import { ApiError, deleteProfile, fetchProfiles, type ProfileSummary } from '../lib/api'
  import {
    ALL_PROJECTS,
    filterProfilesByProject,
    projectFilter,
    projectScopeItems,
  } from '../lib/projectfilter'
  import { profileColor } from '../lib/profilecolor'
  import { shortModel } from '../lib/format'
  import { subscribeChanges } from '../lib/ws'
  import Topbar from '$lib/components/Topbar.svelte'
  import PageScroll from '$lib/components/PageScroll.svelte'
  import ProjectFilter from '$lib/components/ProjectFilter.svelte'
  import PageHead from '$lib/components/PageHead.svelte'
  import StatusBar, { type StatusItem } from '$lib/components/StatusBar.svelte'
  import { Button, buttonVariants } from '$lib/components/ui/button'
  import * as Empty from '$lib/components/ui/empty'
  import * as AlertDialog from '$lib/components/ui/alert-dialog'
  import { Skeleton } from '$lib/components/ui/skeleton'
  import { toast } from 'svelte-sonner'
  import Plus from '@lucide/svelte/icons/plus'
  import Pencil from '@lucide/svelte/icons/pencil'
  import Trash2 from '@lucide/svelte/icons/trash-2'
  import UserCog from '@lucide/svelte/icons/user-cog'
  import ShieldAlert from '@lucide/svelte/icons/shield-alert'

  let items = $state<ProfileSummary[]>([])
  let loaded = $state(false)
  let deleting = $state<string | null>(null)
  let target = $state<ProfileSummary | null>(null)
  let confirmOpen = $state(false)
  let forceOpen = $state(false)

  // Options come from project-scope rows only; global rows have no project
  // identity of their own and would otherwise add a bogus option.
  const projectScopeProfiles = $derived(projectScopeItems(items))
  const filtered = $derived(filterProfilesByProject(items, $projectFilter))

  // Profiles grouped by owning project, plus a `global` group for the shared
  // global-scope store.
  const groups = $derived.by(() => {
    const m = new Map<string, { key: string; project: string; items: ProfileSummary[] }>()
    for (const p of filtered) {
      const k = p.scope === 'global' ? '@global' : p.workspace_id || '_'
      const label = p.scope === 'global' ? 'global' : p.project || 'this project'
      if (!m.has(k)) m.set(k, { key: k, project: label, items: [] })
      m.get(k)!.items.push(p)
    }
    return [...m.values()].sort((a, b) => a.project.localeCompare(b.project))
  })

  // The filtered project's name, for the heading and the top bar.
  const projectLabel = $derived(
    $projectFilter === ALL_PROJECTS
      ? ''
      : (projectScopeProfiles.find((p) => p.workspace_id === $projectFilter)?.project ?? ''),
  )
  const untrusted = $derived(filtered.filter((p) => !p.trusted).length)
  const statusItems = $derived.by((): StatusItem[] => [
    { label: 'profiles', value: filtered.length, tone: 'info' },
    { label: 'project', value: filtered.filter((p) => p.scope !== 'global').length, tone: 'violet' },
    { label: 'global', value: filtered.filter((p) => p.scope === 'global').length, tone: 'violet' },
    { label: 'untrusted', value: untrusted, tone: untrusted ? 'warning' : 'muted' },
  ])

  const cols = 'lg:grid-cols-[minmax(220px,1.4fr)_minmax(180px,1fr)_minmax(140px,1.2fr)_64px]'

  async function load() {
    try {
      items = await fetchProfiles()
    } catch (e) {
      toast.error('Failed to load profiles', { description: String(e) })
    } finally {
      loaded = true
    }
  }

  $effect(() => {
    load()
    return subscribeChanges(load)
  })

  const key = (p: ProfileSummary) => `${p.workspace_id}/${p.scope}/${p.name}`
  const editHref = (p: ProfileSummary) =>
    `#/profile-edit/${encodeURIComponent(p.workspace_id)}/${encodeURIComponent(p.scope)}/${encodeURIComponent(p.name)}`

  function askRemove(p: ProfileSummary) {
    target = p
    confirmOpen = true
  }

  async function doDelete(p: ProfileSummary, force: boolean) {
    deleting = key(p)
    try {
      await deleteProfile(p.name, p.scope, p.workspace_id, force)
      await load()
      toast.success(`Deleted profile "${p.name}"`)
      target = null
    } catch (e) {
      // A 409 from the server means the profile is still referenced by
      // playbooks; offer a force delete. Branch on the structured status
      // rather than matching substrings in the message.
      if (!force && e instanceof ApiError && e.status === 409) {
        forceOpen = true
      } else {
        toast.error('Delete failed', { description: String(e) })
      }
    } finally {
      deleting = null
    }
  }

  function confirmRemove() {
    confirmOpen = false
    if (target) doDelete(target, false)
  }
  function confirmForce() {
    forceOpen = false
    if (target) doDelete(target, true)
  }
</script>

<Topbar active="profiles" project={projectLabel} />

<PageScroll>
  <div class="w-full px-4 py-5 sm:px-5">
    <PageHead title="Profiles">
      {#snippet meta()}
        {#if projectLabel}<span>project · {projectLabel}</span>{/if}
        {#if loaded}
          <span class="text-muted-foreground">
            {filtered.length} profile{filtered.length === 1 ? '' : 's'}{#if untrusted}{` · ${untrusted} untrusted`}{/if}
          </span>
        {/if}
      {/snippet}
      {#snippet actions()}
        <ProjectFilter items={projectScopeProfiles} class="" />
        <Button href="#/profile-new" size="sm" class="max-sm:px-2">
          <Plus data-icon="inline-start" />
          <span class="max-sm:sr-only">New profile</span>
        </Button>
      {/snippet}
    </PageHead>

    {#if !loaded}
      <div class="flex flex-col gap-2.5">
        {#each Array(3) as _, i (i)}<Skeleton class="h-14 w-full" />{/each}
      </div>
    {:else if items.length === 0}
      <Empty.Root class="border border-dashed border-line-2 bg-panel">
        <Empty.Header>
          <Empty.Media variant="icon" class="border border-line-2 bg-transparent text-muted-foreground">
            <UserCog />
          </Empty.Media>
          <Empty.Title class="text-sm font-semibold">No profiles yet</Empty.Title>
          <Empty.Description>
            Profiles bind an executor (agent, model, role prompt, skills) to nodes.
          </Empty.Description>
        </Empty.Header>
        <Empty.Content>
          <Button href="#/profile-new" size="sm">
            <Plus data-icon="inline-start" />
            Create profile
          </Button>
        </Empty.Content>
      </Empty.Root>
    {:else}
      <div
        class="mb-2 hidden gap-4 px-3.5 text-[11px] tracking-[0.08em] text-muted-foreground uppercase lg:grid {cols}"
        aria-hidden="true"
      >
        <span>profile</span><span>executor</span><span>skills</span><span></span>
      </div>
      {#each groups as g (g.key)}
        <section class="mb-6">
          <h2 class="mb-2 px-0.5 text-[11px] tracking-[0.08em] text-muted-foreground uppercase">
            {g.project}
          </h2>
          <ul class="flex flex-col gap-2.5">
            {#each g.items as p (key(p))}
              <li
                class="relative grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-4 gap-y-1.5 border border-line-2 bg-panel px-3.5 py-3 transition-colors hover:border-(--k) {cols}"
                style:--k={profileColor(p.name)}
              >
                <div class="min-w-0">
                  <div class="flex min-w-0 flex-wrap items-center gap-2">
                    <a
                      href={editHref(p)}
                      class="legend-sq truncate font-semibold after:absolute after:inset-0 hover:underline focus-visible:outline-none focus-visible:after:outline focus-visible:after:outline-1 focus-visible:after:outline-violet"
                    >
                      {p.name}
                    </a>
                    {#if !p.trusted}
                      <span class="tag inline-flex items-center gap-1 text-amber">
                        <ShieldAlert class="size-3" />
                        untrusted
                      </span>
                    {/if}
                  </div>
                  {#if p.description}
                    <p class="prose-text truncate text-xs text-muted-foreground">{p.description}</p>
                  {/if}
                </div>
                <div class="col-span-full min-w-0 truncate text-xs text-fg-2 lg:col-span-1 lg:text-[13px]">
                  {p.agent} <span class="text-faint" aria-hidden="true">·</span> <span title={p.model}>{shortModel(p.model)}</span>
                </div>
                <div class="col-span-full flex min-w-0 flex-wrap gap-1 lg:col-span-1">
                  {#each p.skills as s (s)}
                    <span class="tag text-fg-2">{s}</span>
                  {:else}
                    <span class="text-[11px] text-muted-foreground">no skills</span>
                  {/each}
                </div>
                <div class="relative z-10 col-start-2 row-start-1 flex justify-end gap-0.5 lg:order-last lg:col-start-auto lg:row-start-auto">
                  <Button variant="ghost" size="icon-sm" href={editHref(p)} title="Edit" aria-label="Edit">
                    <Pencil />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    class="hover:text-destructive"
                    onclick={() => askRemove(p)}
                    disabled={deleting === key(p)}
                    title="Delete"
                    aria-label="Delete"
                  >
                    <Trash2 />
                  </Button>
                </div>
              </li>
            {/each}
          </ul>
        </section>
      {/each}
    {/if}
  </div>
</PageScroll>

<StatusBar items={statusItems} />

<AlertDialog.Root bind:open={confirmOpen}>
  <AlertDialog.Content>
    <AlertDialog.Header>
      <AlertDialog.Title>Delete profile?</AlertDialog.Title>
      <AlertDialog.Description>
        Delete "{target?.name}" ({target?.scope})? This cannot be undone.
      </AlertDialog.Description>
    </AlertDialog.Header>
    <AlertDialog.Footer>
      <AlertDialog.Cancel>Cancel</AlertDialog.Cancel>
      <AlertDialog.Action onclick={confirmRemove}>Delete</AlertDialog.Action>
    </AlertDialog.Footer>
  </AlertDialog.Content>
</AlertDialog.Root>

<AlertDialog.Root bind:open={forceOpen}>
  <AlertDialog.Content>
    <AlertDialog.Header>
      <AlertDialog.Title>Profile is referenced</AlertDialog.Title>
      <AlertDialog.Description>
        "{target?.name}" is referenced by one or more playbooks. Deleting it may break those
        playbooks. Delete anyway?
      </AlertDialog.Description>
    </AlertDialog.Header>
    <AlertDialog.Footer>
      <AlertDialog.Cancel>Cancel</AlertDialog.Cancel>
      <AlertDialog.Action
        onclick={confirmForce}
        class={buttonVariants({ variant: 'destructive' })}
      >
        Force delete
      </AlertDialog.Action>
    </AlertDialog.Footer>
  </AlertDialog.Content>
</AlertDialog.Root>
