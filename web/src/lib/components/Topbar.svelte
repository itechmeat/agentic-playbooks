<script lang="ts">
  import type { Snippet } from 'svelte'
  import BookMarked from '@lucide/svelte/icons/book-marked'
  import PlayCircle from '@lucide/svelte/icons/play-circle'
  import UserCog from '@lucide/svelte/icons/user-cog'
  import Plug from '@lucide/svelte/icons/plug'
  import ShieldCheck from '@lucide/svelte/icons/shield-check'
  import LogOut from '@lucide/svelte/icons/log-out'
  import { Button } from '$lib/components/ui/button'
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
  }: {
    active?: 'playbooks' | 'runs' | 'profiles' | 'connectors' | 'trust' | ''
    title?: Snippet
    actions?: Snippet
  } = $props()

  const links = [
    { key: 'playbooks', label: 'Playbooks', href: '#/', icon: BookMarked },
    { key: 'runs', label: 'Runs', href: '#/runs', icon: PlayCircle },
    { key: 'profiles', label: 'Profiles', href: '#/profiles', icon: UserCog },
    { key: 'connectors', label: 'Connectors', href: '#/connectors', icon: Plug },
    { key: 'trust', label: 'Approvals', href: '#/trust', icon: ShieldCheck },
  ] as const
</script>

<!-- Plain CSS over the app's variables. The bar wraps: on a narrow window
     (a page with many actions already at 1280px) the actions move to their
     own line instead of running off the screen. -->
<header class="topbar">
  <a href="#/" class="brand">
    <span class="brand-mark">a</span>
    <span class="wide-only">apb</span>
  </a>
  <nav class="nav">
    {#each links as l (l.key)}
      <a href={l.href} class="nav-link" class:active={active === l.key}>
        <l.icon class="nav-icon" />
        <span class="wide-only">{l.label}</span>
      </a>
    {/each}
  </nav>
  {#if title}
    <div class="title">
      {@render title()}
    </div>
  {/if}
  <div class="actions">
    {#if actions}{@render actions()}{/if}
    {#if authSnapshot.required && authSnapshot.authenticated}
      <Button variant="ghost" size="sm" disabled={loggingOut} onclick={doLogout}>
        <LogOut class="nav-icon" />
        <span class="wide-only">Log out</span>
      </Button>
    {/if}
  </div>
</header>

<style>
  .topbar {
    display: flex;
    flex-wrap: wrap;
    flex-shrink: 0;
    align-items: center;
    gap: 8px;
    min-height: 3.5rem;
    padding: 8px 12px;
    border-bottom: 1px solid var(--border);
    background: color-mix(in oklab, var(--background) 95%, transparent);
    backdrop-filter: blur(8px);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-right: 4px;
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  .brand-mark {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    border-radius: var(--radius-md, 6px);
    background: var(--primary);
    color: var(--primary-foreground);
    font-size: 12px;
    font-weight: 700;
  }
  .nav {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .nav-link {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 10px;
    border-radius: var(--radius-md, 6px);
    color: var(--muted-foreground);
    font-size: 14px;
    font-weight: 500;
    transition: background-color 0.15s, color 0.15s;
  }
  .nav-link:hover {
    background: color-mix(in oklab, var(--accent) 50%, transparent);
    color: var(--foreground);
  }
  .nav-link.active {
    background: var(--accent);
    color: var(--accent-foreground);
  }
  .topbar :global(.nav-icon) {
    width: 16px;
    height: 16px;
  }
  .title {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    margin-left: 8px;
    padding-left: 12px;
    border-left: 1px solid var(--border);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: flex-end;
    gap: 8px;
    min-width: 0;
    margin-left: auto;
  }
  @media (max-width: 640px) {
    .wide-only {
      display: none;
    }
  }
</style>
