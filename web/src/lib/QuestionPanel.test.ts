import { describe, it, expect } from 'vitest'
import { render } from 'svelte/server'
import QuestionPanel from './QuestionPanel.svelte'
import type { QuestionEntry } from './rungates'

describe('QuestionPanel', () => {
  it('renders the question text and an option button per option', () => {
    const { body } = render(QuestionPanel, {
      props: {
        question: { runId: 'r', node: 'ask', question: 'Which database?', options: ['pg', 'sqlite'] },
        onAnswer: () => {},
      },
    })
    expect(body).toContain('ask')
    expect(body).toContain('Which database?')
    expect(body).toContain('>pg<')
    expect(body).toContain('>sqlite<')
    // A free-text field is always present, options or not.
    expect(body).toContain('Type an answer')
  })

  it('renders the free-text field with no options', () => {
    const { body } = render(QuestionPanel, {
      props: {
        question: { runId: 'r', node: 'ask', question: 'Anything else?', options: [] },
        onAnswer: () => {},
      },
    })
    expect(body).toContain('Anything else?')
    expect(body).toContain('Type an answer')
  })

  it('escapes hostile question text as plain text, never as markup', () => {
    const hostile = '<img src=x onerror=alert(1)><script>evil()</script>'
    const { body } = render(QuestionPanel, {
      props: {
        question: { runId: 'r', node: 'ask', question: hostile, options: [] },
        onAnswer: () => {},
      },
    })
    expect(body).not.toContain('<img')
    expect(body).not.toContain('<script>evil')
    expect(body).toContain('&lt;img')
  })

  it('escapes a hostile option label the same way', () => {
    const hostile = '<b>bold</b>'
    const { body } = render(QuestionPanel, {
      props: {
        question: { runId: 'r', node: 'ask', question: 'q', options: [hostile] },
        onAnswer: () => {},
      },
    })
    expect(body).not.toContain('<b>bold</b>')
    expect(body).toContain('&lt;b>bold&lt;/b>')
  })

  it('preserves literal newlines in the question text inside the pre-wrap element', () => {
    const multiline = 'first line\nsecond line\nthird line'
    const { body } = render(QuestionPanel, {
      props: {
        question: { runId: 'r', node: 'ask', question: multiline, options: [] },
        onAnswer: () => {},
      },
    })
    const match = body.match(/<pre[^>]*whitespace-pre-wrap[^>]*>([\s\S]*?)<\/pre>/)
    expect(match).not.toBeNull()
    expect(match?.[1]).toContain(multiline)
  })

  it('disables the option buttons and submit while posting', () => {
    const { body } = render(QuestionPanel, {
      props: {
        question: { runId: 'r', node: 'ask', question: 'q', options: ['a'] },
        posting: true,
        onAnswer: () => {},
      },
    })
    // Both the option button and the free-text submit button must be disabled
    // while an answer is in flight.
    const disabledCount = (body.match(/disabled/g) ?? []).length
    expect(disabledCount).toBeGreaterThanOrEqual(3)
  })

  // The next-actions panel leaves the question's `answer_question` to this
  // card, so what an unattended run does there must show here, as on the CLI.
  it('shows what an unattended run does at the question, marked where it stops', () => {
    const line = (unattended: QuestionEntry['unattended']) =>
      render(QuestionPanel, {
        props: { question: { runId: 'r', node: 'ask', question: 'Which?', options: [], unattended }, onAnswer: () => {} },
      }).body
    const defaults = line({ kind: 'answers_default' })
    expect(defaults).toMatch(/<p class="unattended[^"]*"[^>]*data-testid="question-unattended"[^>]*>\s*unattended: posts the default answer at once/)
    expect(defaults).not.toMatch(/class="unattended[^"]*\bstops\b/)
    const fails = line({ kind: 'fails_run', code: 'hitl_required' })
    expect(fails).toMatch(/class="unattended[^"]*\bstops\b[^"]*"[^>]*data-testid="question-unattended"[^>]*>\s*unattended: fails the run \(hitl_required\)/)
  })
})
