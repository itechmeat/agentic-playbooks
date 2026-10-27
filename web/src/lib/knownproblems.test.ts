import { describe, expect, it } from 'vitest'
import {
  actionsFor,
  checkText,
  closeHint,
  groupProblems,
  hasRunProblems,
  hiddenReasonText,
  originText,
  otherWordings,
  problemStateText,
  problemTone,
  quoteProblem,
  recheckResult,
  runsText,
  scopeText,
  shownBasisText,
  shownNodesText,
  sourceNowText,
  unsignedText,
} from './knownproblems'
import { problem } from './knownproblems.fixture'

describe('groupProblems', () => {
  it('puts each problem in exactly one group', () => {
    const shown = problem({ id: 'a' })
    const look = problem({ id: 'b', needs_look: true })
    const changed = problem({ id: 'c', source_now: 'changed', needs_look: true })
    const waiting = problem({ id: 'd', significant: false, runs: 1 })
    const closed = problem({ id: 'e', status: 'closed' })
    const dismissed = problem({ id: 'f', status: 'dismissed', needs_look: true })
    const g = groupProblems([shown, look, changed, waiting, closed, dismissed])
    expect(g.shown.map((p) => p.id)).toEqual(['a'])
    expect(g.look.map((p) => p.id)).toEqual(['b', 'c'])
    expect(g.waiting.map((p) => p.id)).toEqual(['d'])
    expect(g.done.map((p) => p.id)).toEqual(['e', 'f'])
  })

  it('names and colours a problem by its state', () => {
    expect([problemStateText(problem()), problemTone(problem())]).toEqual(['shown', 'fail'])
    expect([problemStateText(problem({ needs_look: true })), problemTone(problem({ needs_look: true }))]).toEqual([
      'needs a look',
      'gap',
    ])
    expect(problemTone(problem({ status: 'closed' }))).toBe('pass')
    expect(problemStateText(problem({ significant: false }))).toBe('not shown yet')
  })
})

describe('texts', () => {
  it('reads a scope, an origin, runs and the source', () => {
    expect(scopeText({ kind: 'mcp_tool', name: 'notion.search' })).toBe('mcp:notion.search')
    expect(originText({ type: 'connector_failures', outcome: 'rate_limited', http_status: 429 })).toBe(
      'calls failed: rate limited, HTTP 429',
    )
    expect(originText({ type: 'finding', kind: 'tool_problem' })).toBe('finding: tool problem')
    expect([runsText(1), runsText(3)]).toEqual(['1 run', '3 runs'])
    expect(sourceNowText('unchanged')).toBeNull()
    expect(sourceNowText(undefined)).toBeNull()
    expect(sourceNowText('changed')).toContain('changed since it was last checked')
    expect(sourceNowText('unreadable')).toContain('never closes')
  })

  it('reads a recheck and why a run hid a problem', () => {
    expect(
      checkText({ digest: 'd', verdict: 'closed_proposed', basis: 'host', reason_code: 'quote_new', at_ms: 1, by: 'mcp' }),
    ).toBe('close proposed by a host (quote new)')
    expect(hiddenReasonText('source_changed_unchecked')).toBe('its source changed and was not rechecked')
    expect(hiddenReasonText('something_new')).toBe('something new')
    expect(hiddenReasonText('no_matching_node')).toContain('no agent node of this run')
    expect(shownBasisText('unchanged')).toBe('source unchanged')
    expect(shownBasisText('model')).toBe('rechecked by the decision model')
    expect(shownNodesText([])).toBe('to every agent node')
    expect(shownNodesText(['fix', 'review'])).toBe('to fix, review')
  })

  it('checks a quote length before it is sent', () => {
    expect(quoteProblem('too short')).toContain('at least 12')
    expect(quoteProblem('x'.repeat(401))).toContain('at most 400')
    expect(quoteProblem('cc -Wno-error -c z.c')).toBeNull()
  })
})

describe('actionsFor', () => {
  it('offers what fits the state', () => {
    expect(actionsFor(problem())).toEqual({
      confirm: true,
      dismiss: true,
      reopen: false,
      close: true,
      holds: false,
      recheck: false,
    })
    const changed = actionsFor(problem({ source_now: 'changed', confirmed: { by: 'cli', at_ms: 1 } }))
    expect([changed.confirm, changed.holds, changed.recheck]).toEqual([false, true, true])
    // An unreadable source closes only on the person's word, and the
    // server cannot record that it still holds.
    const unreadable = actionsFor(problem({ source_now: 'unreadable', needs_look: true }))
    expect([unreadable.close, unreadable.holds, unreadable.recheck]).toEqual([true, false, false])
    const closed = actionsFor(problem({ status: 'closed' }))
    expect([closed.reopen, closed.close, closed.dismiss]).toEqual([true, false, false])
  })
})

describe('hasRunProblems', () => {
  it('is true only when a run showed or hid something', () => {
    expect(hasRunProblems(undefined)).toBe(false)
    expect(hasRunProblems({ shown: [], hidden: [], unsigned: 0 })).toBe(false)
    expect(hasRunProblems({ shown: [], hidden: [{ id: 'kp-1', reason: 'over_limit' }], unsigned: 0 })).toBe(true)
    expect(hasRunProblems({ shown: [], hidden: [], unsigned: 2 })).toBe(true)
  })
})

describe('the close form, a recheck and the wordings', () => {
  const record = (verdict: 'holds' | 'closed' | 'closed_proposed' | 'undetermined') => ({
    digest: 'd',
    verdict,
    basis: 'model' as const,
    reason_code: 'quote_new',
    at_ms: 1,
    by: 'model',
  })

  it('says what a recheck came to', () => {
    expect(recheckResult(record('closed'))).toEqual({
      text: 'Problem rechecked: closed by the decision model (quote new)',
      tone: 'success',
    })
    expect(recheckResult(record('closed_proposed')).tone).toBe('info')
    expect(recheckResult(record('undetermined')).tone).toBe('warning')
  })

  it('takes a close on the person word without a readable source', () => {
    const src = { kind: 'project_file' as const, name: 'scripts/build.sh', digest: 'd' }
    expect(closeHint(problem())).toContain('taken on your word')
    expect(closeHint(problem({ source: src, source_now: 'unreadable' }))).toContain('12 to 400 characters')
    expect(closeHint(problem({ source: src, source_now: 'changed' }))).toContain('no passage new')
  })

  it('lists the other wordings, most runs first, and counts an unsigned file', () => {
    const texts = [
      { text: 'a', runs: 3, last_seen_ms: 1 },
      { text: 'b', runs: 1, last_seen_ms: 1 },
      { text: 'c', runs: 2, last_seen_ms: 1 },
    ]
    expect(otherWordings(texts, 'a').map((t) => t.text)).toEqual(['c', 'b'])
    expect(otherWordings(undefined, 'a')).toEqual([])
    expect(unsignedText(1)).toBe('1 problem in a file this apb installation did not write: never shown to runs and not listed.')
    expect(unsignedText(3)).toContain('3 problems')
  })
})
