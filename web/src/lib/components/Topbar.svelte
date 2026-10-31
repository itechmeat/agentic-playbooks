<script lang="ts">
  import type { Snippet } from 'svelte'
  import BookMarked from '@lucide/svelte/icons/book-marked'
  import PlayCircle from '@lucide/svelte/icons/play-circle'
  import UserCog from '@lucide/svelte/icons/user-cog'
  import Plug from '@lucide/svelte/icons/plug'
  import ShieldCheck from '@lucide/svelte/icons/shield-check'
  import Repeat from '@lucide/svelte/icons/repeat'
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
    active?: 'playbooks' | 'runs' | 'profiles' | 'connectors' | 'trust' | 'discover' | ''
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
    { key: 'discover', label: 'Discover', href: '#/discover', icon: Repeat },
  ] as const

  // Theme switch. mode-watcher persists the choice; dark is the default
  // (App.svelte mounts ModeWatcher with defaultMode="dark").
  const themes = [
    { mode: 'dark', label: 'Dark theme', icon: Moon },
    { mode: 'light', label: 'Light theme', icon: Sun },
    { mode: 'system', label: 'System theme', icon: Monitor },
  ] as const
</script>

<!-- Plain CSS over the app's variables. The bar wraps: on a narrow window
     (a page with many actions already at 1280px) the actions move to their
     own line instead of running off the screen. -->
<header class="topbar">
  <a href="#/" class="brand" aria-label="apb home">
    <ApbLogo class="brand-logo" />
    <span class="brand-name">apb</span>
  </a>
  <nav class="nav">
    {#each links as l (l.key)}
      <a
        href={l.href}
        class="nav-link"
        class:active={active === l.key}
        aria-current={active === l.key ? 'page' : undefined}
      >
        <!-- On a narrow window the icon stands alone; the label stays for
             screen readers, so the link keeps its name. -->
        <l.icon class="nav-icon narrow-only" aria-hidden="true" />
        <span class="wide-label">{l.label}</span>
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
    {#if project}
      <span class="project-chip" title={project}>project · {project}</span>
    {/if}
    <div class="theme-switch" role="group" aria-label="Color theme">
      {#each themes as t (t.mode)}
        <button
          type="button"
          class="theme-btn"
          class:active={userPrefersMode.current === t.mode}
          aria-label={t.label}
          aria-pressed={userPrefersMode.current === t.mode}
          title={t.label}
          onclick={() => setMode(t.mode)}
        >
          <t.icon class="theme-icon" />
        </button>
      {/each}
    </div>
    {#if authSnapshot.required && authSnapshot.authenticated}
      <Button variant="ghost" size="sm" disabled={loggingOut} onclick={doLogout}>
        <LogOut class="nav-icon" aria-hidden="true" />
        <span class="wide-label">Log out</span>
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
    gap: 8px 20px;
    min-height: 3rem;
    padding: 6px 16px;
    border-bottom: 1px solid var(--c-line);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--foreground);
  }
  .topbar :global(.brand-logo) {
    width: 20px;
    height: 20px;
  }
  .brand-name {
    font-weight: 700;
    letter-spacing: 0.14em;
  }
  .nav {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 2px;
  }
  .nav-link {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 2px 10px;
    border: 1px solid transparent;
    color: var(--muted-foreground);
    font-size: 12px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    transition: color 0.15s, background-color 0.15s, border-color 0.15s;
  }
  .nav-link:hover {
    color: var(--foreground);
  }
  .nav-link.active {
    border-color: var(--c-line-2);
    background: var(--c-panel-2);
    color: var(--foreground);
  }
  .topbar :global(.nav-icon) {
    width: 14px;
    height: 14px;
  }
  .topbar :global(.narrow-only) {
    display: none;
  }
  .title {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    padding-left: 12px;
    border-left: 1px solid var(--c-line);
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
  .project-chip {
    max-width: 12rem;
    overflow: hidden;
    padding: 1px 8px;
    border: 1px solid var(--c-line-2);
    color: var(--c-fg-2);
    font-size: 12px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .theme-switch {
    display: flex;
    border: 1px solid var(--c-line);
  }
  .theme-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    color: var(--muted-foreground);
    transition: color 0.15s, background-color 0.15s;
  }
  .theme-btn:hover {
    color: var(--foreground);
  }
  .theme-btn.active {
    background: var(--c-line);
    color: var(--foreground);
  }
  .topbar :global(.theme-icon) {
    width: 14px;
    height: 14px;
  }
  @media (max-width: 767px) {
    .project-chip {
      display: none;
    }
  }
  @media (max-width: 639px) {
    .topbar {
      gap: 8px 12px;
      padding: 6px 12px;
    }
    .brand-name {
      display: none;
    }
    .nav-link {
      padding: 2px 8px;
    }
    .topbar :global(.narrow-only) {
      display: block;
    }
    .wide-label {
      position: absolute;
      width: 1px;
      height: 1px;
      overflow: hidden;
      clip: rect(0 0 0 0);
      white-space: nowrap;
    }
  }
</style>
