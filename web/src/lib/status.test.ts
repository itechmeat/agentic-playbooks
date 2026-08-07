import { describe, expect, it } from 'vitest'
import { showsDriverDead } from './status'

describe('showsDriverDead', () => {
  it('marks a run that still reads running but has no live driver', () => {
    expect(showsDriverDead('running', true)).toBe(true)
  })

  it('does not mark a healthy run', () => {
    expect(showsDriverDead('running', false)).toBe(false)
    expect(showsDriverDead('running', undefined)).toBe(false)
  })

  it('does not mark a run that already finished', () => {
    // A terminal run has nothing left to drive, so a stale claim is not news.
    expect(showsDriverDead('succeeded', true)).toBe(false)
    expect(showsDriverDead('failed', true)).toBe(false)
    expect(showsDriverDead('aborted', true)).toBe(false)
  })
})
