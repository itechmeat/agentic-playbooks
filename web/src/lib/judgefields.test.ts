import { describe, expect, it } from 'vitest'
import { fromYamlField, judgeInputsOf, judgeSlots, questionSummary, slotValuesOf, toYamlField } from './judgefields'

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
})

describe('judge sandbox inputs', () => {
  const state = {
    task: '{{ run.instruction }}',
    clarification: '{{nodes.clarify.review_note}} ({{nodes.clarify.review_decision}})',
    report: 'Verdict {{nodes.qa.output.verdict}} in {{nodes.qa.output}} for {{params.repo}}',
    when: '{{run.id}}',
  }

  it('asks for each input the state reads once, in the order it reads them, and leaves run facts out', () => {
    const slots = judgeSlots(state)
    expect(slots.map((s) => s.key)).toEqual([
      'instruction',
      'reviews.clarify.note',
      'reviews.clarify.decision',
      'outputs.qa',
      'params.repo',
    ])
    expect(slots.find((s) => s.key === 'outputs.qa')!.refs).toEqual(['nodes.qa.output.verdict', 'nodes.qa.output'])
  })

  it('places the typed values where the engine reads them, and reads them back from a run', () => {
    const slots = judgeSlots(state)
    const inputs = judgeInputsOf(slots, {
      instruction: 'Fix the parser',
      'reviews.clarify.note': 'only the YAML one',
      'outputs.qa': '{"verdict":"pass"}',
      'params.repo': 'apb',
    })
    expect(inputs).toEqual({
      params: { repo: 'apb' },
      instruction: 'Fix the parser',
      outputs: { qa: '{"verdict":"pass"}' },
      reviews: { clarify: { decision: '', note: 'only the YAML one' } },
      rejected_outputs: {},
      hooks: {},
      context: '',
    })
    expect(slotValuesOf(slots, inputs)).toEqual({
      instruction: 'Fix the parser',
      'reviews.clarify.note': 'only the YAML one',
      'reviews.clarify.decision': '',
      'outputs.qa': '{"verdict":"pass"}',
      'params.repo': 'apb',
    })
  })
})
