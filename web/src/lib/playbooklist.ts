// Pure helpers behind the playbook list (terminal look): the per-row facts
// read from data the dashboard already has (the run listing, the playbook
// detail, `GET /api/stats`). Nothing here invents a value: a fact the data
// does not carry comes back as null and the row leaves the cell empty.

import type { StatsReport } from './api.gen'
import type { RunSummary } from './types'
import { isTerminalRunStatus } from './status'


/** Key of a playbook across projects, the same one the list uses. */
export const playbookKey = (workspaceId: string, id: string) => `${workspaceId}/${id}`

/** Runs grouped by playbook key, newest first. Child runs count too: each is a run of its playbook. */
export function runsByPlaybook(runs: RunSummary[]): Map<string, RunSummary[]> {
  const m = new Map<string, RunSummary[]>()
  for (const r of runs) {
    const k = playbookKey(r.workspace_id, r.playbook)
    const list = m.get(k)
    if (list) list.push(r)
    else m.set(k, [r])
  }
  for (const list of m.values()) list.sort((a, b) => b.started_ts - a.started_ts)
  return m
}

/**
 * The last `n` runs, oldest first so the newest sits at the right end, padded
 * on the left with nulls (empty slots) when there were fewer runs.
 */
export function runHistory(runs: RunSummary[] | undefined, n = 12): (RunSummary | null)[] {
  const last = (runs ?? []).slice(0, n).reverse()
  return [...Array<null>(Math.max(0, n - last.length)).fill(null), ...last]
}

/** Succeeded share of the finished runs, or null when none finished. */
export function successRate(
  runs: RunSummary[] | undefined,
): { ok: number; of: number; pct: number } | null {
  const done = (runs ?? []).filter((r) => isTerminalRunStatus(r.status))
  if (done.length === 0) return null
  const ok = done.filter((r) => r.status === 'succeeded').length
  return { ok, of: done.length, pct: Math.round((ok / done.length) * 100) }
}

/** How many runs finished: a change in it is when the stats are worth reloading. */
export function finishedCount(runs: RunSummary[] | undefined): number {
  return (runs ?? []).filter((r) => isTerminalRunStatus(r.status)).length
}

/** One-letter glyph of a playbook: the first letter or digit of its name, else of its id. */
export function glyphLetter(name: string, id: string): string {
  const m = /[\p{L}\p{N}]/u.exec(name) ?? /[\p{L}\p{N}]/u.exec(id)
  return m ? m[0].toUpperCase() : '?'
}

/** Whether a run started on the same local calendar day as `now`. */
export function startedToday(r: RunSummary, now = Date.now()): boolean {
  return new Date(r.started_ts).toDateString() === new Date(now).toDateString()
}

/**
 * Median run time of a playbook from a stats report: the current version's,
 * else the newest version that has one; null without any. A report over a
 * whole project holds every playbook's versions, so `playbook` picks one.
 */
export function medianRunTime(
  report: Pick<StatsReport, 'versions'> | null | undefined,
  current: string,
  playbook?: string,
): { ms: number; version: string } | null {
  const versions = (report?.versions ?? []).filter((v) => playbook === undefined || v.playbook === playbook)
  const cur = versions.find((v) => v.version === current && v.duration.median_ms !== undefined)
  const pick = cur ?? [...versions].reverse().find((v) => v.duration.median_ms !== undefined)
  return pick ? { ms: pick.duration.median_ms as number, version: pick.version } : null
}
