import type { NodeCommits, RunGoal } from './api.gen'
import type { WfEvent } from './types'

// What a run produced beyond its node outputs (0.23.0): its goal criteria and
// their results (C1), the commits its nodes made on a git tree (C7), and the
// protected paths an attempt changed (C6, event journal only). Events are read defensively: a newer apb may extend
// their shape.

function str(v: unknown): string | undefined {
  return typeof v === 'string' ? v : undefined
}

function num(v: unknown): number | undefined {
  return typeof v === 'number' && Number.isFinite(v) ? v : undefined
}

export interface CommitLine {
  node: string
  sha: string
  short: string
  subject: string
}

// One line per commit, in journal order (newest first within a node).
export function commitLines(commits: NodeCommits[] | undefined): CommitLine[] {
  const out: CommitLine[] = []
  for (const c of commits ?? []) {
    for (const a of c.commits) {
      out.push({ node: c.node, sha: a.sha, short: a.sha.slice(0, 12), subject: a.subject })
    }
  }
  return out
}

// Commits past the listed ones, over every node.
export function omittedCommits(commits: NodeCommits[] | undefined): number {
  return (commits ?? []).reduce((n, c) => n + c.omitted, 0)
}

export type GoalBadge = 'default' | 'secondary' | 'destructive' | 'outline'

// The badge variant of a criterion status: passed, failed or error, manual,
// pending (not checked yet).
export function goalBadge(status: string): GoalBadge {
  if (status === 'passed') return 'default'
  if (status === 'failed' || status === 'error') return 'destructive'
  if (status === 'manual') return 'outline'
  return 'secondary'
}

// The goal summary: `2 passed · 1 failed · 1 to confirm · enforced`,
// `not checked yet` before the run reached a finish node, or `no criteria to
// check` for a goal with a statement only.
export function goalSummary(g: RunGoal): string {
  // A goal with a statement only is never checked; do not show it as pending.
  if (g.criteria.length === 0) return 'no criteria to check'
  if (!g.checked) return `not checked yet · ${g.criteria.length} ${g.criteria.length === 1 ? 'criterion' : 'criteria'}`
  const parts = [`${g.passed} passed`]
  if (g.failed > 0) parts.push(`${g.failed} failed`)
  if (g.manual > 0) parts.push(`${g.manual} to confirm`)
  if (g.enforce) parts.push('enforced')
  return parts.join(' · ')
}

// The event journal note of the 0.23.0 outcome events.
export function outcomeEventNote(e: WfEvent): string | undefined {
  const r = e as unknown as Record<string, unknown>
  switch (e.type) {
    case 'artifacts_committed': {
      const commits = Array.isArray(r.commits) ? r.commits : []
      const n = commits.length + (num(r.omitted) ?? 0)
      const first = commits[0] as Record<string, unknown> | undefined
      const head = first ? `${(str(first.sha) ?? '').slice(0, 12)} ${str(first.subject) ?? ''}`.trim() : ''
      return `${n} ${n === 1 ? 'commit' : 'commits'}${head ? `: ${head}` : ''}`
    }
    case 'protected_paths_modified': {
      const changes = Array.isArray(r.changes) ? (r.changes as Record<string, unknown>[]) : []
      const list = changes.map((c) => `${str(c.change) ?? '?'} ${str(c.path) ?? ''}`.trim()).join(', ')
      const failed = Array.isArray(r.restore_failed) && r.restore_failed.length ? `; not restored: ${r.restore_failed.join(', ')}` : ''
      const kept = str(r.kept_copies) ? ` (copies kept in ${str(r.kept_copies)})` : ''
      return `protected: ${list || 'changed'}${failed}${kept}`
    }
    case 'goal_checked': {
      const idx = num(r.index)
      const label = `criterion ${idx !== undefined ? idx + 1 : '?'}: ${str(r.status) ?? ''}`
      const detail = str(r.detail)
      return detail ? `${label} (${detail})` : label
    }
    default:
      return undefined
  }
}
