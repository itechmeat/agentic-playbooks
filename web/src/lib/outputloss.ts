// What an agent attempt lost of its output (bounded agent output), as the
// detail line of its `attempt_finished` journal row: each cut stream with the
// bytes kept and the bytes dropped at least, and whether the agent's final
// result could not be read whole.

export interface OutputLoss {
  stream: string
  kept_bytes: number
  dropped_bytes_at_least: number
}

function isLoss(v: unknown): v is OutputLoss {
  if (!v || typeof v !== 'object') return false
  const r = v as Record<string, unknown>
  return typeof r.stream === 'string' && typeof r.kept_bytes === 'number' && typeof r.dropped_bytes_at_least === 'number'
}

// `stdout kept 5120 bytes, at least 14980 dropped; final result not read
// whole`, or undefined for an attempt that lost nothing.
export function attemptOutputNote(truncated: unknown, incomplete: unknown): string | undefined {
  const parts = (Array.isArray(truncated) ? truncated.filter(isLoss) : []).map(
    (l) => `${l.stream} kept ${l.kept_bytes} bytes, at least ${l.dropped_bytes_at_least} dropped`,
  )
  if (incomplete === true) parts.push('final result not read whole')
  return parts.length ? `output cut: ${parts.join('; ')}` : undefined
}
