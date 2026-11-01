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
    expect(launchQuestion({ message: 'why' })).toContain('why')
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
