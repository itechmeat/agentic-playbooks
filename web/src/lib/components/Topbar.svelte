<script lang="ts">
  import type { Snippet } from 'svelte'
  import { cn } from '$lib/utils'
  import BookMarked from '@lucide/svelte/icons/book-marked'
  import PlayCircle from '@lucide/svelte/icons/play-circle'
  import UserCog from '@lucide/svelte/icons/user-cog'
  import Plug from '@lucide/svelte/icons/plug'
  import ShieldCheck from '@lucide/svelte/icons/shield-check'
  import LogOut from '@lucide/svelte/icons/log-out'
  import Moon from '@lucide/svelte/icons/moon'
  import Sun from '@lucide/svelte/icons/sun'
  import Monitor from '@lucide/svelte/icons/monitor'
  import { setMode, userPrefersMode } from 'mode-watcher'
  import { Button } from '$lib/components/ui/button'
  import ApbLogo from '$lib/components/ApbLogo.svelte'
  import { authState, logout } from '$lib/auth.svelte'

  // The control appears only in server mode: on a keyless local dashboard
  // there is no session to end and the item would be dead weight.
  const authSnapshot = authState()
  let loggingOut = $state(false)

  async function doLogout() {
    if (loggingOut) return
    loggingOut = true
    try {
      await logout()
    } finally {
      loggingOut = false
    }
  }

  let {
    active = '',
    title,
    actions,
    project = '',
  }: {
    active?: 'playbooks' | 'runs' | 'profiles' | 'connectors' | 'trust' | ''
    title?: Snippet
    actions?: Snippet
    /** Optional project name shown as a chip on the right. */
    project?: string
  } = $props()

  const links = [
    { key: 'playbooks', label: 'Playbooks', href: '#/', icon: BookMarked },
    { key: 'runs', label: 'Runs', href: '#/runs', icon: PlayCircle },
    { key: 'profiles', label: 'Profiles', href: '#/profiles', icon: UserCog },
    { key: 'connectors', label: 'Connectors', href: '#/connectors', icon: Plug },
    { key: 'trust', label: 'Approvals', href: '#/trust', icon: ShieldCheck },
  ] as const

  // Theme switch. mode-watcher persists the choice; dark is the default
  // (App.svelte mounts ModeWatcher with defaultMode="dark").
  const themes = [
    { mode: 'dark', label: 'Dark theme', icon: Moon },
    { mode: 'light', label: 'Light theme', icon: Sun },
    { mode: 'system', label: 'System theme', icon: Monitor },
  ] as const
</script>

<header class="flex h-12 shrink-0 items-center gap-3 border-b border-line px-3 sm:gap-5 sm:px-4">
  <a href="#/" class="flex items-center gap-2 text-foreground" aria-label="apb home">
    <ApbLogo class="size-5" />
    <span class="hidden font-bold tracking-[0.14em] sm:inline">apb</span>
  </a>
  <nav class="flex items-center gap-0.5">
    {#each links as l (l.key)}
      <a
        href={l.href}
        aria-current={active === l.key ? 'page' : undefined}
        class={cn(
          'flex items-center gap-1.5 border px-2 py-0.5 text-xs tracking-[0.08em] uppercase transition-colors sm:px-2.5',
          active === l.key
            ? 'border-line-2 bg-panel-2 text-foreground'
            : 'border-transparent text-muted-foreground hover:text-foreground',
        )}
      >
        <!-- Below sm the icon stands alone; the label stays for screen
             readers (sr-only), so the link keeps its name. -->
        <l.icon class="size-3.5 sm:hidden" aria-hidden="true" />
        <span class="max-sm:sr-only">{l.label}</span>
      </a>
    {/each}
  </nav>
  {#if title}
    <div class="flex min-w-0 items-center gap-2 border-l border-line pl-3">
      {@render title()}
    </div>
  {/if}
  <div class="ml-auto flex items-center gap-2">
    {#if actions}{@render actions()}{/if}
    {#if project}
      <span
        class="hidden max-w-48 truncate border border-line-2 px-2 py-px text-xs text-fg-2 md:inline"
        title={project}>project · {project}</span
      >
    {/if}
    <div class="flex border border-line" role="group" aria-label="Color theme">
      {#each themes as t (t.mode)}
        <button
          type="button"
          class={cn(
            'flex size-6 items-center justify-center transition-colors',
            userPrefersMode.current === t.mode
              ? 'bg-line text-foreground'
              : 'text-muted-foreground hover:text-foreground',
          )}
          aria-label={t.label}
          aria-pressed={userPrefersMode.current === t.mode}
          title={t.label}
          onclick={() => setMode(t.mode)}
        >
          <t.icon class="size-3.5" />
        </button>
      {/each}
    </div>
    {#if authSnapshot.required && authSnapshot.authenticated}
      <Button variant="ghost" size="sm" disabled={loggingOut} onclick={doLogout}>
        <LogOut class="size-4" aria-hidden="true" />
        <span class="max-sm:sr-only">Log out</span>
      </Button>
    {/if}
  </div>
</header>
