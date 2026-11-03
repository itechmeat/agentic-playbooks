import { describe, expect, it } from 'vitest'
import { floorQuestion, floorStep, launchFields, launchQuestion, launchStep } from './launch'

describe('launchStep', () => {
  const ask = {
    policy: 'launch_requires_confirmation',
    detail: 'playbook `p` asks a person',
    launch_nonce: 'launch-1',
    launch_version: '1.0.0',
    launch_choice: 'current',
  }

  it('offers a confirmation for a refusal with a nonce', () => {
    expect(launchStep(ask)).toEqual({
      kind: 'confirm',
      message: 'playbook `p` asks a person',
      confirmation: { nonce: 'launch-1', version: '1.0.0', choice: 'current' },
    })
  })

  it('does not ask twice for a nonce that was just refused', () => {
    expect(launchStep(ask, { nonce: 'launch-1' })).toEqual({
      kind: 'person',
      message: 'playbook `p` asks a person',
    })
    // A fresh nonce (the permit changed) is asked again.
    expect(launchStep({ ...ask, launch_nonce: 'launch-2' }, { nonce: 'launch-1' })?.kind).toBe(
      'confirm',
    )
  })

  it('leaves a deny refusal to a person', () => {
    const step = launchStep({
      policy: 'launch_requires_person',
      detail: 'only a person launches it',
      launch_by_person: true,
    })
    expect(step).toEqual({ kind: 'person', message: 'only a person launches it' })
  })

  it('asks the person before billed agent work, and leaves a billing deny to a person', () => {
    const billed = {
      policy: 'billing_requires_confirmation',
      detail: 'this run would start billed agent work (codex: billed)',
      launch_nonce: 'launch-9',
      launch_version: '1.0.0',
      launch_choice: 'current',
      billing: { held: ['codex: billed'], sources: ['default'] },
    }
    const step = launchStep(billed)
    expect(step).toEqual({
      kind: 'confirm',
      message: 'this run would start billed agent work (codex: billed)',
      confirmation: { nonce: 'launch-9', version: '1.0.0', choice: 'current' },
      billing: { held: ['codex: billed'], sources: ['default'] },
    })
    // The person is shown the held agents and the settings, never the
    // instructions the refusal gives an agent.
    const question = launchQuestion(step as { message: string; billing: { held: string[]; sources: string[] } })
    expect(question.title).toBe('Start billed agent work?')
    expect(question.held).toEqual(['codex: billed'])
    expect(question.sources).toEqual(['the built-in default'])
    expect(question.reason).not.toContain('confirm_launch')
    expect(launchStep({ policy: 'billing_denied', detail: 'denied' })).toEqual({
      kind: 'person',
      message: 'denied',
    })
  })

  it('ignores refusals that are not the launch policy', () => {
    expect(launchStep({ policy: 'irreversible_requires_confirmation' })).toBeNull()
    expect(launchStep(undefined)).toBeNull()
  })

  it('renders the retry fields and the question', () => {
    expect(launchFields(undefined)).toEqual({})
    expect(launchFields({ nonce: 'n', version: '2.0.0', choice: 'explicit' })).toEqual({
      confirm_launch: 'n',
      launch_version: '2.0.0',
      launch_choice: 'explicit',
    })
    expect(launchFields({ nonce: 'n' })).toEqual({ confirm_launch: 'n' })
    expect(
      launchQuestion({
        message: 'playbook `p` asks a person (agent_launch `ask`: default); ask the person, then call again with confirm_launch',
      }),
    ).toEqual({
      title: 'Launch this playbook?',
      reason: 'playbook `p` asks a person (agent_launch `ask`: default)',
      held: [],
      sources: [],
    })
  })
})

describe('floorStep', () => {
  it('asks to confirm an unconfirmed lowering with both levels', () => {
    const step = floorStep('launch_floor_lowering_unconfirmed', {
      message: 'lowers its floor',
      declared: 'allow',
      floor: 'deny',
    })
    expect(step).toEqual({
      kind: 'confirm',
      message: 'lowers its floor',
      declared: 'allow',
      floor: 'deny',
    })
    if (step?.kind === 'confirm') {
      const q = floorQuestion(step)
      expect(q).toContain('from deny to allow')
      expect(q).not.toMatch(/—|!/)
    }
  })

  it('points a keyless lowering to the terminal', () => {
    expect(
      floorStep('launch_floor_lowering_needs_terminal', { message: 'use apb launch floor' }),
    ).toEqual({ kind: 'terminal', message: 'use apb launch floor' })
  })

  it('ignores other save errors', () => {
    expect(floorStep('validation', {})).toBeNull()
    expect(floorStep(undefined, undefined)).toBeNull()
  })
})
