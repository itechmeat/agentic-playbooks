import { describe, it, expect } from 'vitest'
import { render } from 'svelte/server'
import HostTaskPanel from './HostTaskPanel.svelte'
import type { HostTaskEntry } from './rungates'

const task = (over: Partial<HostTaskEntry> = {}): HostTaskEntry => ({
  runId: 'r-1',
  taskId: 'plan-1',
  node: 'plan',
  attempt: 1,
  prompt: 'Plan the work',
  rolePrompt: null,
  modelHint: null,
  deadline: null,
  ...over,
})

describe('HostTaskPanel', () => {
  it('renders nothing while no host task waits', () => {
    const { body } = render(HostTaskPanel, { props: { tasks: [] } })
    expect(body).not.toContain('Host tasks')
    expect(body).not.toContain('data-testid="host-tasks"')
  })

  it('shows each task with its node, id, deadline and a collapsed prompt', () => {
    const { body } = render(HostTaskPanel, {
      props: {
        tasks: [
          task({ deadline: 60_000, modelHint: 'sonnet', rolePrompt: 'You plan.' }),
          task({ taskId: 'build-1', node: 'build', prompt: 'Build it' }),
        ],
        now: 0,
      },
    })
    expect(body).toContain('Host tasks')
    expect(body).toContain('plan-1')
    expect(body).toContain('build-1')
    expect(body).toContain('model hint sonnet')
    expect(body).toContain('deadline in 60s')
    // The prompt is there but inside a closed <details>: hidden until opened.
    expect(body).toMatch(/<details[^>]*>\s*<summary[^>]*>Prompt<\/summary>/)
    expect(body).not.toMatch(/<details[^>]*open/)
    expect(body).toContain('Plan the work')
    expect(body).toContain('Role prompt')
  })

  it('escapes a hostile prompt as plain text', () => {
    const { body } = render(HostTaskPanel, {
      props: { tasks: [task({ prompt: '<script>evil()</script>' })] },
    })
    expect(body).not.toContain('<script>evil()')
    expect(body).toContain('&lt;script>evil()')
  })
})
