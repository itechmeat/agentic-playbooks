// @vitest-environment happy-dom
import { describe, it, expect, afterEach, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import CandidateQualityCard from './CandidateQualityCard.svelte'
import type { CandidateQualityView } from './api.gen'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

const tally = (good: number, poor = 0, na = 0) => ({ good, partial: 0, poor, not_applicable: na, not_enough_data: 0 })

const quality: CandidateQualityView = {
  version: '1.0.1',
  current: '1.0.0',
  trials: 2,
  gate: { compare: 'not_worse', min_runs: 3, max_trials: 4, window: 20, margin: 0.35 },
  decision: {
    verdict: 'wait',
    reason: 'too few graded trials',
    per_criterion: [
      {
        key: { criterion: 'outcome', digest: 'sha256:a', grader: 'rules/1', task_type: 'default' },
        candidate: tally(2),
        current: tally(3, 0, 1),
        result: 'insufficient',
      },
    ],
  },
  recorded: { verdict: 'wait', reason: 'too few graded trials', decided_at: 1, decided_by: 'apb' },
}

function render(candidate: CandidateQualityView, extra: { frozen?: boolean; busy?: boolean } = {}) {
  const onPromote = vi.fn()
  const onKeepCurrent = vi.fn()
  app = mount(CandidateQualityCard, {
    target: document.body,
    props: { candidate, onPromote, onKeepCurrent, ...extra },
  })
  flushSync()
  return { onPromote, onKeepCurrent }
}

const q = (id: string) => document.querySelector(`[data-testid="${id}"]`) as HTMLElement

describe('CandidateQualityCard', () => {
  it('shows the comparison per criterion and the verdict', () => {
    render(quality)
    expect(q('candidate-line').textContent).toContain('1.0.1')
    expect(q('candidate-line').textContent).toContain('2 trial runs')
    expect(q('candidate-gate').textContent).toContain('not worse than current')
    expect(q('candidate-gate').textContent).toContain('35 points above')
    expect(q('candidate-verdict').textContent).toContain('waiting for more trials')
    const row = q('candidate-row')
    expect(row.getAttribute('data-result')).toBe('insufficient')
    expect(row.title).toBe('outcome (rules/1)')
    expect(row.textContent).toContain('candidate 2/0/0')
    expect(row.querySelectorAll('.tallies span')[1].getAttribute('title')).toBe('1 not applicable')
    expect(q('candidate-recorded').textContent).toContain('by apb')
  })

  it('says no trial is graded yet and shows what current has', () => {
    const empty = tally(0)
    render({
      ...quality,
      decision: {
        verdict: 'wait',
        reason: 'not shown to be not worse yet on outcome',
        per_criterion: [
          {
            key: { criterion: 'outcome', digest: 'sha256:a', grader: 'rules/1', task_type: '' },
            candidate: empty,
            current: tally(18, 2),
            result: 'insufficient',
          },
        ],
      },
    })
    const row = q('candidate-row')
    expect(row.textContent).toContain('candidate no graded trials yet')
    expect(row.textContent).toContain('current 18/0/2')
    expect(row.textContent).not.toContain('0/0/0')
  })

  it('promotes, or keeps current with the reason typed', () => {
    const { onPromote, onKeepCurrent } = render(quality)
    ;(q('candidate-promote') as HTMLButtonElement).click()
    expect(onPromote).toHaveBeenCalledOnce()
    const input = q('candidate-reason') as HTMLInputElement
    input.value = 'the old report reads better'
    input.dispatchEvent(new Event('input'))
    flushSync()
    ;(q('candidate-keep-current') as HTMLButtonElement).click()
    expect(onKeepCurrent).toHaveBeenCalledWith('the old report reads better')
  })

  it('says a success policy decides, and a frozen playbook cannot promote', () => {
    render({ version: '1.0.1', current: '1.0.0', trials: 0 }, { frozen: true })
    expect(q('candidate-gate').textContent).toContain('not by quality')
    expect(q('candidate-table')).toBeNull()
    expect((q('candidate-promote') as HTMLButtonElement).disabled).toBe(true)
    expect((q('candidate-keep-current') as HTMLButtonElement).disabled).toBe(false)
  })
})
