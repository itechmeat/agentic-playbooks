// Pure helpers of the MCP tools section of a connector page: the words a
// person reads for a tool's approval status, and whether a live listing
// holds a tool the account's approval does not cover. No command is named:
// approving tools is a person's act at a terminal.

import type { McpToolListing, McpToolStatus } from './api/connectors'

/** Every status a listed tool can have, in the order the section explains them. */
export const MCP_TOOL_STATUSES: readonly McpToolStatus[] = ['pinned', 'unpinned', 'changed', 'missing']

/** The words for a listed tool's status. An unknown status reads as itself. */
export function statusLabel(status: string): string {
  switch (status) {
    case 'pinned':
      return 'approved'
    case 'unpinned':
      return 'not approved'
    case 'changed':
      return 'changed since approval'
    case 'missing':
      return 'gone from the server'
    default:
      return status
  }
}

/** True when a live listing holds a tool that is not `pinned`: the account's
 * approval no longer covers every tool its functions map. */
export function needsApproval(listing: McpToolListing): boolean {
  return listing.rows.some((r) => r.status !== 'pinned')
}

/** The fixed words of a listing the server refused for this caller (a
 * dashboard without keys, or a bearer key): only a signed-in session lists
 * an MCP server's tools. */
export const LISTING_REFUSED = 'Sign in to the dashboard to list tools, or run `apb connector tools` at a terminal.'

/** The accessible name of an account row's "List tools" button: the
 * account, and its project when the row names one. */
export function listButtonLabel(account: string, rootLabel: string): string {
  return rootLabel ? `List tools of ${account} in ${rootLabel}` : `List tools of ${account}`
}

/** What a failed listing request shows: a 403 is the refused state with
 * its fixed words, anything else its own message. */
export function listingFailure(e: unknown): { refused: true } | { refused: false; message: string } {
  if (typeof e === 'object' && e !== null && 'status' in e && (e as { status: unknown }).status === 403) {
    return { refused: true }
  }
  return { refused: false, message: e instanceof Error ? e.message : String(e) }
}
