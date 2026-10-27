// "Will it fit here?": pure helpers over a `FitReport` for the card. The
// report keeps four sections apart (compatibility, setup, trust, consent),
// and so does everything here: being ready and being trusted are two
// separate facts, never one verdict.
import type { FitItem, FitReport, TrustItem } from './api.gen'

export type Tone = 'ok' | 'warn' | 'danger' | 'muted'

export interface FitBadge {
  key: 'ready' | 'trusted' | 'consent'
  label: string
  tone: Tone
}

// The badges at the top of the card: ready, trusted and, when a start must
// be agreed to, the consent. Each says one thing.
export function fitBadges(r: FitReport): FitBadge[] {
  const out: FitBadge[] = [
    r.ready
      ? { key: 'ready', label: 'ready', tone: 'ok' }
      : { key: 'ready', label: 'not ready', tone: 'danger' },
    r.trusted
      ? { key: 'trusted', label: 'trusted', tone: 'ok' }
      : { key: 'trusted', label: 'not trusted', tone: 'warn' },
  ]
  if (r.needs_consent) out.push({ key: 'consent', label: 'needs consent', tone: 'warn' })
  return out
}

// Items that keep a run from working first, notes after.
export function bySeverity(items: FitItem[]): { blocking: FitItem[]; notes: FitItem[] } {
  return {
    blocking: items.filter((i) => i.severity === 'blocking'),
    notes: items.filter((i) => i.severity === 'note'),
  }
}

function count(n: number, one: string, many: string): string {
  return `${n} ${n === 1 ? one : many}`
}

export function compatibilitySummary(r: FitReport): string {
  const { blocking, notes } = bySeverity(r.compatibility)
  if (blocking.length) return `${count(blocking.length, 'blocker', 'blockers')}: cannot run here`
  if (notes.length) return `compatible, ${count(notes.length, 'note', 'notes')}`
  return 'compatible with this project'
}

export function setupSummary(r: FitReport): string {
  const { blocking, notes } = bySeverity(r.setup)
  if (blocking.length) return `${count(blocking.length, 'thing', 'things')} to set up first`
  if (notes.length) return `nothing blocking, ${count(notes.length, 'note', 'notes')}`
  return 'nothing to set up'
}

export function trustSummary(r: FitReport): string {
  const open = r.trust.filter((t) => !t.approved)
  if (!open.length) return 'every part is approved'
  const covered = open.filter((t) => t.acknowledge_covers).length
  const hard = open.length - covered
  const parts = [`${count(open.length, 'part', 'parts')} not approved`]
  if (covered) parts.push(`${covered} a confirmation at start covers`)
  if (hard) parts.push(`${hard} ${hard === 1 ? 'needs' : 'need'} an approval`)
  return parts.join('; ')
}

export function consentSummary(r: FitReport): string {
  if (!r.consent.irreversible.length) return 'no irreversible effects'
  return `a start must be agreed to: ${r.consent.irreversible.join(', ')}`
}

// One line for a trust item: what it is and whether it is approved.
export function trustLine(t: TrustItem): string {
  const what = `${t.kind.replace(/_/g, ' ')} ${t.subject}`
  if (t.approved) return `${what}: approved`
  return t.acknowledge_covers
    ? `${what}: not approved, a confirmation at start covers it`
    : `${what}: not approved, needs an approval`
}

// One line for a compatibility or setup item.
export function itemLine(i: FitItem): string {
  return i.subject ? `${i.subject}: ${i.detail}` : i.detail
}
