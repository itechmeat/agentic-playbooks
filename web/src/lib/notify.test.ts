import { describe, expect, it } from 'vitest'
import type { FinishOutcome, MachineNotify, RunNotify } from './api.gen'

const ALL_OUTCOMES: FinishOutcome[] = ['succeeded', 'needs_look', 'failed', 'aborted']
import {
  canNotify,
  journalCoverage,
  notifySetNote,
  notifyState,
  onText,
  onWords,
  RUN_TRANSITIONS,
  sentLines,
  startNotify,
  targetText,
} from './notify'

const target = { connector: 'telegram', account: 'me', function: 'send_message', to: '42' }

const on: RunNotify = {
  active: true,
  target,
  on: ['review', 'question', 'paused', 'finished'],
  finished: ALL_OUTCOMES,
  source: 'run',
  sent: [
    { transition: 'review.12', state: 'confirmed', at: 1 },
    { transition: 'finished.30', state: 'not_sent', at: 2 },
    { transition: 'paused.20', state: 'unknown', at: 3 },
  ],
  problems: [],
}

describe('notify', () => {
  it('reads a run that notifies, with what was sent', () => {
    expect(targetText(target)).toBe('telegram/me send_message to 42')
    expect(notifyState(on)).toEqual({
      kind: 'on',
      target: 'telegram/me send_message to 42',
      on: 'everything',
      finished: ALL_OUTCOMES,
      source: 'run',
    })
    expect(notifyState({ ...on, on: ['review'] })).toMatchObject({ on: 'review' })
    expect(sentLines(on)).toEqual(['review.12: sent', 'finished.30: not sent', 'paused.20: outcome unknown'])
  })

  it('reads a run without notifications and keeps the last problem', () => {
    expect(notifyState(null)).toEqual({ kind: 'off' })
    expect(
      notifyState({ active: false, on: [], finished: [], sent: [], problems: ['old', 'connector `telegram` is not approved'] }),
    ).toEqual({ kind: 'off', problem: 'connector `telegram` is not approved' })
    expect(sentLines(undefined)).toEqual([])
  })

  it('says everything only when every transition is named, duplicates or not', () => {
    expect(onText(['finished', 'review', 'paused', 'question'])).toBe('everything')
    expect(onText(['review', 'review', 'finished', 'finished'])).toBe('review, review, finished, finished')
    expect(onWords(['finished', 'review'])).toBe('a review waiting and the end of the run')
    expect(onWords(['review'])).toBe('a review waiting')
  })

  it('sends a start choice only when it differs from what the workspace does anyway', () => {
    const optIn: MachineNotify = { target, on: [], default_on: false, finished: ALL_OUTCOMES }
    const byDefault: MachineNotify = { ...optIn, default_on: true }
    expect(canNotify(null)).toBe(false)
    expect(canNotify({ on: [], finished: ALL_OUTCOMES, default_on: false })).toBe(false)
    expect(startNotify(optIn, false)).toBeUndefined()
    expect(startNotify(optIn, true)).toBe(true)
    expect(startNotify(byDefault, true)).toBeUndefined()
    expect(startNotify(byDefault, false)).toBe(false)
    expect(startNotify({ on: [], finished: ALL_OUTCOMES, default_on: false }, true)).toBeUndefined()
  })

  it('names the opt-in transitions and a narrowed end', () => {
    expect(onText(['review', 'question', 'paused', 'finished', 'host_task'])).toBe('everything, host_task')
    expect(onText(['review', 'finished'], ['failed', 'aborted'])).toBe('review, finished (failed, aborted)')
    expect(onText(['review', 'question', 'paused', 'finished'], ['failed'])).toBe(
      'review, question, paused, finished (failed)',
    )
    expect(onWords(['host_task', 'supervisor'])).toBe(
      'a host task waiting and a failed step waiting for the supervisor',
    )
    expect(notifyState({ ...on, finished: ['failed'] })).toMatchObject({
      on: 'review, question, paused, finished (failed)',
    })
  })

  it('reads the journal names of a filtered subscription', () => {
    expect(journalCoverage(['finished:failed', 'host_task'])).toEqual({
      on: ['finished', 'host_task'],
      finished: ['failed'],
    })
    expect(journalCoverage(['review', 'finished']).finished).toEqual(ALL_OUTCOMES)
    // A repeated name counts once, as the engine reads it.
    expect(journalCoverage(['finished:failed', 'finished:failed', 'finished:failed', 'finished:failed']).finished).toEqual(['failed'])
    expect(journalCoverage(['finished:later', 'someday', 'review'])).toEqual({
      on: ['review'],
      finished: ALL_OUTCOMES,
    })
    expect(notifySetNote({ target, on: ['finished:failed', 'host_task'], source: 'run' })).toBe(
      'notifications on: telegram/me about finished (failed), host_task (from run)',
    )
  })

  it('the run dialog never offers health', () => {
    expect(RUN_TRANSITIONS).not.toContain('health')
    expect(onText(['review', 'question', 'paused', 'finished'])).toBe('everything')
    expect(journalCoverage(['review', 'health']).on).toEqual(['review'])
  })

  it('onWords never names health: a run sends no health notice', () => {
    expect(onWords(['review', 'question', 'paused', 'finished', 'health'])).toBe(
      'a review waiting, a question waiting, a pause you did not make and the end of the run',
    )
    expect(onWords(['health'])).toBe('')
  })
})
