<script lang="ts">
  import type { Snippet } from 'svelte'
  import { cn } from '$lib/utils'
  import BookMarked from '@lucide/svelte/icons/book-marked'
  import PlayCircle from '@lucide/svelte/icons/play-circle'
  import UserCog from '@lucide/svelte/icons/user-cog'
  import Plug from '@lucide/svelte/icons/plug'
  import LogOut from '@lucide/svelte/icons/log-out'
  import { Button } from '$lib/components/ui/button'
  import { auth, logout } from '$lib/auth'

  // The control appears only in server mode: on a keyless local dashboard
  // there is no session to end and the item would be dead weight.
  let authState = $state({ required: false, authenticated: true, checked: false })
  $effect(() => auth.subscribe((v) => (authState = v)))

  let {
    active = '',
    title,
    actions,
  }: {
    active?: 'playbooks' | 'runs' | 'profiles' | 'connectors' | ''
    title?: Snippet
    actions?: Snippet
  } = $props()

  const links = [
    { key: 'playbooks', label: 'Playbooks', href: '#/', icon: BookMarked },
    { key: 'runs', label: 'Runs', href: '#/runs', icon: PlayCircle },
    { key: 'profiles', label: 'Profiles', href: '#/profiles', icon: UserCog },
    { key: 'connectors', label: 'Connectors', href: '#/connectors', icon: Plug },
  ] as const
</script>

<header
  class="flex h-14 shrink-0 items-center gap-2 border-b border-border bg-background/95 px-3 backdrop-blur"
>
  <a href="#/" class="mr-1 flex items-center gap-2 font-semibold tracking-tight">
    <span
      class="flex size-6 items-center justify-center rounded-md bg-primary text-primary-foreground text-xs font-bold"
      >a</span
    >
    <span class="hidden sm:inline">apb</span>
  </a>
  <nav class="flex items-center gap-1">
    {#each links as l (l.key)}
      <a
        href={l.href}
        class={cn(
          'flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-sm font-medium transition-colors',
          active === l.key
            ? 'bg-accent text-accent-foreground'
            : 'text-muted-foreground hover:bg-accent/50 hover:text-foreground',
        )}
      >
        <l.icon class="size-4" />
        <span class="hidden sm:inline">{l.label}</span>
      </a>
    {/each}
  </nav>
  {#if title}
    <div class="ml-2 flex min-w-0 items-center gap-2 border-l border-border pl-3">
      {@render title()}
    </div>
  {/if}
  <div class="ml-auto flex items-center gap-2">
    {#if actions}{@render actions()}{/if}
    {#if authState.required && authState.authenticated}
      <Button variant="ghost" size="sm" onclick={() => logout()}>
        <LogOut class="size-4" />
        <span class="hidden sm:inline">Log out</span>
      </Button>
    {/if}
  </div>
</header>
