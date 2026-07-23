import type { PendingSupervisor } from './types'

// A pending supervisor decision surfaced to the run sidebar: the failed node,
// wake trigger, and available options. Narrower than the server's
// `PendingSupervisor` (which also carries how_to_decide) since the panel only
// displays node / trigger / options / instruction.
export interface SupervisorEntry {
  node: string
  trigger: string
  options: string[]
  instruction: string
}

// Fallback for the run payload's `progress.pending_supervisor` (issue #45
// finding 4 / ST9): present while waiting_kind === 'supervisor'. Mirrors
// pendingQuestionsFromPayload so the sidebar can show the park without
// reconstructing it from the event log.
export function pendingSupervisorFromPayload(
  pending: PendingSupervisor | null | undefined,
): SupervisorEntry | null {
  if (!pending) return null
  const options = Array.isArray(pending.options) ? pending.options : []
  return {
    node: pending.node,
    trigger: pending.trigger,
    options,
    instruction: typeof pending.instruction === 'string' ? pending.instruction : '',
  }
}
