// Runs started from a run step, as the run page shows them: the run's
// starter and the runs a step of this run started in this project. The
// lines match the CLI's in `apb runs <id>`. A link leads to a run page of
// this project only: a starter in another workspace is shown as text.
import type { StartedBy, StartedRunView } from './api.gen'

/** `Started by run <id> (node <n>, attempt <k>)`, with `, not verified`
 * and `, in workspace <ws>` for a starter in another project. */
export function startedByLine(by: StartedBy, workspace: string): string {
  const parts: string[] = []
  if (by.node) parts.push(`node ${by.node}`)
  if (by.attempt != null) parts.push(`attempt ${by.attempt}`)
  let line = `Started by run ${by.run_id}`
  if (parts.length > 0) line += ` (${parts.join(', ')})`
  if (!by.verified) line += ', not verified'
  if (!sameWorkspace(by, workspace)) line += `, in workspace ${by.workspace_id}`
  return line
}

function sameWorkspace(by: StartedBy, workspace: string): boolean {
  return by.workspace_id == null || by.workspace_id === workspace
}

/** The run page of `runId` in `workspace`. */
export function runHref(workspace: string, runId: string): string {
  return `#/run/${encodeURIComponent(workspace)}/${encodeURIComponent(runId)}`
}

/** The starter's run page, or `null` for a starter in another workspace:
 * a link never opens a run of another project under this one. */
export function starterHref(by: StartedBy, workspace: string): string | null {
  return sameWorkspace(by, workspace) ? runHref(workspace, by.run_id) : null
}

/** The label of one started run: `<id> (<playbook>)`. */
export function startedRunLabel(r: StartedRunView): string {
  return `${r.run_id} (${r.playbook})`
}

/** The note of a list whose scan stopped at its bound. */
export const TRUNCATED_NOTE = 'more may exist beyond the newest 500 runs'
