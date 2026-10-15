// Operator pause (issue #200): the words and the rules the run page and the
// run list share. The server sends one `pause` object (reason, by, mode,
// since, until, pausing); everything shown about it is derived here.

import type { PauseInfo } from './types'
import type { PauseRequestBody } from './api'
import { isTerminalRunStatus } from './status'

// A pause is shown as one of two words: `paused` once the run holds no
// process, `pausing` while a live driver still drains toward a queued pause.
export function pauseWord(pause: PauseInfo | null | undefined): 'paused' | 'pausing' | null {
  if (!pause) return null
  return pause.pausing ? 'pausing' : 'paused'
}

// An epoch-milliseconds time as RFC 3339. A value past what a Date holds
// (about 275,000 years out) is shown as the number it is, not dropped.
export function when(ms: number | undefined): string | null {
  if (ms === undefined || !Number.isFinite(ms)) return null
  const date = new Date(ms)
  if (Number.isNaN(date.getTime())) return `${ms} ms after the epoch`
  return date.toISOString().replace('.000Z', 'Z')
}

// "by operator, since 2026-10-15T20:00:00Z, until ...": the parts after the
// reason, in the order `apb runs` prints them.
export function pauseDetails(pause: PauseInfo): string[] {
  const parts = [`by ${pause.by}`]
  if (pause.pausing) parts.push(`mode ${pause.mode}`)
  const since = when(pause.since)
  if (since) parts.push(`since ${since}`)
  const until = when(pause.until)
  if (until) parts.push(`until ${until}`)
  if (pause.keep_lock) parts.push('lock kept')
  return parts
}

// The one-line marker of the run list and the badge tooltip.
export function pauseLine(pause: PauseInfo): string {
  return `${pauseWord(pause)} (${[pause.reason, ...pauseDetails(pause)].join(', ')})`
}

// The Pause button: a run that is running (or queued) and has no pause
// queued already. A paused or finished run has nothing to pause.
export function canPause(status: string, pause: PauseInfo | null | undefined): boolean {
  const s = (status ?? '').toLowerCase()
  return (s === 'running' || s === 'created') && !pause
}

// The Resume button: a paused run, or a non-terminal run whose driver is gone.
export function canResume(status: string, driverDead: boolean): boolean {
  const s = (status ?? '').toLowerCase()
  if (isTerminalRunStatus(s)) return false
  return s === 'paused' || driverDead
}

export interface PauseForm {
  mode: 'drain' | 'now'
  reason: string
  until: string
  keepLock: boolean
}

// The request body of POST /api/runs/{id}/pause: empty fields are left out,
// so the server applies its defaults (drain, no reason, no until).
export function pauseBody(form: PauseForm): PauseRequestBody {
  const body: PauseRequestBody = { mode: form.mode }
  const reason = form.reason.trim()
  const until = form.until.trim()
  if (reason) body.reason = reason
  if (until) body.until = until
  if (form.keepLock) body.keep_lock = true
  return body
}

// The short marker of the run list, next to the status badge: the clipped
// reason (the status badge already says `paused`), or `pausing: reason` while a
// live driver drains. The full line (who, since, until) is the tooltip.
export function pauseMarker(pause: PauseInfo, max = 40): string {
  const reason = pause.reason.length > max ? `${pause.reason.slice(0, max - 1)}…` : pause.reason
  if (pause.pausing) return reason ? `pausing: ${reason}` : 'pausing'
  return reason || 'paused'
}
