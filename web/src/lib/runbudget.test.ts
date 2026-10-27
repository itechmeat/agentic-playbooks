import { describe, expect, it } from 'vitest'
import type { BudgetStatus } from './api.gen'
import { amountText, budgetRows, emptyResumeForm, pausedText, resumeBudgetBody } from './runbudget'

const status: BudgetStatus = {
  limit: { tokens: 400_000, usd: 4 },
  on_no_estimate: 'pause',
  sets: 2,
  tokens: { spent: 231_000, unknown_attempts: 1, imputed: 40_000, remaining: 129_000 },
  usd: { spent: 4.5, unknown_attempts: 0, remaining: -0.5 },
  paused: { node: 'review', basis: 'median', need: { tokens: 150_000 } },
}

describe('runbudget', () => {
  it('formats spent, limit and left per dimension with unknown attempts', () => {
    expect(budgetRows(status)).toEqual([
      {
        dim: 'tokens',
        spent: '231k',
        limit: '400k',
        left: '129k',
        unknown: '1 attempt without usage (~40k)',
        over: false,
      },
      { dim: 'usd', spent: '$4.50', limit: '$4.00', left: '$0.00', unknown: null, over: true },
    ])
    const unknown = budgetRows({
      ...status,
      usd: undefined,
      limit: { tokens: 1000 },
      tokens: { spent: 10, unknown_attempts: 2 },
    })
    expect(unknown[0].left).toBe('unknown')
    expect(unknown[0].unknown).toBe('2 attempts without usage (no estimate)')
  })

  it('says where and why a budget pause holds the run', () => {
    expect(pausedText(status)).toBe('paused before review: its median spend no longer fits (needs ~150k tokens)')
    expect(
      pausedText({ ...status, paused: { node: 'fix', batch: 2, basis: 'no_estimate' } }),
    ).toBe('paused before fix (batch 2): it has no spend estimate')
    expect(pausedText({ ...status, paused: undefined })).toBeNull()
    expect(amountText(undefined)).toBe('unknown')
    expect(amountText({ tokens: 1300 })).toBe('1.3k tokens')
  })

  it('builds the resume budget from the form and refuses an empty one', () => {
    const f = emptyResumeForm()
    expect(resumeBudgetBody({ ...f, raiseTokens: '50k' })).toEqual({
      budget: { raise_tokens: 50_000 },
    })
    expect(resumeBudgetBody({ ...f, raiseUsd: '2.5', once: true })).toEqual({
      budget: { raise_usd: 2.5, once: true },
    })
    expect(resumeBudgetBody({ ...f, setUsd: '5', policy: 'continue' })).toEqual({
      budget: { usd: 5, on_no_estimate: 'continue' },
    })
    expect(resumeBudgetBody({ ...f, policy: 'pause' })).toEqual({ budget: { on_no_estimate: 'pause' } })
    expect(resumeBudgetBody(f).error).toContain('raise')
    expect(resumeBudgetBody({ ...f, raiseTokens: 'many' }).error).toContain('token raise')
    expect(resumeBudgetBody({ ...f, raiseTokens: '1k', setTokens: '5k' }).error).toContain('not both')
  })
})
