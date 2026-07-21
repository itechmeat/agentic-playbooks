import { describe, it, expect } from 'vitest'
import { render } from 'svelte/server'
import QuestionPanel from './QuestionPanel.svelte'

describe('QuestionPanel', () => {
  it('renders the question text and an option button per option', () => {
    const { body } = render(QuestionPanel, {
      props: {
        question: { node: 'ask', question: 'Which database?', options: ['pg', 'sqlite'] },
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
        question: { node: 'ask', question: 'Anything else?', options: [] },
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
        question: { node: 'ask', question: hostile, options: [] },
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
        question: { node: 'ask', question: 'q', options: [hostile] },
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
        question: { node: 'ask', question: multiline, options: [] },
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
        question: { node: 'ask', question: 'q', options: ['a'] },
        posting: true,
        onAnswer: () => {},
      },
    })
    // Both the option button and the free-text submit button must be disabled
    // while an answer is in flight.
    const disabledCount = (body.match(/disabled/g) ?? []).length
    expect(disabledCount).toBeGreaterThanOrEqual(3)
  })
})
