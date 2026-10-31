import { describe, expect, it } from 'vitest'
import {
  finishedCount,
  glyphLetter,
  medianRunTime,
  runHistory,
  runsByPlaybook,
  startedToday,
  successRate,
} from './playbooklist'
import type { RunSummary } from './types'

const run = (id: string, status: string, ts: number, playbook = 'pb', ws = 'w1'): RunSummary =>
  ({ run_id: id, playbook, status, started_ts: ts, workspace_id: ws, project: 'p' }) as RunSummary

describe('runsByPlaybook', () => {
  it('groups per project and playbook, newest first', () => {
    const m = runsByPlaybook([
      run('a', 'succeeded', 1),
      run('b', 'failed', 3),
      run('c', 'succeeded', 2, 'pb', 'w2'),
    ])
    expect(m.get('w1/pb')?.map((r) => r.run_id)).toEqual(['b', 'a'])
    expect(m.get('w2/pb')?.map((r) => r.run_id)).toEqual(['c'])
  })
})

describe('runHistory', () => {
  it('puts the newest run at the right end and pads the left with empty slots', () => {
    const h = runHistory([run('new', 'running', 3), run('old', 'failed', 1)], 4)
    expect(h.map((r) => r?.run_id ?? null)).toEqual([null, null, 'old', 'new'])
  })

  it('keeps only the last n runs', () => {
    const runs = [3, 2, 1].map((t) => run(`r${t}`, 'succeeded', t))
    expect(runHistory(runs, 2).map((r) => r?.run_id)).toEqual(['r2', 'r3'])
  })

  it('is all empty without runs', () => {
    expect(runHistory(undefined, 3)).toEqual([null, null, null])
  })
})

describe('successRate', () => {
  it('counts only finished runs', () => {
    const r = successRate([
      run('a', 'succeeded', 1),
      run('b', 'failed', 2),
      run('c', 'running', 3),
      run('d', 'paused', 4),
      run('e', 'aborted', 5),
      run('f', 'succeeded', 6),
    ])
    expect(r).toEqual({ ok: 2, of: 4, pct: 50 })
  })

  it('is null when nothing finished', () => {
    expect(successRate([run('a', 'running', 1)])).toBeNull()
    expect(successRate(undefined)).toBeNull()
  })

  it('finishedCount matches the denominator', () => {
    expect(finishedCount([run('a', 'succeeded', 1), run('b', 'running', 2)])).toBe(1)
  })
})

describe('glyphLetter', () => {
  it('takes the first letter of the name, else of the id', () => {
    expect(glyphLetter('branch review', 'x')).toBe('B')
    expect(glyphLetter('  ', 'qa-loop')).toBe('Q')
    expect(glyphLetter('', '')).toBe('?')
  })
})

describe('startedToday', () => {
  it('compares local calendar days', () => {
    const now = new Date(2026, 9, 17, 15).getTime()
    expect(startedToday(run('a', 'running', new Date(2026, 9, 17, 1).getTime()), now)).toBe(true)
    expect(startedToday(run('b', 'running', new Date(2026, 9, 16, 23).getTime()), now)).toBe(false)
  })
})

describe('medianRunTime', () => {
  const v = (version: string, median_ms?: number) =>
    ({ version, duration: { count: 1, median_ms } }) as never
  it('prefers the current version', () => {
    expect(medianRunTime({ versions: [v('1.0.0', 10), v('1.1.0', 20)] }, '1.0.0')).toEqual({
      ms: 10,
      version: '1.0.0',
    })
  })
  it('falls back to the newest version with a median', () => {
    expect(medianRunTime({ versions: [v('1.0.0', 10), v('1.1.0', 20), v('1.2.0')] }, '2.0.0')).toEqual({
      ms: 20,
      version: '1.1.0',
    })
  })
  it('picks one playbook out of a project-wide report', () => {
    const w = (playbook: string, version: string, median_ms: number) =>
      ({ playbook, version, duration: { count: 1, median_ms } }) as never
    expect(medianRunTime({ versions: [w('a', '1.0.0', 10), w('b', '1.0.0', 30)] }, '1.0.0', 'b')).toEqual({
      ms: 30,
      version: '1.0.0',
    })
    expect(medianRunTime({ versions: [w('a', '1.0.0', 10)] }, '1.0.0', 'b')).toBeNull()
  })
  it('is null without one', () => {
    expect(medianRunTime(null, '1.0.0')).toBeNull()
    expect(medianRunTime({ versions: [v('1.0.0')] }, '1.0.0')).toBeNull()
  })
})
