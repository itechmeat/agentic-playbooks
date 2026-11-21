import { describe, expect, it } from 'vitest'
import { effectLabels, installTarget } from './templates'

describe('installTarget', () => {
  it('links a global install to the global route', () => {
    expect(installTarget('global', '', 'inbox-triage')).toBe('#/global-playbook/inbox-triage')
  })

  it('links a project install to its playbook page', () => {
    expect(installTarget('project', 'ws-1', 'inbox-triage')).toBe('#/playbook/ws-1/inbox-triage')
  })

  it('encodes its parts', () => {
    expect(installTarget('project', 'ws a/b', 'my id')).toBe(
      `#/playbook/${encodeURIComponent('ws a/b')}/${encodeURIComponent('my id')}`,
    )
  })
})

describe('effectLabels', () => {
  it('puts irreversible first', () => {
    expect(effectLabels(['fs_write', 'irreversible', 'network'])).toEqual([
      'irreversible',
      'fs_write',
      'network',
    ])
    expect(effectLabels(['network'])).toEqual(['network'])
    expect(effectLabels([])).toEqual([])
  })
})
