import type { NodeCommits } from './api.gen'
import type { WfEvent } from './types'

// What a run produced beyond its node outputs (0.23.0): the commits its nodes
// made on a git tree (C7). Events are read defensively: a newer apb may extend
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
    default:
      return undefined
  }
}
