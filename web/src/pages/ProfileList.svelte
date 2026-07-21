<script lang="ts">
  import { ApiError, deleteProfile, fetchProfiles, type ProfileSummary } from '../lib/api'
  import { subscribeChanges } from '../lib/ws'
  import Topbar from '$lib/components/Topbar.svelte'
  import PageScroll from '$lib/components/PageScroll.svelte'
  import { Button } from '$lib/components/ui/button'
  import { Badge } from '$lib/components/ui/badge'
  import * as Card from '$lib/components/ui/card'
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

  // Profiles grouped by owning project, plus a `global` group for the shared
  // global-scope store.
  const groups = $derived.by(() => {
    const m = new Map<string, { key: string; project: string; items: ProfileSummary[] }>()
    for (const p of items) {
      const k = p.scope === 'global' ? '@global' : p.workspace_id || '_'
      const label = p.scope === 'global' ? 'global' : p.project || 'this project'
      if (!m.has(k)) m.set(k, { key: k, project: label, items: [] })
      m.get(k)!.items.push(p)
    }
    return [...m.values()].sort((a, b) => a.project.localeCompare(b.project))
  })

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

<Topbar active="profiles">
  {#snippet actions()}
    <Button href="#/profile-new" size="sm" class="max-sm:px-2">
      <Plus data-icon="inline-start" />
      <span class="max-sm:sr-only">Create</span>
    </Button>
  {/snippet}
</Topbar>

<PageScroll>
  <div class="mx-auto w-full max-w-4xl px-4 py-6">
    {#if !loaded}
      <div class="flex flex-col gap-3">
        {#each Array(3) as _, i (i)}<Skeleton class="h-20 w-full" />{/each}
      </div>
    {:else if items.length === 0}
      <Empty.Root class="border border-dashed">
        <Empty.Header>
          <Empty.Media variant="icon"><UserCog /></Empty.Media>
          <Empty.Title>No profiles yet</Empty.Title>
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
      {#each groups as g (g.key)}
        <section class="mb-8">
          <h2
            class="mb-3 text-xs font-semibold uppercase tracking-wider text-muted-foreground"
          >
            {g.project}
          </h2>
          <div class="flex flex-col gap-3">
            {#each g.items as p (key(p))}
              <Card.Root class="transition-colors hover:border-ring/40">
                <Card.Header>
                  <div class="flex flex-wrap items-center gap-2">
                    <Card.Title class="text-base">
                      <a href={editHref(p)} class="hover:underline">
                        {p.name}
                      </a>
                    </Card.Title>
                    {#if !p.trusted}
                      <Badge variant="outline" class="gap-1 border-warning/30 bg-warning/15 text-warning">
                        <ShieldAlert class="size-3" />
                        untrusted
                      </Badge>
                    {/if}
                  </div>
                  <Card.Description class="font-mono text-xs">
                    {p.agent} · {p.model}{p.skills.length
                      ? ` · skills: ${p.skills.join(', ')}`
                      : ''}
                  </Card.Description>
                  <Card.Action class="flex gap-1">
                    <Button variant="ghost" size="sm" class="max-sm:px-2" href={editHref(p)}>
                      <Pencil data-icon="inline-start" />
                      <span class="max-sm:sr-only">Edit</span>
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      class="max-sm:px-2 text-muted-foreground hover:text-destructive"
                      onclick={() => askRemove(p)}
                      disabled={deleting === key(p)}
                    >
                      <Trash2 data-icon="inline-start" />
                      <span class="max-sm:sr-only">Delete</span>
                    </Button>
                  </Card.Action>
                </Card.Header>
                {#if p.description}
                  <Card.Content class="text-sm text-muted-foreground">
                    {p.description}
                  </Card.Content>
                {/if}
              </Card.Root>
            {/each}
          </div>
        </section>
      {/each}
    {/if}
  </div>
</PageScroll>

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
        class="bg-destructive text-white hover:bg-destructive/90"
      >
        Force delete
      </AlertDialog.Action>
    </AlertDialog.Footer>
  </AlertDialog.Content>
</AlertDialog.Root>
