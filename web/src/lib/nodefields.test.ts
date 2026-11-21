import { describe, it, expect } from 'vitest'
import { NODE_FIELDS, readContinueSession, writeContinueSession } from './nodefields'

describe('NODE_FIELDS', () => {
  it('gives every field a written label, not the raw schema key', () => {
    for (const [key, info] of Object.entries(NODE_FIELDS)) {
      expect(info.label, key).not.toContain('_')
      expect(info.label[0], key).toBe(info.label[0]?.toUpperCase())
    }
  })

  it('explains every field in prose rather than restating the label', () => {
    for (const [key, info] of Object.entries(NODE_FIELDS)) {
      expect(info.hint.length, key).toBeGreaterThan(40)
      expect(info.hint.trim().endsWith('.'), key).toBe(true)
    }
  })
})

describe('continue_session', () => {
  it('continue_session reads both shapes', () => {
    expect(readContinueSession('assess')).toEqual({ from: 'assess', fork: false })
    expect(readContinueSession({ from: 'assess', mode: 'fork' })).toEqual({ from: 'assess', fork: true })
    expect(readContinueSession({ from: 'assess', mode: 'continue' })).toEqual({ from: 'assess', fork: false })
    expect(readContinueSession({ from: 'assess' })).toEqual({ from: 'assess', fork: false })
    expect(readContinueSession(undefined)).toEqual({ from: '', fork: false })
    expect(writeContinueSession('assess', false)).toBe('assess')
    expect(writeContinueSession('assess', true)).toEqual({ from: 'assess', mode: 'fork' })
    expect(writeContinueSession('', true)).toBeUndefined()
  })

  it('names the profile a fork needs besides the agent and the CLI', () => {
    const hint = NODE_FIELDS.continue_session.hint
    expect(hint).toContain('same profile binding the source ran on')
    expect(hint).toContain('another agent or profile')
  })
})
