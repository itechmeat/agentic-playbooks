import type { AgentUsage, CostBasis, CostByBasis, RunUsage } from './api.gen'
import { COST_BASIS_LABELS } from './consts.gen'

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

// A cost in plain words: the figure and what it is (billed, a subscription
// equivalent nobody is billed, or of unknown basis). A usage block without a
// basis (an older apb) is of unknown basis.
export function costWithBasis(usd: number, basis: CostBasis | undefined): string {
  return `${formatCost(usd)} ${COST_BASIS_LABELS[basis ?? 'unknown']}`
}

// A run's cost split by basis, billed money first:
// `$0.3000 billed; $0.1312 subscription equivalent, not billed`.
export function costByBasisText(c: CostByBasis): string {
  const parts: [CostBasis, number | undefined][] = [
    ['billed', c.billed_usd],
    ['subscription_equivalent', c.subscription_equivalent_usd],
    ['unknown', c.unknown_basis_usd],
  ]
  return parts
    .filter((p): p is [CostBasis, number] => p[1] !== undefined)
    .map(([b, v]) => costWithBasis(v, b))
    .join('; ')
}

// The cost of a run total in words; a total from a server that split
// nothing is of unknown basis.
function runCostText(u: RunUsage): string | undefined {
  if (u.cost_usd === undefined) return undefined
  const split = u.cost_by_basis ? costByBasisText(u.cost_by_basis) : ''
  return split || costWithBasis(u.cost_usd, undefined)
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
  // The totals cover only the attempts that reported usage: name every
  // attempt of the run and how many of them that is, never the reporting
  // ones alone as if they were all.
  const all = Math.max(u.finished_attempts ?? u.attempts, u.attempts)
  const noun = `${all} ${all === 1 ? 'attempt' : 'attempts'}`
  parts.push(all > u.attempts ? `${noun}, ${u.attempts} with usage` : noun)
  const cost = runCostText(u)
  if (cost !== undefined) {
    parts.push(u.cost_attempts < all ? `${cost} (reported by ${u.cost_attempts} of ${all})` : cost)
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
  if (typeof u.cost_usd === 'number') parts.push(costWithBasis(u.cost_usd, knownBasis(u.cost_basis)))
  if (u.source === 'estimated') parts.push('estimated')
  return `tokens: ${parts.join(', ')}`
}

// A basis this dashboard knows, else none (a newer apb's value reads as
// unknown, as the server reads it).
function knownBasis(b: unknown): CostBasis | undefined {
  return typeof b === 'string' && b in COST_BASIS_LABELS ? (b as CostBasis) : undefined
}

// The run page's cost cell: the cost split by basis, marked approximate
// when only some attempts reported one or apb estimated part of it; null
// without a reported cost.
export function runCostNote(u: RunUsage): { text: string; title: string } | null {
  const cost = runCostText(u)
  if (cost === undefined) return null
  const all = Math.max(u.finished_attempts ?? u.attempts, u.attempts)
  const partial = u.cost_attempts < all
  const why = [partial ? `reported by ${u.cost_attempts} of ${all} attempts` : 'reported by every attempt', u.estimated ? 'partly estimated by apb' : '']
    .filter(Boolean)
    .join('; ')
  return { text: `${partial || u.estimated ? '~' : ''}${cost}${partial ? ' partial' : ''}`, title: why }
}

// The note for events a newer apb wrote that this dashboard's binary skipped;
// undefined when it read the whole journal.
export function unknownEventsNote(count: number | undefined): string | undefined {
  if (!count) return undefined
  return `${count} unknown ${count === 1 ? 'event' : 'events'} (newer apb?)`
}
