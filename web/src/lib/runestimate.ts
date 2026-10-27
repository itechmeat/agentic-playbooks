// The run estimate and the budget of a start: pure formatting of
// `GET /api/playbooks/{id}/estimate` for the Run dialog, and the parsing of
// the dialog's budget inputs into the start's `budget`. The texts follow
// `apb run --estimate`.

import type { BudgetLimit, OnNoEstimate, RunEstimate, Spread } from './api.gen'
import type { RunBudget } from './api'

// One amount in `unit`: one decimal below ten, whole above.
function inUnit(x: number): number {
  return x < 10 ? Number(x.toFixed(1)) : Math.round(x)
}

// `182k`, `1.3k`, `1.5m`, `950`: one decimal below ten of a unit, so a
// limit of 1300 never reads as 1k. The unit is picked after rounding, so
// 999600 reads as 1m, not 1000k.
export function tokensText(t: number): string {
  if (Math.round(t) < 1000) return String(Math.round(t))
  if (inUnit(t / 1000) < 1000) return `${inUnit(t / 1000)}k`
  return `${inUnit(t / 1_000_000)}m`
}

// `14 min`, `40 s`; 59.6 s reads as 1 min.
export function minutesText(ms: number): string {
  const s = Math.round(ms / 1000)
  return s < 60 ? `${s} s` : `${Math.round(ms / 60_000)} min`
}

// `$1.25`.
export function usdText(x: number): string {
  return `$${x.toFixed(2)}`
}

// `~182k (121k-260k)`: the median and the range (p25-p75 from four samples,
// the smallest and largest from two or three), or the median alone.
export function spreadText(s: Spread<number>, fmt: (x: number) => string): string {
  const head = `~${fmt(s.p50)}`
  return s.kind === 'single' ? head : `${head} (${fmt(s.low)}-${fmt(s.high)})`
}

export interface EstimateRow {
  label: 'time' | 'tokens' | 'cost'
  value: string
  known: boolean
}

// The three figures of an estimate, each with an honest unknown.
function timeValue(e: RunEstimate): string {
  const time = e.time.spread
  if (!time) return 'unknown: no succeeded comparable run and no declared duration'
  if (e.time.basis === 'declared') return `~${minutesText(time.p50)} (declared expected durations)`
  const waits = e.time.human_wait_p50_ms ? `, human waits ~${minutesText(e.time.human_wait_p50_ms)}` : ''
  return spreadText(time, minutesText) + waits
}

export function estimateRows(e: RunEstimate): EstimateRow[] {
  const time = e.time.spread
  return [
    { label: 'time', value: timeValue(e), known: !!time },
    {
      label: 'tokens',
      value: e.tokens ? spreadText(e.tokens.spread, tokensText) : 'unknown: no comparable attempt reported usage',
      known: !!e.tokens,
    },
    {
      label: 'cost',
      value: e.cost_usd ? spreadText(e.cost_usd.spread, usdText) : 'unknown: no comparable attempt reported a cost',
      known: !!e.cost_usd,
    },
  ]
}

// `7 comparable runs (cli); 3 attempts without usage`.
export function comparableText(e: RunEstimate): string {
  const c = e.comparable
  let line = `${c.runs} comparable ${c.runs === 1 ? 'run' : 'runs'} (${e.execution})`
  if (c.attempts_without_usage > 0) {
    line += `; ${c.attempts_without_usage} ${c.attempts_without_usage === 1 ? 'attempt' : 'attempts'} without usage`
  }
  return line
}

// `triage (no history with this profile)`, one per gated node the gate
// cannot weigh under `limit` (see `unestimatedFor`); a node with spend
// history that lacks only its cost or token figure says which. Without a
// limit, the nodes with no spend sample at all.
export function unestimatedText(e: RunEstimate, limit: BudgetLimit = {}): string[] {
  const limited = limit.tokens !== undefined || limit.usd !== undefined
  const names = limited ? unestimatedFor(e, limit) : e.unestimated
  return names.map((n) => {
    if (e.unestimated.includes(n)) {
      return e.comparable.profile_mismatch?.[n] ? `${n} (no history with this profile)` : `${n} (no history)`
    }
    return e.unestimated_cost?.includes(n) && limit.usd !== undefined ? `${n} (no cost figure)` : `${n} (no token figure)`
  })
}

