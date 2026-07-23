import { isTerminalRunStatus } from './status'
import type { ProgressSummary } from './types'

// Displayed percent is monotonic within one run and one work plan: it never
// decreases on refetch. There are exactly two honest resets: the run identity
// changing (navigating from one run to another) and the plan identity
// (plan_key) changing within the same run (a report raising the cycle total,
// or a supervisor patch). Display copy such as `label` is not an input here;
// it can change on every ordinary report without affecting the baseline.
export interface ProgressDisplayState {
  runKey: string
  planKey: string
  shown: number
}

// Badge copy for progress.waiting_kind. Distinct per kind so the run list and
// run view never collapse supervisor park into the generic "waiting" label.
export function waitingKindText(kind: ProgressSummary['waiting_kind']): string {
  switch (kind) {
    case 'human_review':
      return 'waiting for decision'
    case 'wait':
      return 'waiting for event'
    case 'question':
      return 'waiting for answer'
    case 'supervisor':
      return 'waiting for supervisor'
    default:
      return 'waiting'
  }
}

function clamp(percent: number): number {
  return Math.min(100, Math.max(0, percent))
}

export function nextDisplay(
  prev: ProgressDisplayState | null,
  runKey: string,
  next: ProgressSummary,
): ProgressDisplayState {
  const percent = clamp(next.percent)
  if (!prev || prev.runKey !== runKey) {
    return { runKey, planKey: next.plan_key, shown: percent }
  }
  if (prev.planKey !== next.plan_key) {
    return { runKey, planKey: next.plan_key, shown: percent }
  }
  return { runKey, planKey: next.plan_key, shown: clamp(Math.max(prev.shown, percent)) }
}

// A progress bar is shown only while a run is not terminal. Finished runs show
// status only. Terminality is centralized in status.ts so this never drifts
// from runStatusClass's own notion of a finished run.
export function showBar(status: string): boolean {
  return !isTerminalRunStatus(status)
}
