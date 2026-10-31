import { describe, expect, it } from 'vitest'
import type { AttemptIsolationRow } from './api.gen'
import { ISOLATION_CODE_TEXTS, ISOLATION_REASON_TEXTS } from './consts.gen'
import {
  isSandboxed,
  isolationBadge,
  isolationCodeText,
  isolationLimits,
  isolationReasonText,
} from './attemptisolation'

const sandboxed: AttemptIsolationRow = {
  node: 'w',
  attempt: 1,
  isolation: {
    level: 'full',
    backend: 'bwrap',
    network: 'none',
    holds: ['network.none'],
    limits: ['user.host_uid', 'credentials.env_passed:GH_TOKEN', 'from.a.newer.apb'],
    view: [],
  },
}

describe('attempt isolation', () => {
  it('words every limit from the exported table', () => {
    const limits = isolationLimits(sandboxed)
    expect(limits[0]).toEqual({ code: 'user.host_uid', text: ISOLATION_CODE_TEXTS['user.host_uid'] })
    expect(limits[1].text).toContain('(GH_TOKEN)')
    expect(limits[2].text).toBe('from.a.newer.apb')
  })

  it('labels the badge with level, sandbox and network', () => {
    expect(isolationBadge(sandboxed)).toBe('full · sandbox · network none')
    expect(isSandboxed(sandboxed)).toBe(true)
    const bare = { ...sandboxed, isolation: { ...sandboxed.isolation, level: 'best_effort', backend: 'none', network: 'open' } }
    expect(isolationBadge(bare)).toBe('best_effort · no sandbox · network open')
    expect(isSandboxed(bare)).toBe(false)
    const unknown = { ...sandboxed, isolation: { ...sandboxed.isolation, backend: 'landlock' } }
    expect(isSandboxed(unknown)).toBe(false)
    expect(isolationBadge(unknown)).toBe('full · no sandbox · network none')
  })

  it('words a fallback reason by its code and keeps the detail', () => {
    expect(isolationReasonText('disabled (APB_SANDBOX=off)')).toEqual({
      code: 'disabled (APB_SANDBOX=off)',
      text: ISOLATION_REASON_TEXTS.disabled,
    })
    expect(isolationReasonText('bwrap_missing').text).toBe(ISOLATION_REASON_TEXTS.bwrap_missing)
    expect(isolationCodeText('no_sandbox')).toBe(ISOLATION_CODE_TEXTS.no_sandbox)
  })
})
