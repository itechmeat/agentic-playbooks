// Moving a playbook's `current` from the dashboard: the outcome of a move
// (moved, stale, refused) read from the server's answer, and the plain
// words the history list shows for each move.

import { ApiError } from './api'
import type { HistoryEntry, Moved, MovedBy } from './api.gen'
import { CURRENT_REASON_MAX } from './consts.gen'

/// The longest reason the server keeps, after its scrub.
export const REASON_MAX = CURRENT_REASON_MAX

export type MoveOutcome =
  | { kind: 'moved'; moved: Moved }
  /// `current` changed since the page read it: `actual` is what it holds now.
  | { kind: 'stale'; actual: string | null }
  | { kind: 'refused'; message: string }

/// Runs `post` (the move) and reads its answer as an outcome; never throws.
export async function moveOutcome(post: () => Promise<Moved>): Promise<MoveOutcome> {
  try {
    return { kind: 'moved', moved: await post() }
  } catch (e) {
    if (e instanceof ApiError && e.code === 'stale') {
      const actual = e.body?.actual
      return { kind: 'stale', actual: typeof actual === 'string' ? actual : null }
    }
    const message =
      e instanceof ApiError && typeof e.body?.message === 'string' ? e.body.message : String(e)
    return { kind: 'refused', message }
  }
}

/// Who moved `current`, in a few plain words (the CLI's wording).
export function whoMoved(by: MovedBy | undefined): string {
  if (!by) return 'unknown'
  switch (by.kind) {
    case 'person':
      return by.surface === 'cli' ? 'a person on the CLI' : 'a person on the dashboard'
    case 'candidate_promotion':
      return `candidate promotion (${by.basis}${by.run_id ? `, run ${by.run_id}` : ''})`
    case 'patch_promotion':
      return `patch promotion (run ${by.run_id})`
    case 'created':
      return 'a new version'
    case 'migration':
      return 'schema migration'
    case 'scope_move':
      return `copied from the ${by.from_scope} scope`
  }
}

/// When a history line was written, in UTC to the minute.
export function movedAt(ms: number): string {
  return new Date(ms).toISOString().slice(0, 16).replace('T', ' ')
}

/// The `from -> to` of a move; an aborted move names its change.
export function moveSummary(e: HistoryEntry): string {
  if (e.kind === 'aborted') return `aborted move ${e.change_id.slice(0, 8)}`
  return `${e.from ?? 'unset'} -> ${e.to ?? 'unset'}`
}
