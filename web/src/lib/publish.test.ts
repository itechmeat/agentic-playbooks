import { describe, expect, it } from 'vitest'
import type { PlanView, PublishView, PushView } from './api.gen'
import {
  approveCommand,
  commitCount,
  commitTags,
  needsALook,
  publishCards,
  publishSkeleton,
  publishSources,
  pushLine,
  refusalText,
  remoteState,
  resolveCommand,
} from './publish'

const plan = (over: Partial<PlanView> = {}): PlanView => ({
  url: 'file:///srv/r.git',
  branch: 'apb/out',
  after: 'b'.repeat(40),
  commits: [
    { sha: 'b'.repeat(40), subject: 'work', this_run: true, apb_run: ['r1'] },
    { sha: 'c'.repeat(40), subject: 'older', this_run: false, apb_run: [] },
  ],
  omitted: 3,
  this_run: 4,
  outside_push_suspected: false,
  plan_digest: 'sha256:x',
  nonce: 'publish-0123',
  ...over,
})

describe('publish plan text', () => {
  it('tags each commit with whether this run made it and its trailers', () => {
    const [mine, older] = plan().commits
    expect(commitTags(mine)).toEqual(['this run', 'Apb-Run: r1'])
    expect(commitTags(older)).toEqual(['before the run'])
    // A trailer written twice (an amend, a cherry-pick) is listed twice.
    expect(commitTags({ ...mine, apb_run: ['r1', 'r1'] })).toEqual(['this run', 'Apb-Run: r1', 'Apb-Run: r1'])
  })

  it('says what the remote holds now', () => {
    expect(remoteState(plan())).toBe('Branch apb/out does not exist on the remote yet; the push creates it.')
    expect(remoteState(plan({ remote_before: 'a'.repeat(40) }))).toBe(
      'Branch apb/out is at aaaaaaaaaaaa; the push moves it forward.',
    )
  })

  it("counts the omitted commits too, and the run's commits among them", () => {
    expect(commitCount(plan())).toBe('5 commits, 4 made by this run')
    expect(commitCount(plan({ commits: plan().commits.slice(0, 1), omitted: 0, this_run: 1 }))).toBe(
      '1 commit, 1 made by this run',
    )
  })

  it('names the terminal command that approves exactly the plan', () => {
    expect(approveCommand('r1', 'ship', 'publish-0123')).toBe('apb publish approve r1 --node ship --nonce publish-0123')
    // Every value is one shell word: a space or `;` never splits or runs.
    expect(approveCommand('r 1', 'ship release', 'publish-0123')).toBe(
      "apb publish approve 'r 1' --node 'ship release' --nonce publish-0123",
    )
    expect(approveCommand('r1', "a;b'c", 'n')).toBe("apb publish approve r1 --node 'a;b'\\''c' --nonce n")
  })

  it('words a pin refusal code and keeps an unknown one', () => {
    expect(refusalText('url_refused')).toContain('without credentials')
    expect(refusalText('something_new')).toBe('something_new')
  })
})

describe('the push after consent', () => {
  const push: PushView = {
    intent: 'si-1',
    state: 'unknown',
    after: 'd'.repeat(40),
    branch: 'apb/out',
    url: 'file:///srv/r.git',
    plan_digest: 'sha256:y',
    detail: 'timed out',
  }

  it('words the ledger state with what that push sent, and names the resolve command', () => {
    expect(pushLine(push)).toBe('Push si-1 of dddddddddddd to branch apb/out: result unknown: timed out')
    expect(pushLine({ ...push, intent: 'si-2', state: 'confirmed', detail: undefined })).toBe(
      'Push si-2 of dddddddddddd to branch apb/out: confirmed on the remote',
    )
    // A state a newer server reports is shown as written.
    expect(pushLine({ ...push, state: 'later' as PushView['state'], detail: undefined })).toBe(
      'Push si-1 of dddddddddddd to branch apb/out: later',
    )
    expect(resolveCommand('r1', push)).toBe('apb intent resolve r1 si-1 --sent|--not-sent')
  })

  it('needs a look only when the result is unknown', () => {
    const v: PublishView = { node: 'p', remote: 'origin', awaiting_consent: false, push }
    expect(needsALook(v)).toBe(true)
    expect(needsALook({ ...v, push: { ...push, state: 'confirmed' } })).toBe(false)
    expect(needsALook({ ...v, push: undefined })).toBe(false)
  })
})

describe('publishCards', () => {
  it('takes the plan of an open gate from the pending review', () => {
    const stale = plan({ nonce: 'publish-old' })
    const views: PublishView[] = [{ node: 'p', remote: 'origin', plan: stale, awaiting_consent: false }]
    const cards = publishCards(views, [{ node: 'p', plan: plan() }])
    expect(cards).toHaveLength(1)
    expect(cards[0].plan?.nonce).toBe('publish-0123')
    expect(cards[0].awaiting_consent).toBe(true)
  })

  it('offers no decision for a request no open gate holds (a stopped run)', () => {
    const left: PublishView = { node: 'p', remote: 'origin', plan: plan(), awaiting_consent: true }
    expect(publishCards([left], [])[0].awaiting_consent).toBe(false)
  })

  it('shows a gate the run block does not list yet, and finished nodes as they are', () => {
    const done: PublishView = { node: 'q', remote: 'origin', awaiting_consent: false, result: 'published' }
    const cards = publishCards([done], [{ node: 'p', plan: plan() }])
    expect(cards.map((c) => [c.node, c.awaiting_consent])).toEqual([
      ['q', false],
      ['p', true],
    ])
    expect(publishCards(null, [])).toEqual([])
  })
})

describe('the node form', () => {
  const nodes = [
    { id: 'start', type: 'start' },
    { id: 'impl', type: 'agent_task' },
    { id: 'each', type: 'agent_task', list: { items: [] } },
    { id: 'build', type: 'script' },
    { id: 'ship', type: 'publish' },
  ]

  it('offers agent tasks and scripts that are not list nodes as sources', () => {
    expect(publishSources(nodes, 'ship')).toEqual(['impl', 'build'])
  })

  it('starts a new node from the last source and origin', () => {
    expect(publishSkeleton(nodes)).toEqual({ from: 'build', remote: 'origin' })
    expect(publishSkeleton([{ id: 'start', type: 'start' }])).toEqual({ remote: 'origin' })
  })
})
