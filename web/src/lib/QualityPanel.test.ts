// @vitest-environment happy-dom
import { describe, it, expect, afterEach, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import QualityPanel from './QualityPanel.svelte'
import type { Grade } from './api.gen'
import type { WfEvent } from './types'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

const events: WfEvent[] = [
  { seq: 1, ts: 1, type: 'attempt_started', node: 'fix', attempt: 1, transcript: 'attempts/fix-1' },
  { seq: 5, ts: 5, type: 'node_finished', node: 'fix' },
]

const rules: Grade = {
  schema: 1,
  grade_id: 'rules-1-1000-aaaa',
  run_id: 'r1',
  playbook: 'p',
  version: '1.0.0',
  task_type: 'default',
  grader: { kind: 'rules', version: 1 },
  created_at: Date.UTC(2026, 9, 27, 12, 0),
  criteria: [
    {
      id: 'loops',
      source: 'builtin',
      digest: 'sha256:x',
      subject: 'run',
      level: 'poor',
      reason_code: 'repeated_calls',
      why: 'the same failing command ran three times',
      cites: [{ ref: 't:fix-1/session#4' }, { ref: 'j:5' }, { ref: 'x:9' }],
      counts_for_promotion: true,
    },
    {
      id: 'consent',
      source: 'builtin',
      digest: 'sha256:y',
      subject: 'run',
      level: 'not_applicable',
      reason_code: 'no_irreversible_action',
      why: 'no irreversible action ran',
      counts_for_promotion: false,
    },
  ],
  reading: { budget_chars: 0, read_chars: 0 },
}

function render(props: Partial<{ grades: Grade[]; ended: boolean; grading: boolean }>) {
  const onGrade = vi.fn()
  const onReopen = vi.fn()
  const onCite = vi.fn()
  app = mount(QualityPanel, {
    target: document.body,
    props: { grades: [], events, ended: true, onGrade, onReopen, onCite, ...props },
  })
  flushSync()
  return { onGrade, onReopen, onCite }
}

const q = (id: string) => document.querySelectorAll(`[data-testid="${id}"]`)

describe('QualityPanel', () => {
  it('shows each criterion with its level and opens a cited event', () => {
    const { onCite } = render({ grades: [rules] })
    const rows = q('run-quality-criterion')
    expect(rows.length).toBe(2)
    expect(rows[0].getAttribute('data-level')).toBe('poor')
    expect(rows[0].textContent).toContain('the same failing command ran three times')
    expect(rows[1].textContent).toContain('not compared')
    const cites = q('run-quality-cite') as NodeListOf<HTMLButtonElement>
    // The unknown ref has no event to open.
    expect(cites.length).toBe(2)
    expect(cites[0].title).toBe('runs/r1/attempts/fix-1/session.jsonl')
    cites[0].click()
    cites[1].click()
    expect(onCite.mock.calls).toEqual([[1], [5]])
    expect(q('run-quality-reading')[0].textContent).toContain('read 0 of 0 chars')
  })

  it('grades an ended run by rules on request', () => {
    const { onGrade } = render({})
    expect(q('run-quality-empty')[0].textContent).toContain('Not graded yet.')
    ;(q('run-quality-grade-rules')[0] as HTMLButtonElement).click()
    expect(onGrade).toHaveBeenCalledOnce()
  })

  it('offers no grading while the run is live or a grade is running', () => {
    render({ ended: false })
    expect(q('run-quality-grade-rules').length).toBe(0)
    expect(q('run-quality-empty')[0].textContent).toContain('once the run ends')
    unmount(app!)
    app = null
    render({ grading: true })
    expect((q('run-quality-grade-rules')[0] as HTMLButtonElement).disabled).toBe(true)
  })

  it('offers Grade again only once a host graded the run', () => {
    render({ grades: [rules] })
    expect(q('run-quality-reopen').length).toBe(0)
    unmount(app!)
    app = null
    const { onReopen } = render({ grades: [rules, { ...rules, grade_id: 'h1', grader: { kind: 'host', version: 1 } }] })
    ;(q('run-quality-reopen')[0] as HTMLButtonElement).click()
    expect(onReopen).toHaveBeenCalledOnce()
  })
})
