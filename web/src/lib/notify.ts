// "Notify me" on the dashboard: the text of a run's notify block, the
// state the run view's control shows, what the Run dialog's checkbox sends,
// and the journal notes of the notification events. Pure functions; the
// components only render them.

import type { FinishOutcome, MachineNotify, NotifyTarget, RunNotify, Transition } from './api.gen'
import { NOTIFY_DEFAULT_TRANSITIONS, NOTIFY_OUTCOMES, NOTIFY_TRANSITIONS } from './consts.gen'

/** `telegram/me send_message to 123456789`. */
export function targetText(t: NotifyTarget): string {
  return `${t.connector}/${t.account} ${t.function} to ${t.to}`
}

/**
 * `review, finished`; every default transition reads `everything`, with the
 * opt-in ones (`host_task`, `supervisor`) named after it. A `finished`
 * narrowed to some outcomes reads `finished (failed, aborted)`.
 */
export function onText(on: Transition[], finished: FinishOutcome[] = [...NOTIFY_OUTCOMES]): string {
  const narrowed = finished.length > 0 && finished.length < NOTIFY_OUTCOMES.length
  if (!narrowed && NOTIFY_DEFAULT_TRANSITIONS.every((t) => on.includes(t))) {
    const extra = NOTIFY_TRANSITIONS.filter((t) => on.includes(t) && !isDefault(t))
    return ['everything', ...extra].join(', ')
  }
  const name = (t: Transition) => (t === 'finished' && narrowed ? `finished (${outcomeText(finished)})` : t)
  return on.map(name).join(', ')
}

function isDefault(t: Transition): boolean {
  return (NOTIFY_DEFAULT_TRANSITIONS as readonly Transition[]).includes(t)
}

/** `failed, aborted`, in report order. */
export function outcomeText(outcomes: FinishOutcome[]): string {
  return NOTIFY_OUTCOMES.filter((o) => outcomes.includes(o)).join(', ')
}

/**
 * The transitions and outcomes of a journaled `notify_set.on`: `finished`
 * or one `finished:<outcome>` per outcome; a name this dashboard does not
 * know is left out.
 */
export function journalCoverage(names: string[]): { on: Transition[]; finished: FinishOutcome[] } {
  const on: Transition[] = []
  const finished: FinishOutcome[] = []
  let plainFinished = false
  const add = (t: Transition) => {
    if (!on.includes(t)) on.push(t)
  }
  for (const name of names) {
    if (name.startsWith('finished:')) {
      const outcome = name.slice('finished:'.length)
      if ((NOTIFY_OUTCOMES as readonly string[]).includes(outcome)) {
        if (!finished.includes(outcome as FinishOutcome)) finished.push(outcome as FinishOutcome)
        add('finished')
      }
    } else if ((NOTIFY_TRANSITIONS as readonly string[]).includes(name)) {
      plainFinished ||= name === 'finished'
      add(name as Transition)
    }
  }
  return { on, finished: plainFinished || finished.length === 0 ? [...NOTIFY_OUTCOMES] : finished }
}

/** What a start notifies about, in words for the Run dialog. */
export function onWords(on: Transition[]): string {
  const words: Record<Transition, string> = {
    review: 'a review waiting',
    question: 'a question waiting',
    paused: 'a pause you did not make',
    finished: 'the end of the run',
    host_task: 'a host task waiting',
    supervisor: 'a failed step waiting for the supervisor',
    stalled: 'a step that stopped retrying on the same rejection',
    follow_up: 'the follow-up check read after the run',
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
  | { kind: 'on'; target: string; on: string; source?: string; finished: FinishOutcome[] }

export function notifyState(block: RunNotify | null | undefined): NotifyState {
  const problem = block?.problems.at(-1)
  if (!block || !block.active || !block.target) {
    return problem ? { kind: 'off', problem } : { kind: 'off' }
  }
  return {
    kind: 'on',
    target: targetText(block.target),
    on: onText(block.on, block.finished),
    finished: block.finished,
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
  const cov = Array.isArray(r.on) ? journalCoverage(r.on.map(String)) : null
  const on = cov ? onText(cov.on, cov.finished) : ''
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

/** The outcomes the run view's filter offers, in words. */
export const OUTCOME_WORDS: Record<FinishOutcome, string> = {
  succeeded: 'succeeded',
  needs_look: 'needs a look',
  failed: 'failed',
  aborted: 'stopped by apb or the supervisor',
}
