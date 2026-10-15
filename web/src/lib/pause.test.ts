import { describe, it, expect } from 'vitest'
import { canPause, canResume, pauseBody, pauseDetails, pauseLine, pauseMarker, pauseWord, when } from './pause'
import type { PauseInfo } from './types'

const paused: PauseInfo = {
  reason: 'lunch',
  by: 'operator',
  mode: 'drain',
  since: Date.UTC(2026, 9, 15, 20, 0, 0),
  until: Date.UTC(2026, 9, 15, 22, 0, 0),
}

describe('pause words', () => {
  it('reads paused, pausing, or nothing', () => {
    expect(pauseWord(null)).toBeNull()
    expect(pauseWord(paused)).toBe('paused')
    expect(pauseWord({ ...paused, pausing: true })).toBe('pausing')
  })

  it('prints who, since and until in the order apb runs does', () => {
    expect(pauseDetails(paused)).toEqual([
      'by operator',
      'since 2026-10-15T20:00:00Z',
      'until 2026-10-15T22:00:00Z',
    ])
    expect(pauseLine(paused)).toBe(
      'paused (lunch, by operator, since 2026-10-15T20:00:00Z, until 2026-10-15T22:00:00Z)',
    )
  })

  it('says the mode and no since while still pausing, and a kept lock', () => {
    const p: PauseInfo = { reason: 'r', by: 'supervisor', mode: 'now', pausing: true, keep_lock: true }
    expect(pauseDetails(p)).toEqual(['by supervisor', 'mode now', 'lock kept'])
    expect(pauseLine(p)).toContain('pausing (r, ')
  })

  it('clips a long reason in the list marker', () => {
    const long = { ...paused, reason: 'x'.repeat(100) }
    expect(pauseMarker(long).length).toBeLessThan(60)
    expect(pauseMarker(long).startsWith('xxx')).toBe(true)
    expect(pauseMarker({ ...paused, pausing: true })).toBe('pausing: lunch')
    expect(pauseMarker(paused)).toBe('lunch')
  })
})

describe('what the buttons offer', () => {
  it('pauses a running run that has no pause queued', () => {
    expect(canPause('running', null)).toBe(true)
    expect(canPause('running', { ...paused, pausing: true })).toBe(false)
    expect(canPause('paused', paused)).toBe(false)
    expect(canPause('succeeded', null)).toBe(false)
  })

  it('resumes a paused run or one whose driver is gone, never a finished one', () => {
    expect(canResume('paused', false)).toBe(true)
    expect(canResume('running', true)).toBe(true)
    expect(canResume('running', false)).toBe(false)
    expect(canResume('failed', true)).toBe(false)
  })
})

describe('pause request body', () => {
  it('leaves empty fields out', () => {
    expect(pauseBody({ mode: 'drain', reason: ' ', until: '', keepLock: false })).toEqual({
      mode: 'drain',
    })
    expect(pauseBody({ mode: 'now', reason: ' lunch ', until: '90m', keepLock: true })).toEqual({
      mode: 'now',
      reason: 'lunch',
      until: '90m',
      keep_lock: true,
    })
  })
})

describe('pause times', () => {
  it('shows a time past what a Date holds instead of throwing', () => {
    expect(when(Date.UTC(2026, 9, 15, 20, 0, 0))).toBe('2026-10-15T20:00:00Z')
    expect(when(18446744073709552000)).toBe('18446744073709552000 ms after the epoch')
    expect(() => pauseDetails({ ...paused, until: 18446744073709552000 })).not.toThrow()
    expect(when(undefined)).toBeNull()
  })
})
