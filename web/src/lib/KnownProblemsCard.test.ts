// @vitest-environment happy-dom
import { describe, it, expect, afterEach, vi } from 'vitest'
import { flushSync, mount, tick, unmount } from 'svelte'
import KnownProblemsCard from './KnownProblemsCard.svelte'
import type { ChangedPassage, ProblemsView } from './api.gen'
import { problem } from './knownproblems.fixture'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

const FIX = 'cc -Wno-error=deprecated -c vendor/zlib.c'

function view(over: Partial<ProblemsView> = {}): ProblemsView {
  return {
    playbook: 'fix-build',
    scope: 'project',
    signed: true,
    unsigned_problems: 0,
    problems: [
      problem({ id: 'kp-shown', workaround: 'run with CFLAGS=-Wno-error' }),
      problem({ id: 'kp-changed', source_now: 'changed', needs_look: true }),
      problem({
        id: 'kp-closed',
        status: 'closed',
        check: {
          digest: 'd',
          verdict: 'closed',
          basis: 'person',
          reason_code: 'quote_new',
          quote: { text: FIX, lines: [3, 3] },
          at_ms: 1,
          by: 'dashboard',
        },
      }),
    ],
    ...over,
  }
}

function render(
  v: ProblemsView | null,
  passages: ChangedPassage[] = [],
  opts: { act?: string | null; busy?: Set<string>; load?: () => Promise<ChangedPassage[]> } = {},
) {
  const onAct = vi.fn(async () => opts.act ?? null)
  const loadPassages = vi.fn(opts.load ?? (async () => passages))
  app = mount(KnownProblemsCard, {
    target: document.body,
    props: { view: v, onAct, loadPassages, busy: opts.busy },
  })
  flushSync()
  return { onAct, loadPassages }
}

const settle = async () => {
  await tick()
  await Promise.resolve()
  await tick()
  flushSync()
}

const typeQuote = (text: string) => {
  const input = q('known-problem-quote-input')[0] as HTMLTextAreaElement
  input.value = text
  input.dispatchEvent(new Event('input'))
  flushSync()
}

const SOURCE = { kind: 'project_file' as const, name: 'scripts/build.sh', digest: 'd' }

const q = (id: string, root: ParentNode = document) => root.querySelectorAll(`[data-testid="${id}"]`)
const item = (id: string) => document.querySelector(`[data-testid="known-problem"][data-id="${id}"]`) as HTMLElement
const click = (el: Element | null | undefined) => {
  ;(el as HTMLButtonElement).click()
  flushSync()
}

