// @vitest-environment happy-dom
import { describe, it, expect, afterEach, beforeEach, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import PublishPanel from './PublishPanel.svelte'
import { auth } from './auth.svelte'
import type { PlanView, PublishView } from './api.gen'

let app: ReturnType<typeof mount> | null = null
// A dashboard with keys, signed in: the one that takes a publish consent.
beforeEach(() => auth.set({ required: true, authenticated: true, checked: true }))
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
  auth.set({ required: false, authenticated: true, checked: false })
})

const byTestId = (id: string) => document.querySelector<HTMLElement>(`[data-testid="${id}"]`)
const text = (id: string) => byTestId(id)?.textContent?.replace(/\s+/g, ' ').trim()

const plan: PlanView = {
  url: 'file:///srv/r.git',
  branch: 'apb/out',
  after: 'b'.repeat(40),
  remote_before: 'a'.repeat(40),
  commits: [
    { sha: 'b'.repeat(40), subject: 'work of the run', this_run: true, apb_run: ['r1'] },
    { sha: 'c'.repeat(40), subject: '<b>older</b>', this_run: false, apb_run: [] },
  ],
  omitted: 0,
  this_run: 1,
  outside_push_suspected: true,
  plan_digest: 'sha256:x',
  nonce: 'publish-0123',
}

function render(props: Record<string, unknown>) {
  app = mount(PublishPanel, { target: document.body, props: { runId: 'r1', onDecide: () => {}, ...props } })
  flushSync()
}

describe('PublishPanel', () => {
  it('renders the exact plan with the commit tags and the remote state', () => {
    render({ gates: [{ node: 'ship', plan }] })
    expect(text('publish-after')).toBe(plan.after)
    expect(text('publish-url')).toBe('file:///srv/r.git')
    expect(text('publish-branch')).toBe('apb/out')
    expect(text('publish-remote')).toBe('Branch apb/out is at aaaaaaaaaaaa; the push moves it forward.')
    const commits = [...document.querySelectorAll('[data-testid="publish-commits"] li')].map((li) =>
      li.textContent?.replace(/\s+/g, ' ').trim(),
    )
    expect(commits).toEqual([
      'bbbbbbbbbbbb work of the run this run Apb-Run: r1',
      'cccccccccccc <b>older</b> before the run',
    ])
    // A subject is text, never markup.
    expect(document.querySelector('[data-testid="publish-commits"] b')).toBeNull()
    expect(byTestId('publish-outside')).not.toBeNull()
  })

  it('shows the node guidance as text above an open plan', () => {
    render({ gates: [{ node: 'ship', plan, prompt: 'Check <i>the</i> subjects.' }] })
    expect(text('publish-guidance')).toBe('Check <i>the</i> subjects.')
    expect(document.querySelector('[data-testid="publish-guidance"] i')).toBeNull()
  })

  it('posts the plan nonce with the decision', () => {
    const onDecide = vi.fn()
    render({ gates: [{ node: 'ship', plan }], onDecide })
    expect(text('publish-approve')).toBe('Publish exactly this')
    byTestId('publish-approve')?.click()
    byTestId('publish-skip')?.click()
    expect(onDecide.mock.calls).toEqual([
      ['ship', 'publish', 'publish-0123'],
      ['ship', 'skip', 'publish-0123'],
    ])
  })

  it('offers the terminal command instead of the consent button on a dashboard without keys', () => {
    auth.set({ required: false, authenticated: true, checked: true })
    const onDecide = vi.fn()
    render({ gates: [{ node: 'ship', plan }], onDecide })
    expect(byTestId('publish-approve')).toBeNull()
    expect(text('publish-keyless')).toContain('apb publish approve r1 --node ship --nonce publish-0123')
    // Skipping pushes nothing and stays here.
    byTestId('publish-skip')?.click()
    expect(onDecide.mock.calls).toEqual([['ship', 'skip', 'publish-0123']])
  })

  it('keeps the consent button until the dashboard knows whether it has keys', () => {
    auth.set({ required: false, authenticated: true, checked: false })
    render({ gates: [{ node: 'ship', plan }] })
    expect(byTestId('publish-approve')).not.toBeNull()
    expect(byTestId('publish-keyless')).toBeNull()
  })

  it('disables the buttons while a decision is in flight', () => {
    render({ gates: [{ node: 'ship', plan }], posting: 'ship:publish' })
    expect((byTestId('publish-approve') as HTMLButtonElement).disabled).toBe(true)
    expect((byTestId('publish-skip') as HTMLButtonElement).disabled).toBe(true)
  })

  it('shows an unknown push as needing a look, with the resolve command and no buttons', () => {
    const view: PublishView = {
      node: 'ship',
      remote: 'origin',
      url: 'file:///srv/r.git',
      plan,
      awaiting_consent: false,
      push: {
        intent: 'si-9',
        state: 'unknown',
        after: plan.after,
        branch: 'apb/out',
        url: plan.url,
        plan_digest: 'sha256:x',
        detail: 'timed out',
      },
      result: 'publish_unknown: timed out',
    }
    render({ views: [view] })
    expect(text('publish-push')).toBe('Push si-9 of bbbbbbbbbbbb to branch apb/out: result unknown: timed out')
    expect(text('publish-needs-look')).toContain('apb intent resolve r1 si-9 --sent|--not-sent')
    expect(byTestId('publish-approve')).toBeNull()
    expect(text('publish-result')).toBe('Result: publish_unknown: timed out')
  })

  it('shows a confirmed push without a look, and a pin refusal in words', () => {
    const confirmed: PublishView = {
      node: 'ship',
      remote: 'origin',
      awaiting_consent: false,
      push: {
        intent: 'si-1',
        state: 'confirmed',
        after: plan.after,
        branch: 'apb/out',
        url: plan.url,
        plan_digest: 'sha256:x',
      },
      result: 'published',
    }
    const refused: PublishView = { node: 'other', remote: 'origin', pin_refusal: 'url_refused', awaiting_consent: false }
    render({ views: [confirmed, refused] })
    expect(byTestId('publish-needs-look')).toBeNull()
    expect(text('publish-push')).toBe('Push si-1 of bbbbbbbbbbbb to branch apb/out: confirmed on the remote')
    expect(text('publish-pin-refusal')).toContain('without credentials')
    // The refused remote is named even without a URL.
    expect(document.body.textContent).toContain('remote origin')
  })

  it('renders a commit whose trailer repeats', () => {
    const twice = { ...plan, commits: [{ ...plan.commits[0], apb_run: ['r1', 'r1'] }] }
    render({ gates: [{ node: 'ship', plan: twice }] })
    expect(text('publish-commits')).toBe('bbbbbbbbbbbb work of the run this run Apb-Run: r1 Apb-Run: r1')
  })

  it('renders nothing for a run without a publish node', () => {
    render({ views: [], gates: [] })
    expect(byTestId('publish-panel')).toBeNull()
  })
})
