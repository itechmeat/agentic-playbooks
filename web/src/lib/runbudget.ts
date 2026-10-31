// A run's budget on the run page: pure formatting of `RunDetail.budget`
// (limits, spent and left per dimension, unknown attempts, the node a budget
// pause holds the run before) and the resume form of a budget pause. The
// texts follow `apb runs <id>`.

import type { BudgetAmount, BudgetStatus, DimStatus, OnNoEstimate } from './api.gen'
import type { ResumeBudget } from './api'
import { parseTokens, parseUsd, tokensText, usdText } from './runestimate'

export interface BudgetRow {
  dim: 'tokens' | 'usd'
  spent: string
  limit: string
  left: string
  // `1 attempt without usage (~40k)`, or null when every attempt reported.
  unknown: string | null
  // Nothing (or less than nothing) left.
  over: boolean
}

function row(
  dim: 'tokens' | 'usd',
  limit: number,
  d: DimStatus,
  fmt: (x: number) => string,
): BudgetRow {
  return {
    dim,
    spent: fmt(d.spent),
    limit: fmt(limit),
    left: d.remaining === undefined ? 'unknown' : fmt(Math.max(0, d.remaining)),
    unknown: unknownText(d, fmt),
    over: d.remaining !== undefined && d.remaining <= 0,
  }
}

// `1 attempt without usage (~40k)`, or null when every attempt reported.
function unknownText(d: DimStatus, fmt: (x: number) => string): string | null {
  const n = d.unknown_attempts
  if (n === 0) return null
  const estimate = d.imputed === undefined ? 'no estimate' : `~${fmt(d.imputed)}`
  return `${n} ${n === 1 ? 'attempt' : 'attempts'} without usage (${estimate})`
}

// One row per limited dimension.
export function budgetRows(b: BudgetStatus): BudgetRow[] {
  const rows: BudgetRow[] = []
  if (b.limit.tokens !== undefined && b.tokens) rows.push(row('tokens', b.limit.tokens, b.tokens, tokensText))
  if (b.limit.usd !== undefined && b.usd) rows.push(row('usd', b.limit.usd, b.usd, usdText))
  return rows
}

// `30k tokens, $1.25`, or `unknown`.
export function amountText(a: BudgetAmount | undefined): string {
  const parts = [
    a?.tokens !== undefined ? `${tokensText(a.tokens)} tokens` : null,
    a?.usd !== undefined ? usdText(a.usd) : null,
  ].filter((p): p is string => p !== null)
  return parts.length ? parts.join(', ') : 'unknown'
}

const BASIS: Record<string, string> = {
  median: 'its median spend no longer fits',
  exhausted: 'the run spent its limit',
  no_estimate: 'it has no spend estimate',
  unknown_spend: 'what the run spent is unknown',
  operator: 'a person decided',
}

// `paused before review: its median spend no longer fits (needs ~30k tokens)`,
// or null while no budget pause holds the run.
export function pausedText(b: BudgetStatus): string | null {
  const p = b.paused
  if (!p) return null
  const where = p.batch !== undefined ? `${p.node} (batch ${p.batch})` : p.node
  const why = BASIS[p.basis] ?? p.basis
  return `paused before ${where}: ${why}${p.need ? ` (needs ~${amountText(p.need)})` : ''}`
}

export interface BudgetResumeForm {
  raiseTokens: string
  raiseUsd: string
  // A new limit instead of a raise.
  setTokens: string
  setUsd: string
  // `''` keeps the policy in force.
  policy: '' | OnNoEstimate
  once: boolean
}

export function emptyResumeForm(): BudgetResumeForm {
  return { raiseTokens: '', raiseUsd: '', setTokens: '', setUsd: '', policy: '', once: false }
}

const TOKENS_ERROR = 'expected an amount such as 50000 or 50k'
const USD_ERROR = 'expected an amount such as 2 or 2.50'

// The resume's `budget` from the form, or an error naming the input that
// does not parse, a dimension both raised and set, or that nothing was
// asked for.
export function resumeBudgetBody(f: BudgetResumeForm): { budget?: ResumeBudget; error?: string } {
  const budget: ResumeBudget = {}
  const fields: [keyof ResumeBudget, string, (s: string) => number | null, string][] = [
    ['raise_tokens', f.raiseTokens, parseTokens, `token raise: ${TOKENS_ERROR}`],
    ['raise_usd', f.raiseUsd, parseUsd, `dollar raise: ${USD_ERROR}`],
    ['tokens', f.setTokens, parseTokens, `token limit: ${TOKENS_ERROR}`],
    ['usd', f.setUsd, parseUsd, `dollar limit: ${USD_ERROR}`],
  ]
  for (const [key, typed, parse, error] of fields) {
    if (!typed.trim()) continue
    const n = parse(typed)
    if (n === null) return { error }
    ;(budget as Record<string, number>)[key] = n
  }
  if (
    (budget.tokens !== undefined && budget.raise_tokens !== undefined) ||
    (budget.usd !== undefined && budget.raise_usd !== undefined)
  ) {
    return { error: 'set a new limit or raise it, not both, for the same dimension' }
  }
  if (f.policy) budget.on_no_estimate = f.policy
  if (f.once) budget.once = true
  if (!Object.keys(budget).length) {
    return { error: 'raise or set a limit, choose what happens before a node with no estimate, or let the paused node run once' }
  }
  return { budget }
}
