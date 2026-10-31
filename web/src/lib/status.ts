// The seven run statuses are created/running/paused/succeeded/failed/aborted/
// interrupted. Only succeeded/failed/aborted are terminal (finished, cannot
// progress further): interrupted is a resumable crash-recovery state and
// paused is resumable, so neither counts. This is the single source of truth
// for run terminality; do not hand-roll another substring classifier.
export function isTerminalRunStatus(status: string): boolean {
  const s = (status ?? '').toLowerCase()
  return s === 'succeeded' || s === 'failed' || s === 'aborted'
}

// A run whose drive claim points at a process that is gone. The status text
// still says what the journal says (usually `running`), because the only thing
// that writes a terminal event is the drive loop that no longer exists, so the
// marker is shown NEXT TO the status badge rather than replacing it. A terminal
// run has nothing left to drive, so a stale claim there is not worth a marker.
export function showsDriverDead(status: string, driverDead?: boolean): boolean {
  return !!driverDead && !isTerminalRunStatus(status)
}

// Tailwind classes for a run-status Badge, shared by the run list and run view
// so the two never drift. Outlined tags in the semantic colors (running blue,
// success green, failure red, paused amber); the status text stays visible, so
// color is never the only signal. Empty string means "use the Badge's own
// variant".
export function runStatusClass(status: string): string {
  const s = (status ?? '').toLowerCase()
  if (s.includes('succeed')) return 'border-current bg-transparent text-success'
  if (s.includes('fail') || s.includes('timed') || s.includes('abort'))
    return 'border-current bg-transparent text-destructive'
  if (s.includes('run')) return 'border-current bg-transparent text-info'
  if (s.includes('pause') || s.includes('interrupt') || s.includes('wait'))
    return 'border-current bg-transparent text-warning'
  return ''
}
