import { describe, expect, it } from 'vitest'
import { pendingSupervisorFromPayload } from './supervisors'
import type { PendingSupervisor } from './types'

describe('pendingSupervisorFromPayload', () => {
  it('is null when there is no pending supervisor block', () => {
    expect(pendingSupervisorFromPayload(null)).toBeNull()
    expect(pendingSupervisorFromPayload(undefined)).toBeNull()
  })

  it('surfaces the failed node, trigger, and options from the run payload', () => {
    const pending: PendingSupervisor = {
      node: 'work',
      trigger: 'node_failed',
      instruction: 'waiting after work failed',
      options: ['retry', 'continue_from', 'abort'],
      how_to_decide: 'use supervisor tools',
    }
    expect(pendingSupervisorFromPayload(pending)).toEqual({
      node: 'work',
      trigger: 'node_failed',
      options: ['retry', 'continue_from', 'abort'],
      instruction: 'waiting after work failed',
    })
  })

  it('defaults options to an empty array when the payload omits them', () => {
    // JSON.parse is the JSON/API boundary: a wire payload may lack options.
    const pending = JSON.parse(
      '{"node":"work","trigger":"node_timeout","instruction":"","how_to_decide":""}',
    ) as PendingSupervisor
    expect(pendingSupervisorFromPayload(pending)).toEqual({
      node: 'work',
      trigger: 'node_timeout',
      options: [],
      instruction: '',
    })
  })
})