// The gated nodes the gate cannot weigh in a dimension `limit` limits (a
// node with token history only, under a dollar limit, included): before
// each of them `on_no_estimate` decides. Mirrors the engine's
// `RunEstimate::unestimated_for`.
export function unestimatedFor(e: RunEstimate, limit: BudgetLimit): string[] {
  const missing = (list: string[] | undefined, limited: boolean, n: string) => limited && !!list?.includes(n)
  return e.nodes
    .map((n) => n.node)
    .filter(
      (n) =>
        missing(e.unestimated_tokens, limit.tokens !== undefined, n) ||
        missing(e.unestimated_cost, limit.usd !== undefined, n),
    )
}

// The pause or continue choice is the person's when the policy is `ask`,
// the budget limits a dimension, and some gated node has no estimate in it;
// also when the estimate could not be read (`e` null after a failure), so
// a start is never refused for a decision the person was not shown.
export function needsPolicyChoice(
  e: RunEstimate | null,
  policy: OnNoEstimate,
  limit: BudgetLimit,
  loadFailed = false,
): boolean {
  if (policy !== 'ask' || (limit.tokens === undefined && limit.usd === undefined)) return false
  if (!e) return loadFailed
  return unestimatedFor(e, limit).length > 0
}

const TOKEN_UNITS: Record<string, number> = { '': 1, k: 1e3, m: 1e6 }

// A token amount as typed: `300000`, `300k`, `1.5m` (`_` separators
// ignored); null for anything else, a comma included (a decimal comma would
// read ten times larger), or for zero. The engine's `parse_tokens` rule.
export function parseTokens(s: string): number | null {
  const t = s.trim().toLowerCase().replace(/_/g, '')
  const m = /^(\d+(?:\.\d+)?)([km]?)$/.exec(t)
  if (!m) return null
  const n = Math.round(Number(m[1]) * TOKEN_UNITS[m[2]])
  return n >= 1 ? n : null
}

// A dollar amount as typed: `3`, `3.50`, `$3.50`; null for anything else or
// for zero.
export function parseUsd(s: string): number | null {
  const t = s.trim().replace(/^\$/, '').trim()
  if (!/^\d+(?:\.\d+)?$/.test(t)) return null
  const n = Number(t)
  return n > 0 && Number.isFinite(n) ? n : null
}

export interface BudgetForm {
  tokens: string
  usd: string
  policy: OnNoEstimate
}

// The limit a start will have: the dialog's inputs over the playbook's
// block (an empty input keeps the block's limit, as the server merges it).
// An input that does not parse counts as set.
export function effectiveLimit(f: BudgetForm, block: RunBudget | null | undefined): BudgetLimit {
  const pick = (typed: string, parse: (s: string) => number | null, fallback?: number) =>
    typed.trim() ? (parse(typed) ?? 0) : fallback
  return {
    tokens: pick(f.tokens, parseTokens, block?.tokens),
    usd: pick(f.usd, parseUsd, block?.usd),
  }
}

// The start's `budget` from the dialog's inputs: empty inputs are left out
// (the playbook's block applies); `error` names an input that does not
// parse. The policy is sent whenever the start will have a limit (typed or
// the block's) and the person chose another one than the block's.
export function budgetFromForm(
  f: BudgetForm,
  defaults: RunBudget | null | undefined,
): { budget?: RunBudget; error?: string } {
  const budget: RunBudget = {}
  if (f.tokens.trim()) {
    const t = parseTokens(f.tokens)
    if (t === null) return { error: 'tokens: expected an amount such as 300000, 300k or 1.5m' }
    if (t !== defaults?.tokens) budget.tokens = t
  }
  if (f.usd.trim()) {
    const u = parseUsd(f.usd)
    if (u === null) return { error: 'dollars: expected an amount such as 3 or 3.50' }
    if (u !== defaults?.usd) budget.usd = u
  }
  const limit = effectiveLimit(f, defaults)
  const limited = limit.tokens !== undefined || limit.usd !== undefined
  if (limited && f.policy !== (defaults?.on_no_estimate ?? 'ask')) budget.on_no_estimate = f.policy
  return Object.keys(budget).length ? { budget } : {}
}

// The dialog's inputs prefilled from the playbook's `budget:` block.
export function formFromBlock(block: RunBudget | null | undefined): BudgetForm {
  return {
    tokens: block?.tokens ? String(block.tokens) : '',
    usd: block?.usd ? String(block.usd) : '',
    policy: block?.on_no_estimate ?? 'ask',
  }
}