describe('KnownProblemsCard', () => {
  it('says so when there are no problems', () => {
    render({ ...view(), problems: [] })
    expect(q('known-problems-empty').length).toBe(1)
  })

  it('groups the problems and shows the workaround, the source and the closing quote', () => {
    render(view())
    const groups = [...q('known-problems-group')].map((g) => g.getAttribute('data-group'))
    expect(groups).toEqual(['look', 'shown', 'done'])
    expect(item('kp-shown').textContent).toContain('Workaround reported: run with CFLAGS=-Wno-error')
    expect(item('kp-shown').textContent).toContain('path:scripts/build.sh')
    expect(q('known-problem-source', item('kp-changed')).length).toBe(1)
    expect(q('known-problem-quote', item('kp-closed'))[0].textContent).toContain(FIX)
    expect(q('known-problem-reopen', item('kp-closed')).length).toBe(1)
  })

  it('only counts the problems of a file this installation did not write', () => {
    render(view({ signed: false, unsigned_problems: 4, problems: [] }))
    expect(q('known-problems-unsigned')[0].textContent).toContain('4 problems in a file this apb installation did not write')
    expect(q('known-problem').length).toBe(0)
    expect(q('known-problems-empty').length).toBe(0)
  })

  it('confirms, dismisses, says it still holds and asks the model', () => {
    const { onAct } = render(view())
    click(q('known-problem-confirm', item('kp-shown'))[0])
    click(q('known-problem-dismiss', item('kp-shown'))[0])
    click(q('known-problem-holds', item('kp-changed'))[0])
    click(q('known-problem-recheck', item('kp-changed'))[0])
    click(q('known-problem-reopen', item('kp-closed'))[0])
    expect(onAct.mock.calls).toEqual([
      ['kp-shown', 'confirm', {}],
      ['kp-shown', 'dismiss', {}],
      ['kp-changed', 'recheck', { verdict: 'holds' }],
      ['kp-changed', 'recheck', {}],
      ['kp-closed', 'reopen', {}],
    ])
    // No recheck is offered for an unchanged source.
    expect(q('known-problem-recheck', item('kp-shown')).length).toBe(0)
  })

  it('closes with a picked passage and refuses a short quote first', async () => {
    const { onAct, loadPassages } = render(
      { ...view(), problems: [problem({ id: 'kp-changed', source_now: 'changed', needs_look: true, source: { kind: 'project_file', name: 'scripts/build.sh', digest: 'd' } })] },
      [{ index: 1, lines: [3, 3], text: FIX }],
    )
    click(q('known-problem-close')[0])
    expect(loadPassages).toHaveBeenCalledWith('kp-changed')
    await tick()
    flushSync()
    const input = q('known-problem-quote-input')[0] as HTMLTextAreaElement
    input.value = 'too short'
    input.dispatchEvent(new Event('input'))
    flushSync()
    expect(q('known-problem-quote-error').length).toBe(1)
    expect((q('known-problem-close-submit')[0] as HTMLButtonElement).disabled).toBe(true)
    click(q('known-problem-passage')[0])
    expect(q('known-problem-quote-error').length).toBe(0)
    click(q('known-problem-close-submit')[0])
    expect(onAct).toHaveBeenCalledWith('kp-changed', 'close', { quote: FIX, reason: undefined })
    await settle()
    expect(q('known-problem-close-form').length).toBe(0)
  })

  it('keeps the form and the quote when the server refuses the close', async () => {
    render({ ...view(), problems: [problem({ id: 'kp-x' })] }, [], { act: 'the quote does not close the problem (quote_not_new)' })
    click(q('known-problem-close')[0])
    await settle()
    typeQuote('set -e and nothing more')
    click(q('known-problem-close-submit')[0])
    await settle()
    expect(q('known-problem-close-form').length).toBe(1)
    expect(q('known-problem-close-error')[0].textContent).toContain('quote_not_new')
    expect((q('known-problem-quote-input')[0] as HTMLTextAreaElement).value).toBe('set -e and nothing more')
  })

  it('shows why the passages did not load', async () => {
    render({ ...view(), problems: [problem({ id: 'kp-x', source: SOURCE, source_now: 'changed' })] }, [], {
      load: async () => {
        throw new Error('passages unavailable')
      },
    })
    click(q('known-problem-close')[0])
    await settle()
    expect(q('known-problem-close-form')[0].textContent).toContain('passages unavailable')
  })

  it('takes a close on the person word without a readable source', async () => {
    render({ ...view(), problems: [problem({ id: 'kp-x', source: SOURCE, source_now: 'unreadable', needs_look: true })] })
    // No "still holds" for a source the server cannot read.
    expect(q('known-problem-holds').length).toBe(0)
    click(q('known-problem-close')[0])
    await settle()
    expect(q('known-problem-close-hint')[0].textContent).toContain('taken on your word')
  })

  it('cancels the form and starts the next one empty', async () => {
    render({ ...view(), problems: [problem({ id: 'kp-x' })] })
    click(q('known-problem-close')[0])
    await settle()
    typeQuote('some typed text here')
    click(q('known-problem-close-cancel')[0])
    expect(q('known-problem-close-form').length).toBe(0)
    click(q('known-problem-close')[0])
    await settle()
    expect((q('known-problem-quote-input')[0] as HTMLTextAreaElement).value).toBe('')
  })

  it('disables only the buttons of a problem whose action is in flight', () => {
    render(view(), [], { busy: new Set(['kp-shown']) })
    expect((q('known-problem-confirm', item('kp-shown'))[0] as HTMLButtonElement).disabled).toBe(true)
    expect((q('known-problem-holds', item('kp-changed'))[0] as HTMLButtonElement).disabled).toBe(false)
  })

  it('lists the other wordings runs reported', () => {
    render({
      ...view(),
      problems: [
        problem({
          id: 'kp-x',
          workaround: 'wait a minute',
          workarounds: [
            { text: 'wait a minute', runs: 3, last_seen_ms: 1 },
            { text: 'skip the approval step', runs: 1, last_seen_ms: 2 },
          ],
        }),
      ],
    })
    expect(q('known-problem-wordings')[0].textContent).toContain('workaround, 1 run: skip the approval step')
  })
})
