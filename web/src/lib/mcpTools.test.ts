import { describe, it, expect } from 'vitest'
import {
  LISTING_REFUSED,
  MCP_TOOL_STATUSES,
  listButtonLabel,
  listingFailure,
  needsApproval,
  statusLabel,
} from './mcpTools'
import { ApiError } from './api/http'
import type { McpToolListing, McpToolRow, McpToolStatus } from './api/connectors'

const row = (status: McpToolStatus): McpToolRow => ({
  function: 'whoami',
  tool: 'whoami',
  status,
  title: null,
  description: null,
  pinnedDescription: null,
  members: [],
  annotations: null,
  inputSchema: null,
})

const listing = (rows: McpToolRow[]): McpToolListing => ({
  connector: 'tracker-mcp',
  account: 'work',
  era: 'modern 2026-07-28',
  cleanup: null,
  server: { command: 'tracker', program: '/usr/bin/tracker', launcher: null, status: 'same' },
  rows,
  unmapped: [],
})

describe('mcpTools', () => {
  it('statusLabel names every status', () => {
    expect(statusLabel('pinned')).toBe('approved')
    expect(statusLabel('unpinned')).toBe('not approved')
    expect(statusLabel('changed')).toBe('changed since approval')
    expect(statusLabel('missing')).toBe('gone from the server')
    for (const s of MCP_TOOL_STATUSES) {
      expect(statusLabel(s)).not.toBe(s)
    }
  })

  it('needsApproval is true for every status but pinned', () => {
    for (const s of MCP_TOOL_STATUSES) {
      expect(needsApproval(listing([row('pinned'), row(s)]))).toBe(s !== 'pinned')
    }
    expect(needsApproval(listing([]))).toBe(false)
  })
})

describe('the listing button and its failures', () => {
  it('names the account and its project', () => {
    expect(listButtonLabel('work', 'alpha')).toBe('List tools of work in alpha')
    expect(listButtonLabel('work', '')).toBe('List tools of work')
  })

  it('reads a 403 as the refused state, anything else as its message', () => {
    expect(listingFailure(new ApiError('mcp_needs_a_person: x', 403))).toEqual({ refused: true })
    expect(listingFailure(new ApiError('boom', 500))).toEqual({ refused: false, message: 'boom' })
    expect(listingFailure('plain')).toEqual({ refused: false, message: 'plain' })
  })

  it('the refused words name both ways to list', () => {
    expect(LISTING_REFUSED).toContain('Sign in to the dashboard')
    expect(LISTING_REFUSED).toContain('apb connector tools')
  })
})
