import { describe, expect, it } from 'vitest'
import {
  PROFILE_ACCENTS,
  isTerminalNodeKind,
  nodeKindColor,
  profileAccent,
  profileColor,
  statusColor,
} from './profilecolor'

describe('profileAccent', () => {
  it('is stable for the same name', () => {
    expect(profileAccent('developer')).toBe(profileAccent('developer'))
    expect(profileAccent('  reviewer ')).toBe(profileAccent('reviewer'))
  })

  it('only hands out profile accents, never the semantic ones', () => {
    const names = Array.from({ length: 200 }, (_, i) => `profile-${i}`)
    const seen = new Set(names.map((n) => profileAccent(n)))
    for (const a of seen) expect(PROFILE_ACCENTS).toContain(a)
    expect(seen.has('green')).toBe(false)
    expect(seen.has('amber')).toBe(false)
    expect(seen.has('red')).toBe(false)
  })

  it('spreads common names over more than one accent', () => {
    const names = ['developer', 'reviewer', 'qa', 'janitor', 'facilitator', 'planner']
    expect(new Set(names.map((n) => profileAccent(n))).size).toBeGreaterThan(1)
  })

  it('gives no accent to an empty name', () => {
    expect(profileAccent('')).toBeNull()
    expect(profileAccent(undefined)).toBeNull()
    expect(profileColor(null)).toBe('var(--c-muted)')
  })
})

describe('nodeKindColor', () => {
  it('colors judge, human review and terminals by kind', () => {
    expect(nodeKindColor('judge', 'developer')).toBe('var(--c-green)')
    expect(nodeKindColor('human_review')).toBe('var(--c-amber)')
    expect(nodeKindColor('start')).toBe('var(--c-muted)')
    expect(nodeKindColor('finish')).toBe('var(--c-muted)')
    expect(isTerminalNodeKind('finish')).toBe(true)
    expect(isTerminalNodeKind('agent_task')).toBe(false)
  })

  it('uses the profile color for agent tasks', () => {
    expect(nodeKindColor('agent_task', 'developer')).toBe(profileColor('developer'))
    expect(nodeKindColor('agent_task')).toBe('var(--c-muted)')
  })
})

describe('statusColor', () => {
  it('maps statuses onto the semantic colors', () => {
    expect(statusColor('running')).toBe('var(--c-blue)')
    expect(statusColor('succeeded')).toBe('var(--c-green)')
    expect(statusColor('failed')).toBe('var(--c-red)')
    expect(statusColor('aborted')).toBe('var(--c-red)')
    expect(statusColor('paused')).toBe('var(--c-amber)')
    expect(statusColor('interrupted')).toBe('var(--c-amber)')
    expect(statusColor('skipped')).toBe('var(--c-faint)')
    expect(statusColor('pending')).toBe('var(--c-muted)')
    expect(statusColor(undefined)).toBe('var(--c-muted)')
  })
})
