import type { AgentUsage, RunUsage } from './api.gen'

// Token counts read at a glance: 950, 12.3k, 1.2M.
export function formatTokens(n: number): string {
  if (n < 1000) return String(n)
  // Below 999,950 the one-decimal thousands never round up to 1000k.
  if (n < 999_950) return `${trimZero((n / 1000).toFixed(1))}k`
  return `${trimZero((n / 1_000_000).toFixed(1))}M`
}

function trimZero(s: string): string {
  return s.endsWith('.0') ? s.slice(0, -2) : s
}

// A reported cost in dollars: cents precision above a dollar, four decimals
// below, so a cheap attempt does not read as $0.00.
export function formatCost(usd: number): string {
  return usd >= 1 ? `$${usd.toFixed(2)}` : `$${usd.toFixed(4)}`
}

// The token part shared by a run total and one attempt.
function tokenParts(u: Pick<AgentUsage, 'input_tokens' | 'output_tokens' | 'cache_read_tokens' | 'cache_write_tokens'>): string[] {
  const parts = [`${formatTokens(u.input_tokens)} in`, `${formatTokens(u.output_tokens)} out`]
  if (u.cache_read_tokens > 0) parts.push(`${formatTokens(u.cache_read_tokens)} cache read`)
  if (u.cache_write_tokens > 0) parts.push(`${formatTokens(u.cache_write_tokens)} cache write`)
  return parts
}

// The run page's usage line: totals over the attempts that reported usage,
// the reported cost when any attempt printed one, and which part is partial.
export function runUsageSummary(u: RunUsage): string {
  const parts = tokenParts(u)
  parts.push(`${u.attempts} ${u.attempts === 1 ? 'attempt' : 'attempts'}`)
  if (u.cost_usd !== undefined) {
    parts.push(
      u.cost_attempts < u.attempts
        ? `${formatCost(u.cost_usd)} reported by ${u.cost_attempts} of ${u.attempts}`
        : `${formatCost(u.cost_usd)} reported`,
    )
  }
  if (u.estimated) parts.push('partly estimated by apb')
  return parts.join(' · ')
}

// The event journal note of one attempt's usage, or undefined when the
// event carries none (or not in the expected shape).
export function attemptUsageNote(usage: unknown): string | undefined {
  if (!usage || typeof usage !== 'object') return undefined
  const u = usage as Partial<AgentUsage>
  if (typeof u.input_tokens !== 'number' || typeof u.output_tokens !== 'number') return undefined
  const parts = tokenParts({
    input_tokens: u.input_tokens,
    output_tokens: u.output_tokens,
    cache_read_tokens: u.cache_read_tokens ?? 0,
    cache_write_tokens: u.cache_write_tokens ?? 0,
  })
  if (typeof u.cost_usd === 'number') parts.push(formatCost(u.cost_usd))
  if (u.source === 'estimated') parts.push('estimated')
  return `tokens: ${parts.join(', ')}`
}

// The note for events a newer apb wrote that this dashboard's binary skipped;
// undefined when it read the whole journal.
export function unknownEventsNote(count: number | undefined): string | undefined {
  if (!count) return undefined
  return `${count} unknown ${count === 1 ? 'event' : 'events'} (newer apb?)`
}
