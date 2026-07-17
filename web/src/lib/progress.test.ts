import { describe, expect, it } from 'vitest'
import { nextDisplay, showBar, type ProgressDisplayState } from './progress'

function summary(percent: number, planKey: string, label: string | null = null) {
  return { percent, label, waiting_on: null, waiting_kind: null, plan_key: planKey }
}

describe('nextDisplay', () => {
  it('is monotonic within the same run and plan: a lower percent keeps the shown value', () => {
    const prev: ProgressDisplayState = { runKey: 'ws/run-a', planKey: 'p1', shown: 40 }
    const state = nextDisplay(prev, 'ws/run-a', summary(30, 'p1'))
    expect(state.shown).toBe(40)
    expect(state.runKey).toBe('ws/run-a')
    expect(state.planKey).toBe('p1')
  })

  it('honestly resets down when plan_key changes, even with the same run', () => {
    const prev: ProgressDisplayState = { runKey: 'ws/run-a', planKey: 'p1', shown: 40 }
    const state = nextDisplay(prev, 'ws/run-a', summary(21, 'p2'))
    expect(state.shown).toBe(21)
    expect(state.planKey).toBe('p2')
  })

  it('does not reset on a label change alone: label is not even an input', () => {
    const prev: ProgressDisplayState = { runKey: 'ws/run-a', planKey: 'p1', shown: 40 }
    const state = nextDisplay(prev, 'ws/run-a', summary(21, 'p1', 'chapter 4 of 14'))
    expect(state.shown).toBe(40)
  })

  it('resets to the incoming run percent when runKey changes, even if the old shown was higher', () => {
    const prev: ProgressDisplayState = { runKey: 'ws/run-a', planKey: 'p1', shown: 80 }
    const state = nextDisplay(prev, 'ws/run-b', summary(20, 'p1'))
    expect(state.shown).toBe(20)
    expect(state.runKey).toBe('ws/run-b')
  })

  it('starts fresh at the incoming percent when there is no previous state', () => {
    const state = nextDisplay(null, 'ws/run-a', summary(15, 'p1'))
    expect(state.shown).toBe(15)
    expect(state.runKey).toBe('ws/run-a')
    expect(state.planKey).toBe('p1')
  })

  it('clamps shown to 100 even if percent somehow exceeds it', () => {
    const prev: ProgressDisplayState = { runKey: 'ws/run-a', planKey: 'p1', shown: 100 }
    const state = nextDisplay(prev, 'ws/run-a', summary(150, 'p1'))
    expect(state.shown).toBe(100)
  })

  it('clamps shown to 0 even if percent is negative', () => {
    const state = nextDisplay(null, 'ws/run-a', summary(-5, 'p1'))
    expect(state.shown).toBe(0)
  })
})

describe('showBar', () => {
  it('shows for running, hides for terminal', () => {
    expect(showBar('running')).toBe(true)
    expect(showBar('created')).toBe(true)
    expect(showBar('paused')).toBe(true)
    expect(showBar('succeeded')).toBe(false)
    expect(showBar('failed')).toBe(false)
    expect(showBar('aborted')).toBe(false)
  })
  it('shows for interrupted, a resumable crash-recovery state, not a terminal one', () => {
    expect(showBar('interrupted')).toBe(true)
  })
})
