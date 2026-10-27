import { describe, expect, it } from 'vitest'
import type { MachineNotify, RunNotify } from './api.gen'
import { canNotify, notifyState, onText, onWords, sentLines, startNotify, targetText } from './notify'

const target = { connector: 'telegram', account: 'me', function: 'send_message', to: '42' }

const on: RunNotify = {
  active: true,
  target,
  on: ['review', 'question', 'paused', 'finished'],
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
      source: 'run',
    })
    expect(notifyState({ ...on, on: ['review'] })).toMatchObject({ on: 'review' })
    expect(sentLines(on)).toEqual(['review.12: sent', 'finished.30: not sent', 'paused.20: outcome unknown'])
  })

  it('reads a run without notifications and keeps the last problem', () => {
    expect(notifyState(null)).toEqual({ kind: 'off' })
    expect(
      notifyState({ active: false, on: [], sent: [], problems: ['old', 'connector `telegram` is not approved'] }),
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
    const optIn: MachineNotify = { target, on: [], default_on: false }
    const byDefault: MachineNotify = { ...optIn, default_on: true }
    expect(canNotify(null)).toBe(false)
    expect(canNotify({ on: [], default_on: false })).toBe(false)
    expect(startNotify(optIn, false)).toBeUndefined()
    expect(startNotify(optIn, true)).toBe(true)
    expect(startNotify(byDefault, true)).toBeUndefined()
    expect(startNotify(byDefault, false)).toBe(false)
    expect(startNotify({ on: [], default_on: false }, true)).toBeUndefined()
  })
})
