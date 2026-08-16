// Server-mode authentication state for the dashboard (spec
// 2026-08-16-server-mode-design). The raw API key is never stored: the login
// screen posts it once and the server answers with an HttpOnly session cookie
// the browser attaches on its own, so this module holds only two booleans.
//
// This module deliberately uses the bare `fetch` and imports nothing from
// `./api`: the fetch layer imports the marker header and the 401 hook from
// here, and a cycle between the two would be a real ordering hazard.
import { writable } from 'svelte/store'

/** Marker header the server requires on cookie-authenticated writes. A
 * cross-site form cannot set it, which is the second CSRF layer behind
 * SameSite=Lax. Harmless when auth is off. */
export const XRW_HEADER = 'x-requested-with'
export const XRW_VALUE = 'apb-dashboard'

/** Headers every dashboard request carries, merged over the caller's own. */
export function apiHeaders(extra?: Record<string, string>): Record<string, string> {
  return { ...(extra ?? {}), [XRW_HEADER]: XRW_VALUE }
}

export interface AuthSnapshot {
  /** The server has at least one API key, so credentials are enforced. */
  required: boolean
  /** This browser currently holds a valid session (or auth is off). */
  authenticated: boolean
  /** A status read has completed at least once. */
  checked: boolean
}

/** Optimistic default: the local, keyless dashboard is the common case, and
 * starting in the authenticated state keeps it from flashing a login screen
 * while the first status read is in flight. A 401 corrects it immediately. */
export const auth = writable<AuthSnapshot>({
  required: false,
  authenticated: true,
  checked: false,
})

/** Reads GET /api/auth/status. An unreachable or older server (no such route)
 * is treated as auth off, so a dashboard built before server mode keeps
 * working against a newer frontend and vice versa. */
export async function refreshAuthStatus(): Promise<AuthSnapshot> {
  let next: AuthSnapshot = { required: false, authenticated: true, checked: true }
  try {
    const res = await fetch('/api/auth/status', { headers: apiHeaders() })
    if (res.ok) {
      const body = (await res.json()) as { auth_required?: boolean; authenticated?: boolean }
      next = {
        required: body.auth_required === true,
        authenticated: body.authenticated === true,
        checked: true,
      }
    }
  } catch {
    // Network failure: keep the permissive default rather than locking the
    // operator out of a dashboard that may simply be restarting.
  }
  auth.set(next)
  return next
}

/** Exchanges a pasted API key for a session cookie. The key is not kept
 * anywhere after this call returns. */
export async function login(key: string): Promise<{ ok: boolean; message?: string }> {
  let res: Response
  try {
    res = await fetch('/api/auth/login', {
      method: 'POST',
      headers: apiHeaders({ 'content-type': 'application/json' }),
      body: JSON.stringify({ key }),
    })
  } catch {
    return { ok: false, message: 'The server could not be reached.' }
  }
  if (res.ok) {
    await refreshAuthStatus()
    return { ok: true }
  }
  if (res.status === 429) {
    return { ok: false, message: 'Too many attempts. Wait a minute and try again.' }
  }
  return { ok: false, message: 'That key was not accepted.' }
}

/** Ends the session and returns the app to the login screen. */
export async function logout(): Promise<void> {
  try {
    await fetch('/api/auth/logout', { method: 'POST', headers: apiHeaders() })
  } catch {
    // A failed logout still drops the local view of the session below.
  }
  await refreshAuthStatus()
}

/** Called by the fetch layer on any 401: the session expired or never
 * existed, so the app shows the login screen instead of a generic error. */
export function markUnauthenticated(): void {
  auth.set({ required: true, authenticated: false, checked: true })
}
