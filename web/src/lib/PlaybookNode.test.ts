// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'

// The card outside a flow canvas: its handles need the canvas's context.
vi.mock('@xyflow/svelte', () => ({
  Handle: () => {},
  Position: { Top: 'top', Bottom: 'bottom' },
}))

import PlaybookNode from './PlaybookNode.svelte'
import { retryBadge } from './retrywait'
import type { FlowNodeData } from './graph'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

function show(data: Partial<FlowNodeData>) {
  const full = { kind: 'agent_task', profile: 'coder', ...data } as FlowNodeData
  app = mount(PlaybookNode, { target: document.body, props: { id: 'work', data: full } })
  flushSync()
}

const badge = () => document.querySelector<HTMLElement>('[data-testid="node-retry-wait"]')
const wait = { node: 'work', attempt: 1, retry: 1, delay_ms: 120_000 }
const fmt = (ms: number) => `T${ms}`

describe('PlaybookNode retry badge', () => {
  it('shows a waiting retry with its due time', () => {
    show({ status: 'retry_waiting', retryWait: retryBadge({ ...wait, due: { kind: 'at', at_ms: 5 } }, fmt, (ms) => `t${ms}`) })
    expect(badge()?.textContent?.trim()).toBe('retry waiting · t5')
    expect(badge()?.classList.contains('frozen')).toBe(false)
    expect(badge()?.title).toBe('retry 1 after attempt 1, delay 2m 0s; starts by itself at T5')
  })

  it('marks a waiting retry frozen while the run is paused', () => {
    show({
      status: 'retry_waiting',
      retryWait: retryBadge({ ...wait, due: { kind: 'frozen', remaining_ms: 90_000 } }, fmt),
    })
    expect(badge()?.textContent?.trim()).toBe('retry frozen while paused · 1m 30s')
    expect(badge()?.classList.contains('frozen')).toBe(true)
  })

  it('shows no badge on a node without a waiting retry', () => {
    show({ status: 'succeeded' })
    expect(badge()).toBeNull()
  })
})

describe('PlaybookNode approval refusal', () => {
  it('says why the approval check refused the node, with its code', () => {
    show({
      status: 'failed',
      approval: { code: 'approval_incomplete', text: 'An approved snapshot is incomplete.', detail: 'implement' },
    })
    const line = document.querySelector<HTMLElement>('[data-testid="node-approval-refusal"]')
    expect(line?.textContent?.replace(/\s+/g, ' ').trim()).toBe('approval refused · approval_incomplete')
    expect(line?.title).toBe('An approved snapshot is incomplete. (implement)')
  })
})
