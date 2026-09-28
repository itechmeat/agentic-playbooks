import { describe, expect, it } from 'vitest'
import { fromYamlField, judgeSkeleton, questionSummary, toYamlField } from './judgefields'

describe('judge node fields', () => {
  it('round-trips a questions map through its YAML snippet, order kept', () => {
    const questions = {
      verdict: { type: 'choice', instructions: 'i', criteria: { clean: 'a', needs_fix: 'b', unclear: 'c' } },
      risky: { type: 'noul', instructions: 'r' },
    }
    const text = toYamlField(questions)
    expect(text.indexOf('verdict')).toBeLessThan(text.indexOf('risky'))
    const back = fromYamlField(text)
    expect(back).toEqual({ ok: true, value: questions })
  })

  it('removes an emptied field and refuses a broken snippet', () => {
    expect(fromYamlField('  ')).toEqual({ ok: true, value: undefined })
    const bad = fromYamlField('verdict: { type: choice')
    expect(bad.ok).toBe(false)
    expect(toYamlField(undefined)).toBe('')
    expect(toYamlField('fail')).toBe('fail')
  })

  it('summarizes the questions, list-shaped ones included', () => {
    expect(questionSummary({ verdict: { type: 'choice' }, risky: { type: 'noul' } })).toBe(
      'verdict (choice), risky (noul)',
    )
    expect(questionSummary([{ id: 'v', type: 'score' }, { type: 'noul' }])).toBe('v (score), q2 (noul)')
    expect(questionSummary(undefined)).toBe('')
  })

  it('starts a new judge node with an unclear option and a declared fallback', () => {
    const s = judgeSkeleton() as { questions: { verdict: { criteria: Record<string, string> } }; on_unavailable: string }
    expect(Object.keys(s.questions.verdict.criteria)).toContain('unclear')
    expect(s.on_unavailable).toBe('fail')
  })
})
