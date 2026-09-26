// Stale-tab detection. The dashboard is embedded in the apb binary, so a
// reinstall swaps the frontend and the API under any open tab. The server
// names its build on every response (`x-apb-build`, also `build_id` in
// /api/health) and in the shell this tab loaded (`<meta name="apb-build">`).
// When the two differ, this tab is running an old bundle against a new API:
// `staleBuild` flips and the app offers a reload.
//
// Imports nothing from `./api`: the fetch layer calls `noteServerBuild` on
// every response, and a cycle between the two would be an ordering hazard.
import { writable } from 'svelte/store'

export const BUILD_HEADER = 'x-apb-build'

/** True once the server was seen serving a different build than this tab's. */
export const staleBuild = writable(false)

/** The build this tab's shell was served by; null on the vite dev server. */
function ownBuild(): string | null {
  const doc = (globalThis as { document?: Document }).document
  return doc?.querySelector('meta[name="apb-build"]')?.getAttribute('content') ?? null
}

/** Compares a build id the server reported with this tab's own. */
export function noteServerBuild(served: string | null | undefined): void {
  const mine = ownBuild()
  if (mine && served && served !== mine) staleBuild.set(true)
}

/** Asks the server which build it runs now (focus, reconnect, a timer). */
export async function checkServerBuild(): Promise<void> {
  try {
    const res = await fetch('/api/health', { cache: 'no-store' })
    if (!res.ok) return
    const body = (await res.json()) as { build_id?: string }
    noteServerBuild(body.build_id ?? res.headers.get(BUILD_HEADER))
  } catch {
    // The server is restarting; the next check or API call settles it.
  }
}

/** Re-checks when the tab regains attention and once a minute while it is
 * visible. Returns the teardown. */
export function watchServerBuild(intervalMs = 60_000): () => void {
  const check = () => {
    if (document.visibilityState === 'visible') void checkServerBuild()
  }
  window.addEventListener('focus', check)
  document.addEventListener('visibilitychange', check)
  const timer = setInterval(check, intervalMs)
  return () => {
    window.removeEventListener('focus', check)
    document.removeEventListener('visibilitychange', check)
    clearInterval(timer)
  }
}
