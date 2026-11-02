import { describe, expect, it } from 'vitest'
import { driverDead, showsDriverDead } from './status'

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

describe('driverDead', () => {
  it('reads a run with no live driver as dead, but not one released while a retry waits', () => {
    expect(driverDead({ driver_alive: false })).toBe(true)
    expect(driverDead({ driver_alive: false, released: { seq: 9, wake_at_ms: 1, nodes: ['w'] } })).toBe(false)
    expect(driverDead({ driver_alive: true })).toBe(false)
    expect(driverDead({ driver_alive: null })).toBe(false)
  })
})
