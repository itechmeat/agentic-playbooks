import { describe, it, expect } from 'vitest'
import { render } from 'svelte/server'
import RunDriveNotes from './RunDriveNotes.svelte'

const fmt = (ms: number) => `T${ms}`

describe('RunDriveNotes', () => {
  it('says a released run waits for its retry and a waker resumes it, and who drove it', () => {
    const { body } = render(RunDriveNotes, {
      props: {
        released: { seq: 9, wake_at_ms: 5000, nodes: ['work', 'side'] },
        drivenBy: 'driven by apb 1.0.0, resumed by apb 0.25.0',
        fmt,
      },
    })
    expect(body).toContain('data-testid="run-released"')
    // The sentence itself is `releasedLine`'s (retrywait.test.ts).
    expect(body).toContain('work, side')
    expect(body).toContain('data-testid="run-driven-by"')
    expect(body).toContain('driven by apb 1.0.0, resumed by apb 0.25.0')
  })

  it('shows nothing for a run neither released nor with a recorded binary', () => {
    const { body } = render(RunDriveNotes, { props: { released: null, drivenBy: undefined } })
    expect(body).not.toContain('run-released')
    expect(body).not.toContain('run-driven-by')
  })
})

describe('RunDriveNotes untrusted', () => {
  it('lists each node that renders untrusted text with its accepted reason', () => {
    const { body } = render(RunDriveNotes, {
      props: {
        untrusted: [
          {
            node: 'classify',
            sources: ['connector github on list_open'],
            accepted: 'labels come from params',
          },
        ],
      },
    })
    expect(body).toContain('data-testid="run-untrusted"')
    expect(body).toContain('accepted: labels come from params')
    expect(body).toContain('connector github on list_open')
  })

  it('lists nothing when no node renders untrusted text', () => {
    const { body } = render(RunDriveNotes, { props: { untrusted: [] } })
    expect(body).not.toContain('run-untrusted')
  })
})
