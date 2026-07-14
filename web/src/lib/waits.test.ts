import { describe, it, expect } from 'vitest'
import { pendingWaits } from './waits'
import type { WfEvent } from './types'

const ev = (seq: number, type: string, node?: string): WfEvent =>
  ({ seq, ts: 0, type, node }) as WfEvent

describe('pendingWaits', () => {
  it('surfaces a started but unresolved wait', () => {
    expect(pendingWaits([ev(0, 'wait_started', 'w')])).toEqual(['w'])
  })
  it('hides a signalled wait', () => {
    expect(pendingWaits([ev(0, 'wait_started', 'w'), ev(1, 'wait_signalled', 'w')])).toEqual([])
  })
  it('hides a timed-out wait', () => {
    expect(pendingWaits([ev(0, 'wait_started', 'w'), ev(1, 'wait_timeout', 'w')])).toEqual([])
  })
})
