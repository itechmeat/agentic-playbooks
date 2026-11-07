// The one line a run surface shows for the run's `run.previous_success`
// record: the earlier run's id and times, or the `when_none` window, or
// none; withheld keys are named. Matches the CLI's line in `apb runs <id>`.
import type { PreviousSuccessView } from './api.gen'

export function previousSuccessLine(ps: PreviousSuccessView): string {
  let line: string
  if (ps.run_id) {
    if (ps.started_at && ps.finished_at) {
      line = `${ps.run_id}, started ${ps.started_at}, finished ${ps.finished_at}`
    } else if (ps.started_at) {
      line = `${ps.run_id}, started ${ps.started_at}`
    } else {
      line = ps.run_id
    }
  } else if (ps.fallback) {
    const times: string[] = []
    if (ps.started_at) times.push(ps.started_at)
    if (ps.finished_at) times.push(`finished ${ps.finished_at}`)
    line = `none (window from when_none: ${times.join(', ')})`
  } else {
    line = 'none'
  }
  if (ps.withheld && ps.withheld.length > 0) {
    line += `, withheld: ${ps.withheld.join(', ')}`
  }
  return line
}

// The one line a run surface shows per node that renders untrusted text.
// Matches the CLI's `untrusted input:` lines in `apb runs <id>`.
import type { UntrustedNode } from './api.gen'

export function untrustedLine(u: UntrustedNode): string {
  const why = u.accepted ? `accepted: ${u.accepted}` : 'not accepted'
  return `node \`${u.node}\` renders ${u.sources.join(', ')}; ${why}`
}
