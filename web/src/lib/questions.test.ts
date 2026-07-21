import { describe, it, expect } from 'vitest'
import { pendingQuestions, pendingQuestionsFromPayload } from './questions'
import type { PendingQuestion, WfEvent } from './types'

const ev = (seq: number, type: string, extra: Record<string, unknown> = {}): WfEvent =>
  ({ seq, ts: 0, type, ...extra }) as WfEvent

describe('pendingQuestions', () => {
  it('surfaces a question asked but not yet answered', () => {
    const events = [
      ev(0, 'question_asked', { node: 'ask', question: 'which way', options: ['left', 'right'] }),
    ]
    expect(pendingQuestions(events)).toEqual([
      { node: 'ask', question: 'which way', options: ['left', 'right'] },
    ])
  })

  it('clears once answered', () => {
    const events = [
      ev(0, 'question_asked', { node: 'ask', question: 'which way', options: [] }),
      ev(1, 'question_answered', { node: 'ask', answer: 'left', answered_by: 'human' }),
    ]
    expect(pendingQuestions(events)).toEqual([])
  })

  it('re-surfaces a second question for the same node after the first is answered', () => {
    const events = [
      ev(0, 'question_asked', { node: 'ask', question: 'Q1', options: [] }),
      ev(1, 'question_answered', { node: 'ask', answer: 'a1', answered_by: 'human' }),
      ev(2, 'question_asked', { node: 'ask', question: 'Q2', options: [] }),
    ]
    expect(pendingQuestions(events)).toEqual([{ node: 'ask', question: 'Q2', options: [] }])
  })

  it('defaults options to an empty array when absent', () => {
    const events = [ev(0, 'question_asked', { node: 'ask', question: 'q' })]
    expect(pendingQuestions(events)).toEqual([{ node: 'ask', question: 'q', options: [] }])
  })
})

describe('pendingQuestionsFromPayload', () => {
  it('is empty when there is no pending question', () => {
    expect(pendingQuestionsFromPayload(null)).toEqual([])
    expect(pendingQuestionsFromPayload(undefined)).toEqual([])
  })

  it('surfaces the channel-derived pending question from the run payload', () => {
    const pending: PendingQuestion = {
      node: 'ask',
      question: 'which way',
      options: ['left', 'right'],
      answer_by: 'human',
      asked_at: 0,
    }
    expect(pendingQuestionsFromPayload(pending)).toEqual([
      { node: 'ask', question: 'which way', options: ['left', 'right'] },
    ])
  })
})
