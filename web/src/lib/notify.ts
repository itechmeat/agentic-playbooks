// "Notify me" on the dashboard: the text of a run's notify block, the
// state the run view's control shows, what the Run dialog's checkbox sends,
// and the journal notes of the notification events. Pure functions; the
// components only render them.

import type { MachineNotify, NotifyTarget, RunNotify, Transition } from './api.gen'
import { NOTIFY_TRANSITIONS } from './consts.gen'

/** `telegram/me send_message to 123456789`. */
export function targetText(t: NotifyTarget): string {
  return `${t.connector}/${t.account} ${t.function} to ${t.to}`
}

/** `review, finished`; every transition reads `everything`. */
export function onText(on: Transition[]): string {
  return NOTIFY_TRANSITIONS.every((t) => on.includes(t)) ? 'everything' : on.join(', ')
}

/** What a start notifies about, in words for the Run dialog. */
export function onWords(on: Transition[]): string {
  const words: Record<Transition, string> = {
    review: 'a review waiting',
    question: 'a question waiting',
    paused: 'a pause you did not make',
    finished: 'the end of the run',
  }
  const said = NOTIFY_TRANSITIONS.filter((t) => on.includes(t)).map((t) => words[t])
  if (said.length < 2) return said.join('')
  return `${said.slice(0, -1).join(', ')} and ${said.at(-1)}`
}

/** What the dashboard says when it cannot read the notification settings. */
export const NOTIFY_UNREADABLE = 'The notification settings could not be read.'

/** What the run view's control shows about a run's notifications. */
export type NotifyState =
  | { kind: 'off'; problem?: string }
  | { kind: 'on'; target: string; on: string; source?: string }

export function notifyState(block: RunNotify | null | undefined): NotifyState {
  const problem = block?.problems.at(-1)
  if (!block || !block.active || !block.target) {
    return problem ? { kind: 'off', problem } : { kind: 'off' }
  }
  return {
    kind: 'on',
    target: targetText(block.target),
    on: onText(block.on),
    ...(block.source ? { source: block.source } : {}),
  }
}

/** One line per notified transition: `review.12: sent`. */
export function sentLines(block: RunNotify | null | undefined): string[] {
  return (block?.sent ?? []).map((s) => `${s.transition}: ${stateWord(s.state)}`)
}

function stateWord(state: string): string {
  switch (state) {
    case 'confirmed':
      return 'sent'
    case 'not_sent':
      return 'not sent'
    case 'unknown':
      return 'outcome unknown'
    default:
      return state
  }
}

/** Whether the machine has a target a run can notify through. */
export function canNotify(machine: MachineNotify | null | undefined): boolean {
  return !!machine?.target
}

/**
 * What a start sends for the Run dialog's checkbox: nothing while it says
 * what the workspace does anyway, `true` to ask for notifications, `false`
 * to turn a default off.
 */
export function startNotify(
  machine: MachineNotify | null | undefined,
  checked: boolean,
): boolean | undefined {
  if (!canNotify(machine)) return undefined
  return checked === !!machine?.default_on ? undefined : checked
}

/** The node every engine notification is journaled under. */
export const NOTIFY_SITE = '@notify'

/** The journal note of a `notify_set`. */
export function notifySetNote(r: Record<string, unknown>): string {
  const t = r.target as Partial<NotifyTarget> | undefined
  const who = r.by === 'operator' ? ', changed by a person' : ''
  const from = typeof r.source === 'string' && r.source ? ` (from ${r.source})` : ''
  if (!t || !t.connector) {
    const why = typeof r.problem === 'string' && r.problem ? `: ${r.problem}` : ''
    return `notifications off${from}${who}${why}`
  }
  const on = Array.isArray(r.on) ? onText(r.on as Transition[]) : ''
  return `notifications on: ${t.connector}/${t.account ?? ''} about ${on}${from}${who}`
}

/** The transition a notification label names (`notify:review.12`). */
function transitionOf(label: unknown): string {
  return typeof label === 'string' && label.startsWith('notify:') ? label.slice(7) : String(label ?? '')
}

/**
 * The journal note of a send-ledger or connector event that belongs to a
 * notification, `undefined` for any other event.
 */
export function notifyEventNote(type: string, r: Record<string, unknown>): string | undefined {
  if (type === 'send_intent_recorded' && r.node === NOTIFY_SITE) {
    return `notification ${transitionOf(r.label)}: ${String(r.connector ?? '')}.${String(r.function ?? '')}`
  }
  // apb's own send only: a read-back of an earlier one carries `reconcile`.
  if (type === 'connector_call' && r.origin === 'notify') {
    const via = `${String(r.connector ?? '')}.${String(r.function ?? '')}`
    return r.outcome === 'ok'
      ? `notification sent through ${via}`
      : `notification through ${via} failed (${String(r.outcome ?? 'unknown')})`
  }
  return undefined
}
