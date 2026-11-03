// @vitest-environment happy-dom
import { afterEach, describe, expect, it } from 'vitest'
import { mount, unmount } from 'svelte'
import InterventionsCard from './InterventionsCard.svelte'
import type { InterventionStats, KnownCount } from './api.gen'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

const k = (value: number, known_runs = 3, runs = 3): KnownCount => ({ value, known_runs, runs })

// The three-run version: one channel decision; one automatic decision and an
// engine action; a channel decision, an operator action and a correction.
const stats: InterventionStats = {
  runs: 3,
  total: k(4),
  per_run_median: 1,
  per_run_mean: 1.33,
  without_any: k(1),
  automatic_total: k(1),
  categories: [
    {
      category: 'review',
      unit: 'gate visits',
      denominator: 3,
      count: k(2),
      split: { channel: k(2), automatic: k(1), withdrawn: k(0) },
    },
    { category: 'operator', unit: 'runs', denominator: 3, count: k(1), split: { pause: k(1) } },
    {
      category: 'supervisor_run',
      unit: 'runs',
      denominator: 3,
      count: k(1),
      split: { context_append: k(1) },
    },
  ],
  engine: { infra_retry: 1 },
  other: {},
  actor_unknown: {},
}

function render(s: InterventionStats) {
  app = mount(InterventionsCard, { target: document.body, props: { stats: s } })
  return document.body
}

describe('InterventionsCard', () => {
  it('shows counts beside their denominators and keeps engine actions apart', () => {
    const body = render(stats)
    expect(body.querySelector('[data-testid="interventions-total"]')?.textContent).toBe(
      '4 over 3 finished runs, median 1 and mean 1.33 per run',
    )
    expect(body.querySelector('[data-testid="interventions-without-any"]')?.textContent).toBe('1 of 3')
    const rows = [...body.querySelectorAll('[data-testid="interventions-category"]')].map((e) => e.textContent)
    expect(rows[0]).toBe('2 over 3 gate visits (channel 2, automatic 1)')
    expect(body.querySelector('[data-testid="interventions-engine"]')?.textContent).toBe(
      'infra retry 1 (not interventions)',
    )
    expect(body.querySelector('[data-testid="interventions-note"]')).toBeNull()
    expect(body.textContent?.toLowerCase()).not.toContain('human')
  })

  it('says where a figure is known instead of reading it as zero', () => {
    const partly: InterventionStats = {
      ...stats,
      total: k(1, 1, 2),
      without_any: k(0, 1, 2),
      categories: [{ ...stats.categories[0], count: k(1, 1, 2), split: { channel: k(1, 1, 2) } }],
    }
    const body = render(partly)
    expect(body.querySelector('[data-testid="interventions-total"]')?.textContent).toContain(
      '1 (known in 1 of 2 runs)',
    )
    expect(body.querySelector('[data-testid="interventions-note"]')).not.toBeNull()
  })
})
