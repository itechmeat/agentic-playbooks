// @vitest-environment happy-dom
import { describe, it, expect, afterEach } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import RunOutputsPanel, { MISSING_REASON_TEXTS } from './RunOutputsPanel.svelte'
import { MISSING_REASONS } from './consts.gen'
import type { ResultRef, RunOutputs } from './api.gen'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

const ref = (node: string, ordinal: number, seq: number): ResultRef => ({
  execution: { node, ordinal, ended_seq: seq },
  settlement_seq: seq,
  part: { kind: 'fields' },
})

const OUTPUTS: RunOutputs = {
  final: true,
  definition: 'stored',
  outputs: [
    { name: 'verdict', pick: 'last', state: 'value', value: 'v3', source: ref('w', 3, 16) },
    { name: 'maybe', pick: 'last', state: 'null', source: ref('w', 3, 16) },
    { name: 'absent', pick: 'last', state: 'missing', reason: 'field_absent', source: ref('w', 3, 16) },
    { name: 'other', pick: 'last', state: 'missing', reason: 'not_run' },
    {
      name: 'all',
      pick: 'all',
      state: 'value',
      value: ['v1', { n: 3 }],
      values: [
        { state: 'value', value: 'v1', source: ref('w', 1, 4) },
        { state: 'value', value: { n: 3 }, source: ref('w', 3, 16) },
      ],
    },
  ],
}

function rows() {
  return [...document.querySelectorAll('[data-testid="run-output"]')]
}

describe('RunOutputsPanel', () => {
  it('shows each output with its state, value, reason and source', () => {
    app = mount(RunOutputsPanel, { target: document.body, props: { outputs: OUTPUTS } })
    flushSync()
    expect(rows().map((r) => r.getAttribute('data-state'))).toEqual(['value', 'null', 'missing', 'missing', 'value'])
    const states = [...document.querySelectorAll('[data-testid="run-output-state"]')].map((s) => s.textContent?.trim())
    expect(states).toEqual([
      'value',
      'null',
      `missing: ${MISSING_REASON_TEXTS.field_absent}`,
      `missing: ${MISSING_REASON_TEXTS.not_run}`,
      'value',
    ])
    expect(document.querySelector('[data-testid="run-output-value"]')?.textContent).toBe('v3')
    const text = rows()[4].textContent ?? ''
    expect(text).toContain('{"n":3}')
    expect(text).toContain('w #1')
    expect(document.querySelector('[data-testid="run-outputs-note"]')?.textContent?.trim()).toBe('Final')
  })

  it('opens the event that settled a value', () => {
    const cited: number[] = []
    app = mount(RunOutputsPanel, { target: document.body, props: { outputs: OUTPUTS, onCite: (s: number) => cited.push(s) } })
    flushSync()
    const sources = [...document.querySelectorAll<HTMLButtonElement>('[data-testid="run-output-source"]')]
    expect(sources[0].textContent?.trim()).toBe('w #3')
    sources[0].click()
    sources[sources.length - 2].click()
    expect(cited).toEqual([16, 4])
  })

  it('says when the values may still change or come from the current definition', () => {
    app = mount(RunOutputsPanel, {
      target: document.body,
      props: { outputs: { ...OUTPUTS, final: false, definition: 'current' } },
    })
    flushSync()
    expect(document.querySelector('[data-testid="run-outputs-note"]')?.textContent?.trim()).toBe(
      'Not final: the run may still change them · read against the current definition',
    )
  })

  it('has words for every missing reason', () => {
    for (const r of MISSING_REASONS) expect(MISSING_REASON_TEXTS[r]).toBeTruthy()
  })

  it('renders nothing for a run without declared outputs', () => {
    app = mount(RunOutputsPanel, { target: document.body, props: { outputs: null } })
    flushSync()
    expect(document.querySelector('[data-testid="run-outputs"]')).toBeNull()
  })
})
