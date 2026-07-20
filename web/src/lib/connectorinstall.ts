// Connect / disconnect of official connectors (design doc section 9). The
// server keeps two separate stores: the connector tree (installed or not) and
// the account configuration. Uninstalling only removes the former, which is
// why every disconnect string here promises the accounts survive.

// One entry of GET /api/connectors/available: an embedded official connector
// that is NOT currently installed. The endpoint already excludes installed
// ones, so the dashboard never has to subtract the two lists itself.
export interface AvailableConnector {
  name: string
  version: string
  displayName: string
  summary: string
  tags: string[]
}

export interface InstallResult {
  ok: boolean
  name: string
  version: string
  digest: string
  // The same digest was already installed, so nothing changed on disk.
  noOp: boolean
  trustRecorded: boolean
  // Set when the connector installed but trust could not be recorded. It is a
  // partial success, so it is surfaced as a warning rather than swallowed.
  trustWarning: string | null
}

export interface UninstallResult {
  ok: boolean
  name: string
  // It was not installed to begin with. There is no 404 here by design.
  noOp: boolean
}

// Which of the three list layouts ConnectorList renders.
//
// - `installed`         some connectors installed: add button on top, list below
// - `first-connect`     nothing installed but something can be connected
// - `nothing-available` nothing installed and nothing left to connect
// - `available-failed`  nothing installed and /available did not load, which is
//                       NOT the same as "nothing available" and must say so
export type ConnectorListState =
  | 'installed'
  | 'first-connect'
  | 'nothing-available'
  | 'available-failed'

export interface ListStateInput {
  installedCount: number
  availableCount: number
  // GET /api/connectors/available failed. Kept separate from a count of 0 so an
  // outage never renders as "there is nothing to connect".
  availableFailed: boolean
}

export function connectorListState(input: ListStateInput): ConnectorListState {
  if (input.installedCount > 0) return 'installed'
  if (input.availableFailed) return 'available-failed'
  if (input.availableCount > 0) return 'first-connect'
  return 'nothing-available'
}

// The add-connector button shown above an existing list. It is only disabled
// when we positively know there is nothing left to add; a failed fetch keeps it
// enabled so the picker can offer a retry.
export interface AddButtonState {
  disabled: boolean
  note: string | null
}

export function addButtonState(input: Omit<ListStateInput, 'installedCount'>): AddButtonState {
  if (input.availableFailed) {
    return { disabled: false, note: 'The list of available connectors could not be loaded.' }
  }
  if (input.availableCount === 0) {
    return { disabled: true, note: 'No more connectors are available to connect.' }
  }
  return { disabled: false, note: null }
}

export type ConnectorAction = 'connect' | 'disconnect'

// The documented failure codes of POST install / uninstall. The wire body is
// always `{"error":"<code>","detail":"<message>"}`.
const CONNECT_MESSAGES: Record<string, string> = {
  invalid_name: 'That connector name is not valid.',
  not_found: 'There is no official connector with that name to connect.',
  needs_force:
    'A different version of this connector is already installed. Replace it to connect this version.',
  no_config_dir: 'The server has no config directory to install the connector into.',
  io_error: 'The server could not write the connector files.',
}

const DISCONNECT_MESSAGES: Record<string, string> = {
  invalid_name: 'That connector name is not valid.',
  not_found: 'There is no official connector with that name.',
  needs_force: 'A different version of this connector is installed. Reload the page and try again.',
  no_config_dir: 'The server has no config directory to remove the connector from.',
  io_error: 'The server could not remove the connector files.',
}

// Maps a documented error code to a user-facing sentence. An unknown code falls
// back to the server detail, so a new server-side code still says something
// useful instead of a bare "error".
export function connectorActionMessage(
  code: string | null | undefined,
  action: ConnectorAction,
  detail?: string | null,
): string {
  const table = action === 'connect' ? CONNECT_MESSAGES : DISCONNECT_MESSAGES
  const known = code ? table[code] : undefined
  if (known) return known
  if (detail) return detail
  return action === 'connect' ? 'Could not connect the connector.' : 'Could not disconnect the connector.'
}

// A 409 means a different version is on disk, so the only way forward is an
// explicit replace. Never retried automatically.
export function needsForce(code: string | null | undefined): boolean {
  return code === 'needs_force'
}

// Shown next to the disconnect action and in the picker. Kept in one place so
// the promise never drifts between the two screens.
export const DISCONNECT_KEEPS_CONFIG =
  'Disconnecting removes the connector files only. Account configuration is stored separately and is kept, so reconnecting picks up the previous accounts automatically.'
