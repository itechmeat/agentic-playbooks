import type { CommittedArtifact, NodeCommits, RunGoal, RunInboxGap } from './api.gen'
import type { WfEvent } from './types'
import type { Tone } from './runverdict'

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
  // Set when the commit's `Apb-Run` trailer does not name this run.
  trailer?: string
}

// What a commit's `Apb-Run` trailer says against the run `runId`, in the
// words of `apb runs`: `no Apb-Run trailer`, `Apb-Run names <id>`, or
// nothing when it names this run or the trailers were not read (an older
// apb, an old git).
export function trailerNote(a: CommittedArtifact, runId: string | undefined): string | undefined {
  const ids = a.apb_run
  if (!Array.isArray(ids) || runId === undefined) return undefined
  if (ids.length === 0) return 'no Apb-Run trailer'
  if (ids.includes(runId)) return undefined
  return `Apb-Run names ${ids.join(', ')}`
}

// One line per commit, in journal order (newest first within a node),
// flagged when its trailer does not name `runId`.
export function commitLines(commits: NodeCommits[] | undefined, runId?: string): CommitLine[] {
  const out: CommitLine[] = []
  for (const c of commits ?? []) {
    for (const a of c.commits) {
      const line: CommitLine = { node: c.node, sha: a.sha, short: a.sha.slice(0, 12), subject: a.subject }
      const trailer = trailerNote(a, runId)
      if (trailer) line.trailer = trailer
      out.push(line)
    }
  }
  return out
}

// Commits past the listed ones, over every node.
export function omittedCommits(commits: NodeCommits[] | undefined): number {
  return (commits ?? []).reduce((n, c) => n + c.omitted, 0)
}

// The tag tone of a criterion status: passed, failed or error, and manual
// or pending (not checked yet) as neutral.
export function goalTone(status: string): Tone {
  if (status === 'passed') return 'pass'
  if (status === 'failed' || status === 'error') return 'fail'
  return 'neutral'
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

// The event journal note of the outcome events (0.23.0, plus inbox gaps,
// baseline snapshots and comparisons, and attempt notes).
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
    case 'inbox_gap': {
      const from = num(r.from)
      const through = num(r.through)
      if (from === undefined || through === undefined) return undefined
      return inboxGapLine({
        node: str(r.node_id) ?? '',
        connector: str(r.connector) ?? '',
        account: str(r.account) ?? '',
        from,
        through,
      })
    }
    case 'baseline_observed': {
      const head = `${str(r.check) ?? '?'} ${str(r.phase) ?? ''}: ${str(r.status) ?? ''}`
      const reason = str(r.reason)
      if (reason) return `${head} (${reason})`
      const obs = r.observations && typeof r.observations === 'object' ? Object.entries(r.observations) : []
      const values = obs.map(([id, o]) => `${id} ${observationText(o)}`).join(', ')
      return values ? `${head} (${values})` : head
    }
    case 'baseline_compared': {
      const change = `${observationText(r.before)} -> ${observationText(r.after)}`
      const rule = str(r.rule) ? `, ${str(r.rule)}` : ''
      const verdict = (str(r.verdict) ?? '').replace(/_/g, ' ')
      const worse = r.regressed === true ? ', worse than before' : ''
      const detail = str(r.detail) ? ` (${str(r.detail)})` : ''
      return `${str(r.check) ?? '?'}/${str(r.observation) ?? '?'}: ${change}${rule}: ${verdict}${worse}${detail}`
    }
    case 'attempt_notes': {
      const count = (v: unknown, one: string, many: string) => {
        const n = Array.isArray(v) ? v.length : 0
        return n ? `${n} ${n === 1 ? one : many}` : ''
      }
      const dropped = num(r.dropped) ?? 0
      const parts = [
        count(r.findings, 'finding', 'findings'),
        count(r.warnings, 'warning', 'warnings'),
        dropped ? `${dropped} dropped as malformed` : '',
      ].filter(Boolean)
      return parts.join(', ') || undefined
    }
    default:
      return undefined
  }
}

// One journaled baseline observation as people read it: `37`, `3 items`,
// `unknown`. Read defensively: the shape may come from a newer apb.
export function observationText(o: unknown): string {
  const obs = o as { state?: unknown; value?: { type?: unknown; value?: unknown } } | null | undefined
  if (!obs || obs.state !== 'value' || !obs.value) return 'unknown'
  const v = obs.value.value
  if (obs.value.type === 'list' && Array.isArray(v)) return `${v.length} ${v.length === 1 ? 'item' : 'items'}`
  if (typeof v === 'number' || typeof v === 'boolean') return String(v)
  if (typeof v === 'string') return `\`${v}\``
  return 'unknown'
}

// One warning line per inbox gap: unread events inbox retention dropped
// before they were read, so the run did not see its inbox whole. The
// consumer's cursor may predate the run, so the line does not blame the run.
export function inboxGapLine(g: RunInboxGap): string {
  const lost =
    g.from === g.through
      ? `unread event ${g.from} was dropped by inbox retention before it was read`
      : `unread events ${g.from}-${g.through} were dropped by inbox retention before they were read`
  return `${g.connector}/${g.account}: ${lost} (found by ${g.node})`
}
