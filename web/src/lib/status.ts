// The seven run statuses are created/running/paused/succeeded/failed/aborted/
// interrupted. Only succeeded/failed/aborted are terminal (finished, cannot
// progress further): interrupted is a resumable crash-recovery state and
// paused is resumable, so neither counts. This is the single source of truth
// for run terminality; do not hand-roll another substring classifier.
export function isTerminalRunStatus(status: string): boolean {
  const s = (status ?? '').toLowerCase()
  return s === 'succeeded' || s === 'failed' || s === 'aborted'
}

// Tailwind classes for a run-status Badge, shared by the run list and run view
// so the two never drift. Empty string means "use the Badge's own variant".
export function runStatusClass(status: string): string {
  const s = (status ?? '').toLowerCase()
  if (s.includes('succeed')) return 'border-transparent bg-success text-success-foreground'
  if (s.includes('fail') || s.includes('timed') || s.includes('abort'))
    return 'border-transparent bg-destructive text-white'
  if (s.includes('run')) return 'border-transparent bg-chart-1 text-white'
  return ''
}
