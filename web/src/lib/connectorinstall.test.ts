import { describe, expect, it } from 'vitest'
import {
  addButtonState,
  connectorActionMessage,
  connectorListState,
  needsForce,
  DISCONNECT_KEEPS_CONFIG,
} from './connectorinstall'

describe('connectorListState', () => {
  it('installed wins over everything else', () => {
    expect(
      connectorListState({ installedCount: 2, availableCount: 0, availableFailed: false }),
    ).toBe('installed')
    expect(
      connectorListState({ installedCount: 1, availableCount: 3, availableFailed: true }),
    ).toBe('installed')
  })

  it('nothing installed but something available invites the first connection', () => {
    expect(
      connectorListState({ installedCount: 0, availableCount: 1, availableFailed: false }),
    ).toBe('first-connect')
  })

  it('nothing installed and nothing available is its own state', () => {
    expect(
      connectorListState({ installedCount: 0, availableCount: 0, availableFailed: false }),
    ).toBe('nothing-available')
  })

  it('a failed /available fetch is not the same as nothing available', () => {
    expect(
      connectorListState({ installedCount: 0, availableCount: 0, availableFailed: true }),
    ).toBe('available-failed')
  })
})

describe('addButtonState', () => {
  it('enabled with no note when there is something to add', () => {
    expect(addButtonState({ availableCount: 2, availableFailed: false })).toEqual({
      disabled: false,
      note: null,
    })
  })

  it('disabled with an explanation when nothing is left to add', () => {
    const s = addButtonState({ availableCount: 0, availableFailed: false })
    expect(s.disabled).toBe(true)
    expect(s.note).toBe('No more connectors are available to connect.')
  })

  it('stays enabled when the list failed to load, so the user can retry', () => {
    const s = addButtonState({ availableCount: 0, availableFailed: true })
    expect(s.disabled).toBe(false)
    expect(s.note).toBe('The list of available connectors could not be loaded.')
  })
})

describe('connectorActionMessage', () => {
  it('needs_force says a different version is installed', () => {
    expect(connectorActionMessage('needs_force', 'connect')).toBe(
      'A different version of this connector is already installed. Replace it to connect this version.',
    )
  })

  it('maps every documented connect code to its own sentence', () => {
    const codes = ['invalid_name', 'not_found', 'needs_force', 'no_config_dir', 'io_error']
    const messages = codes.map((c) => connectorActionMessage(c, 'connect'))
    expect(new Set(messages).size).toBe(codes.length)
    for (const m of messages) expect(m.length).toBeGreaterThan(0)
  })

  it('disconnect wording differs from connect wording where it matters', () => {
    expect(connectorActionMessage('io_error', 'disconnect')).toBe(
      'The server could not remove the connector files.',
    )
    expect(connectorActionMessage('no_config_dir', 'disconnect')).not.toBe(
      connectorActionMessage('no_config_dir', 'connect'),
    )
  })

  it('an unknown code falls back to the server detail', () => {
    expect(connectorActionMessage('teapot', 'connect', 'server said no')).toBe('server said no')
  })

  it('an unknown code with no detail still says which action failed', () => {
    expect(connectorActionMessage(undefined, 'connect')).toBe('Could not connect the connector.')
    expect(connectorActionMessage(null, 'disconnect')).toBe(
      'Could not disconnect the connector.',
    )
  })
})

describe('needsForce', () => {
  it('only needs_force offers the replace action', () => {
    expect(needsForce('needs_force')).toBe(true)
    expect(needsForce('io_error')).toBe(false)
    expect(needsForce(undefined)).toBe(false)
  })
})

describe('DISCONNECT_KEEPS_CONFIG', () => {
  it('promises the account configuration survives', () => {
    expect(DISCONNECT_KEEPS_CONFIG).toContain('kept')
    expect(DISCONNECT_KEEPS_CONFIG).not.toMatch(/delete|lost|erase/i)
  })
})
