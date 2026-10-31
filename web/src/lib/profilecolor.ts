// Colors of the terminal look, as CSS custom properties from app.css, so they
// follow the light/dark palette. Use them as `style:--k={color}` together
// with the `.box`, `.legend-sq`, `.hatch-bar` and `.glow` classes, or directly
// as `style:color` / `style:border-color`. Never a static `style="..."`
// attribute: the page CSP refuses those; the `style:` directive goes through
// the CSSOM, which it allows.

/** One accent of the palette (see `--c-*` in app.css). */
export type Accent = 'violet' | 'blue' | 'orange' | 'teal' | 'pink' | 'green' | 'amber' | 'red'

/** CSS value of an accent, e.g. `var(--c-violet)`. */
export function accentVar(accent: Accent | 'muted' | 'faint'): string {
  return `var(--c-${accent})`
}

// Accents a profile may get. Green, amber and red are left out on purpose:
// they mean judge, human review and failure, and a profile must not be
// mistaken for any of them.
export const PROFILE_ACCENTS: readonly Accent[] = ['violet', 'blue', 'orange', 'teal', 'pink']

// FNV-1a over UTF-16 code units: tiny, stable across sessions and browsers.
function hash(text: string): number {
  let h = 0x811c9dc5
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i)
    h = Math.imul(h, 0x01000193)
  }
  return h >>> 0
}

/** Stable accent for a profile name; an empty name gets no accent (muted). */
export function profileAccent(name: string | null | undefined): Accent | null {
  const n = (name ?? '').trim()
  if (!n) return null
  return PROFILE_ACCENTS[hash(n) % PROFILE_ACCENTS.length]
}

/** CSS color of a profile, `var(--c-muted)` when there is no profile. */
export function profileColor(name: string | null | undefined): string {
  const a = profileAccent(name)
  return a ? accentVar(a) : accentVar('muted')
}

/**
 * CSS color of a graph node by kind. Agent tasks take their profile color;
 * judges are green, human reviews amber, start and finish muted. Any other
 * kind (connector, script, sub-playbook, ...) falls back to the profile color
 * when one is given, else muted.
 */
export function nodeKindColor(kind: string | null | undefined, profile?: string | null): string {
  const k = (kind ?? '').toLowerCase()
  if (k === 'judge') return accentVar('green')
  if (k === 'human_review') return accentVar('amber')
  if (k === 'start' || k === 'finish') return accentVar('muted')
  return profileColor(profile)
}

/** Whether a node kind renders as the muted start/finish pill. */
export function isTerminalNodeKind(kind: string | null | undefined): boolean {
  const k = (kind ?? '').toLowerCase()
  return k === 'start' || k === 'finish'
}

/**
 * CSS color of a run or node status: running blue, success green, failure
 * or cancel red, paused or waiting amber, skipped faint, anything else
 * (pending, created) muted. Always show the status text next to it; color is never the only
 * signal.
 */
export function statusColor(status: string | null | undefined): string {
  const s = (status ?? '').toLowerCase()
  if (s.includes('succeed') || s === 'done' || s === 'ok' || s === 'passed') return accentVar('green')
  if (s.includes('fail') || s.includes('timed') || s.includes('abort') || s.includes('cancel') || s === 'error')
    return accentVar('red')
  if (s.includes('run')) return accentVar('blue')
  if (s.includes('pause') || s.includes('wait') || s.includes('interrupt') || s.includes('review'))
    return accentVar('amber')
  if (s.includes('skip')) return accentVar('faint')
  return accentVar('muted')
}
